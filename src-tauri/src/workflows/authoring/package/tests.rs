use std::{fs, path::Path};

use crate::{
    conversations::SessionScope,
    jobs::JobRole,
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode},
    workflows::{
        BuiltinNode, NodeConfiguration, NodeInstance, WorkflowGraph,
        role_templates::{ConversationTemplate, TemplateReviewPolicy},
        tests::approved_delivery_repository,
    },
};

use super::super::tests::finish_background_authoring_job;
use super::*;

fn graph() -> WorkflowGraph {
    WorkflowGraph {
        name: "Review pipeline".into(),
        nodes: vec![NodeInstance {
            id: "start".into(),
            kind: BuiltinNode::Start,
            configuration: NodeConfiguration::None,
        }],
        wires: vec![],
    }
}

fn role() -> PackageArtifactBody {
    PackageArtifactBody::Role {
        template: ConversationTemplate {
            name: "Reviewer".into(),
            role: JobRole::Agent,
            model: ModelSelection {
                provider: Provider::Codex,
                profile_id: "fixture".into(),
                model_id: "reviewer".into(),
            },
            effort: None,
            instructions: "Private review instructions".into(),
            tool_scope: SessionMode::ReadOnly,
            completion_contract: "Identify an exact affected file".into(),
            review_policy: TemplateReviewPolicy::HumanApproval,
            requirements: Default::default(),
        },
    }
}

fn member(key: &str, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn reference(key: &str) -> PackageReference {
    PackageReference::Draft { key: key.into() }
}

fn package(members: Vec<PackageMember>) -> WorkflowPackage {
    WorkflowPackage {
        name: "Reviewed artifact package".into(),
        members,
    }
}

#[tokio::test]
async fn opens_an_archived_function_for_repair_without_publishing_retired_nodes() -> Result<()> {
    use crate::workflows::ArchivedNode;
    use serde_json::json;

    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("archived-function.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let body = serde_json::from_value(json!({
        "kind": "subflow",
        "graph": {
            "name": "Archived Function",
            "nodes": [
                {"id": "start", "kind": "start", "configuration": {"kind": "none"}},
                {"id": "legacy", "kind": "join", "configuration": {"kind": "none"}},
                {"id": "end", "kind": "end", "configuration": {"kind": "end"}}
            ],
            "wires": [
                {"from": {"node": "start", "port": "start"}, "to": {"node": "legacy", "port": "branches"}},
                {"from": {"node": "legacy", "port": "joined"}, "to": {"node": "end", "port": "ready"}}
            ]
        }
    }))?;
    let version = PackageArtifactVersion {
        destination: PackageDestination::Project,
        kind: PackageArtifactKind::Subflow,
        id: "archived-function".into(),
        revision: "saved-revision".into(),
    };
    let legacy_definition = serde_json::from_value(json!({
        "kind": "join", "revision": 1, "title": "Join", "executor": "join_branches",
        "configuration": "none", "effect": "none",
        "inputs": [{"name": "branches", "value": {"kind": "control"}}],
        "outputs": [{"name": "joined", "value": {"kind": "control"}}]
    }))?;
    let artifact = PublishedPackageArtifact {
        version: version.clone(),
        name: "Archived Function".into(),
        body,
        references: BTreeMap::new(),
        definitions: BTreeMap::from([("legacy".into(), legacy_definition)]),
        layout: serde_json::from_value(json!({"legacy": {"x": 36, "y": 24}}))?,
        blockers: Vec::new(),
        prompt_expansion: None,
    };
    let mut editing = member("artifact", artifact.body.clone());
    editing.base = Some(version.clone());
    let scope = version.destination.key(&project);
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO workflow_package_artifact_revisions VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    scope,
                    version.kind.sql(),
                    version.id,
                    version.revision,
                    seal(&artifact)?
                ],
            )?;
            db.execute(
                "INSERT INTO workflow_package_artifact_heads VALUES (?1, ?2, ?3, ?4)",
                params![scope, version.kind.sql(), version.id, version.revision],
            )?;
            Ok(())
        })
        .await?;

    let draft = store
        .create_workflow_package(project.clone(), None, package(vec![editing.clone()]))
        .await?;
    assert!(draft.layouts["artifact"].contains_key("legacy"));
    assert!(
        store
            .publish_reviewed_workflow_package(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("retired node type")
    );
    let PackageArtifactBody::Subflow { graph, .. } = &mut editing.body else {
        unreachable!();
    };
    assert_eq!(
        graph.nodes[1].kind,
        BuiltinNode::Archived(ArchivedNode::Join)
    );
    graph.nodes.remove(1);
    graph.wires = crate::workflows::tests::simple_graph("Repaired Function").wires;
    let saved = store
        .save_workflow_package(
            project.clone(),
            draft.id,
            draft.revision,
            package(vec![editing]),
        )
        .await?;
    assert!(saved.layouts.is_empty());
    assert!(matches!(
        store
            .publish_reviewed_workflow_package(project, saved.id, saved.revision)
            .await?,
        WorkflowPackagePublishOutcome::Published { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn bundled_development_pipeline_publishes_through_the_native_compiler() -> Result<()> {
    use std::process::Command;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().context("Find the workspace")?;
    let encoded = Command::new("node")
        .current_dir(root)
        .args(["--input-type=module", "-e", r#"
            import { registerHooks } from "node:module";
            registerHooks({ resolve(specifier, context, nextResolve) {
              const target = specifier.startsWith("@/")
                ? new URL("./src/" + specifier.slice(2) + ".ts", import.meta.url).href
                : specifier;
              return nextResolve(target, context);
            } });
            const { developmentPipelinePackage } = await import("./src/development-pipeline.ts");
            process.stdout.write(JSON.stringify(developmentPipelinePackage({
              provider: "codex", profileId: "fixture", modelId: "reviewer",
            })));
        "#])
        .output()?;
    ensure!(encoded.status.success(), "Serialize the built-in template: {}", String::from_utf8_lossy(&encoded.stderr));
    let package: WorkflowPackage = serde_json::from_slice(&encoded.stdout)?;
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("development-template.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let draft = store.create_workflow_package(project.clone(), None, package).await?;
    let published = commit(&store, &project, &draft).await?;
    let version = &artifact(&published, "pipeline").version;
    let selected = crate::workflows::WorkflowRevision { id: version.id.clone(), revision: version.revision.clone() };
    store.execute(move |db| {
        let definition = crate::workflows::load_definition(db, &project, &selected)?
            .context("The bundled pipeline was not persisted")?;
        assert!(definition.unsupported_node().is_none());
        assert!(definition.nodes.iter().any(crate::workflows::write_scope::writer));
        assert!(definition.nodes.len() <= 4096);
        Ok(())
    }).await?;
    Ok(())
}

#[tokio::test]
async fn human_approval_releases_a_typed_agent_into_its_managed_worktree() -> Result<()> {
    use crate::{
        jobs::{AdmissionEnvironment, JobPermission, JobRule, JobWorkspace},
        workflows::{
            ArtifactValue, WorkflowLaunchRequest, decision_nodes::WorkflowDecisionSubmission,
            generic_agent::tests::complete_response,
        },
    };
    use serde_json::json;
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("native-writer.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    store.set_project_job_rules(project.clone(), Vec::new(), vec![JobRule {
        role: JobRole::Agent,
        operation: SessionMode::ApprovedWrite,
        workspace: JobWorkspace::Project,
        permission: JobPermission::Allow,
        requirements: Default::default(),
    }]).await?;

    let mut writer = role();
    let PackageArtifactBody::Role { template } = &mut writer else { unreachable!() };
    template.tool_scope = SessionMode::ApprovedWrite;
    template.review_policy = TemplateReviewPolicy::Independent;
    let input = serde_json::from_value(json!({"kind":"schema","fields":[
        {"name":"task_key","type":"string","required":true},
        {"name":"tasks","type":"array","required":true,"items":{"type":"object","fields":[
            {"name":"key","type":"string","required":true},{"name":"title","type":"string","required":true}
        ]}}
    ]}))?;
    let output = serde_json::from_value(json!({"kind":"schema","fields":[
        {"name":"reply","type":"string","required":true}
    ]}))?;
    let mut prompt = member("writer", serde_json::from_value(json!({
        "kind":"prompt","prompt":{"text":"Implement {{input.task_key}}.","executor":"agent","schemaContract":true,"nodePrefab":true}
    }))?);
    prompt.references = BTreeMap::from([
        ("role".into(), reference("role")), ("input_schema".into(), reference("input")),
        ("output_schema".into(), reference("output")),
    ]);
    let mut pipeline = member("pipeline", serde_json::from_value(json!({
        "kind":"pipeline","graph":{"name":"Approved writer smoke test","nodes":[
            {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
            {"id":"approve","kind":"human","configuration":{"kind":"human","interaction":"approval","message":"Authorize writing the task worktree?","fields":[
                {"id":"authorizeWrites","label":"Authorize worktree writes","required":true,"input":{"kind":"boolean"}}
            ]}},
            {"id":"write","kind":"subflow","configuration":{"kind":"none"}},
            {"id":"end","kind":"end","configuration":{"kind":"end"}}
        ],"wires":[
            {"from":{"node":"start","port":"start"},"to":{"node":"approve","port":"ready"}},
            {"from":{"node":"start","port":"inputs"},"to":{"node":"approve","port":"input"}},
            {"from":{"node":"approve","port":"approved"},"to":{"node":"write","port":"ready"}},
            {"from":{"node":"approve","port":"approved_result"},"to":{"node":"write","port":"input"}},
            {"from":{"node":"write","port":"completed"},"to":{"node":"end","port":"ready"}},
            {"from":{"node":"write","port":"result"},"to":{"node":"end","port":"result"}}
        ]}
    }))?);
    pipeline.references = BTreeMap::from([
        ("start:schema".into(), reference("input")), ("write".into(), reference("writer")),
        ("end:schema".into(), reference("output")),
    ]);
    let draft = store.create_workflow_package(project.clone(), None, package(vec![
        member("role", writer), member("input", input), member("output", output), prompt, pipeline,
    ])).await?;
    let publication = commit(&store, &project, &draft).await?;
    let version = &artifact(&publication, "pipeline").version;
    let request: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project,"checkoutId":repo.checkout_id,"requestId":"native-writer",
        "workflow":{"id":version.id,"revision":version.revision},
        "inputs":{"task_key":"TASK-1","tasks":[{"key":"TASK-1","title":"Add an example"}]}
    }))?;
    let run = store.launch_workflow_request(request).await?;
    assert!(store.next_workflow_agent().await?.is_none());
    let inspection = store.inspect_workflow_execution(project.clone(), run.id.clone()).await?
        .context("The workflow was not launched")?;
    let decision = inspection.pending_decisions.first().context("Expected write approval")?;
    let input: WorkflowDecisionSubmission = serde_json::from_value(json!({
        "projectId":project,"runId":run.id,"requestId":decision.request_id,
        "decision":{"kind":"human_approval","approved":true,"values":{"authorizeWrites":true}}
    }))?;
    store.resolve_workflow_stage_decision(input).await?;
    let stage = store.next_workflow_agent().await?.context("The approved writer is not ready")?;
    let first = stage.fixture_submission(&store).await?;
    let second = stage.fixture_submission(&store).await?;
    assert_eq!(first.dispatch.release_id, second.dispatch.release_id);
    let release = first.dispatch.release_id.clone().context("Missing approved release")?;
    assert_ne!(first.request.checkout_id.as_deref(), Some(repo.checkout_id.as_str()));
    assert!(first.request.task_id.is_some());
    assert_eq!(first.scope.mode, SessionMode::ApprovedWrite);
    let worktree = first.scope.checkout.clone();
    let mut invalid = stage.fixture_submission(&store).await?;
    invalid.request.task_id = Some("other-task".into());
    assert!(store.submit_workflow_agent(invalid).await.is_err());
    stage.enqueue(&store, first).await?;
    let attempt = store.claim_next_job(AdmissionEnvironment {
        runner: "fixture-runner".into(), bridge_available: false,
    }).await?.context("The write job was not admitted")?;
    assert_eq!(attempt.scope.checkout, worktree);
    assert!(attempt.input[0].text.contains("Implement TASK-1."));
    fs::write(worktree.join("first.txt"), "approved candidate\n")?;
    assert_eq!(fs::read_to_string(Path::new(&repo.checkout_root).join("first.txt"))?, "original first\n");
    complete_response(&store, &attempt, r#"{"reply":"changed first.txt"}"#, None).await?;
    let settled = store.inspect_workflow_execution(project.clone(), run.id.clone()).await?
        .context("The workflow execution disappeared")?;
    assert!(settled.outputs.iter().any(|output| {
        output.current && output.artifact.node_instance_id == "end"
            && matches!(&output.artifact.value, ArtifactValue::JsonObject(value)
                if value.get("reply") == Some(&json!("changed first.txt")))
    }));
    let approved = settled.outputs.iter().find(|output| output.artifact.node_instance_id == "approve"
        && output.artifact.port == "approved_result").context("Missing approved input")?;
    store.revoke_job_pipeline_release(project.clone(), release).await?;
    let error = store.prepare_human_write_scope(&project, &run.id, "write::execute", Some(&approved.artifact))
        .await.unwrap_err();
    assert!(error.to_string().contains("revoked"));
    Ok(())
}

#[tokio::test]
async fn pins_typed_prompt_ports_and_isolates_prefab_overrides() -> Result<()> {
    use crate::{
        conversations::ConversationPipelineChoice,
        jobs::AdmissionEnvironment,
        workflows::{
            ArtifactValue, WorkflowLaunchRequest, control::ControlStageState,
            generic_agent::tests::complete_response,
        },
    };
    use serde_json::json;
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("typed-prompts.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    let mut saved_role = role();
    let PackageArtifactBody::Role { template } = &mut saved_role else {
        unreachable!();
    };
    template.review_policy = TemplateReviewPolicy::Independent;
    let contract = serde_json::from_value(json!({
        "kind":"schema", "fields":[{"name":"reply","required":true,"type":"string"},
            {"name":"message","required":false,"type":"string"}]
    }))?;
    let mut prompt = member(
        "prefab",
        serde_json::from_value(json!({
            "kind":"prompt", "prompt":{"text":"Respond to {{input.reply}}.", "executor":"agent", "schemaContract":true, "nodePrefab":true}
        }))?,
    );
    prompt.references = BTreeMap::from([
        ("role".into(), reference("role")),
        ("input_schema".into(), reference("text")),
        ("output_schema".into(), reference("text")),
    ]);
    let mut root = member(
        "pipeline",
        serde_json::from_value(json!({
            "kind":"pipeline", "graph":{
                "name":"Typed Prompt pipeline",
                "nodes":[
                    {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","conversation":true,"fields":[]}},
                    {"id":"first","kind":"subflow","configuration":{"kind":"prompt_overrides","prompt":"First: {{input.reply}}"}},
                    {"id":"second","kind":"subflow","configuration":{"kind":"prompt_overrides","roleOverrides":{"instructions":"Second reviewer."}}},
                    {"id":"end","kind":"end","configuration":{"kind":"end"}}
                ],
                "wires":[
                    {"from":{"node":"start","port":"start"},"to":{"node":"first","port":"ready"}},
                    {"from":{"node":"start","port":"inputs"},"to":{"node":"first","port":"input"}},
                    {"from":{"node":"first","port":"completed"},"to":{"node":"second","port":"ready"}},
                    {"from":{"node":"first","port":"result"},"to":{"node":"second","port":"input"}},
                    {"from":{"node":"second","port":"completed"},"to":{"node":"end","port":"ready"}},
                    {"from":{"node":"second","port":"result"},"to":{"node":"end","port":"result"}}
                ]
            }
        }))?,
    );
    root.references = BTreeMap::from([
        ("start:schema".into(), reference("text")),
        ("first".into(), reference("prefab")),
        ("second".into(), reference("prefab")),
        ("end:schema".into(), reference("text")),
    ]);
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![
                member("role", saved_role),
                member("text", contract),
                prompt,
                root,
            ]),
        )
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let catalog = serde_json::to_value(store.workflow_package_catalog(project.clone()).await?)?;
    assert!(
        catalog
            .as_array()
            .context("Expected library catalog")?
            .iter()
            .any(|item| {
                item["nodePrefab"] == true
                    && item["version"]["id"] == artifact(&published, "prefab").version.id
            })
    );
    let version = artifact(&published, "pipeline").version.clone();
    let chat = store
        .create_conversation_with_pipeline(
            repo.checkout_id.clone(),
            "Typed Prompt chat".into(),
            ConversationPipelineChoice::Published {
                workflow: crate::workflows::WorkflowRevision {
                    id: version.id.clone(),
                    revision: version.revision.clone(),
                },
            },
        )
        .await?;
    let schema = serde_json::to_value(store.conversation_pipeline_schema(chat.id).await?)?;
    let agents = schema["agents"]
        .as_array()
        .context("Expected typed agents")?;
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().all(|agent| agent["actionEligible"] == false));
    let launch: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project, "checkoutId":repo.checkout_id, "requestId":"typed-prompt-test",
        "workflow":{"id":version.id,"revision":version.revision},"inputs":{"reply":"Hello"}
    }))?;
    let run = store.launch_workflow_request(launch).await?;
    let inspection = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("Missing pipeline execution")?;
    let agent = |id| -> Result<_> {
        let node = inspection
            .definition
            .nodes
            .iter()
            .find(|node| node.instance.id == id)
            .context("Missing expanded Prompt")?;
        let NodeConfiguration::AgentRole { requirements, .. } = &node.instance.configuration else {
            anyhow::bail!("Expected a typed Agent");
        };
        requirements
            .structured
            .as_ref()
            .context("Missing typed Agent configuration")
    };
    assert_eq!(agent("first::execute")?.prompt, "First: {{input.reply}}");
    assert_eq!(
        agent("second::execute")?.prompt,
        "Respond to {{input.reply}}."
    );
    assert_eq!(
        agent("second::execute")?.effective_role.instructions,
        "Second reviewer."
    );
    assert_eq!(
        agent("first::execute")?.effective_role.instructions,
        "Private review instructions"
    );
    assert!(inspection.outputs.iter().any(|output| matches!(
        &output.artifact.value, ArtifactValue::JsonObject(value) if value.get("reply") == Some(&json!("Hello"))
    )));
    let dispatch_agent = |store: Store, id: &'static str| async move {
        let stage = store
            .next_workflow_agent()
            .await?
            .context("Missing ready typed Agent")?;
        assert_eq!(stage.fixture_identity().1, id);
        let submission = stage.fixture_submission(&store).await?;
        stage.enqueue(&store, submission).await?;
        store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-runner".into(),
                bridge_available: false,
            })
            .await?
            .context("Typed Agent did not pass the ordinary workflow admission")
    };
    let first = dispatch_agent(store.clone(), "first::execute").await?;
    assert!(first.input[0].text.contains("First: Hello"));
    assert_eq!(
        store
            .workflow_output_schema_for_attempt(first.run.id.clone())
            .await?,
        Some(json!({
            "type":"object","properties":{"message":{"type":["string","null"]},"reply":{"type":"string"}},
            "required":["message","reply"],"additionalProperties":false
        }))
    );
    complete_response(&store, &first, r#"{"reply":"first answer"}"#, None).await?;
    let second = dispatch_agent(store.clone(), "second::execute").await?;
    assert!(second.input[0].text.contains("Respond to first answer."));
    assert!(second.input[0].text.contains("Second reviewer."));
    complete_response(&store, &second, r#"{"reply":"second answer"}"#, None).await?;
    let completed = store
        .inspect_workflow_execution(project.clone(), run.id)
        .await?
        .context("Missing completed pipeline execution")?;
    assert!(completed.outputs.iter().any(|output| {
        output.artifact.node_instance_id == "end"
            && matches!(&output.artifact.value, ArtifactValue::JsonObject(value) if value["reply"] == "second answer")
    }));
    let invalid_launch: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project, "checkoutId":repo.checkout_id, "requestId":"invalid-typed-prompt-test",
        "workflow":{"id":version.id,"revision":version.revision},"inputs":{"reply":"Hello"}
    }))?;
    let invalid_run = store.launch_workflow_request(invalid_launch).await?;
    let invalid = dispatch_agent(store.clone(), "first::execute").await?;
    complete_response(&store, &invalid, r#"{"reply":42}"#, None).await?;
    let failure = store
        .inspect_workflow_execution(project, invalid_run.id)
        .await?
        .context("Missing failed pipeline execution")?;
    assert!(failure.control.iter().any(|receipt| {
        receipt.node_id == "first::execute"
            && receipt.state == ControlStageState::Failed
            && receipt.selected_ports == ["failed"]
    }));
    assert!(
        failure
            .outputs
            .iter()
            .all(|output| output.artifact.node_instance_id != "first::execute")
    );
    assert!(store.next_workflow_agent().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn duplicates_a_published_workflow_as_an_independent_draft_with_its_pins_and_layout()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("store.sqlite"))?;
    let project = approved_delivery_repository(&store, temporary.path())
        .await?
        .project_id;
    let package = package(vec![member(
        "pipeline",
        PackageArtifactBody::Pipeline { graph: graph() },
    )]);
    let original_draft = store
        .create_workflow_package(project.clone(), None, package.clone())
        .await?;
    let position = serde_json::from_value(serde_json::json!({ "x": 73.0, "y": 14.0 }))?;
    let layout = BTreeMap::from([("start".into(), position)]);
    let original_draft = store
        .save_workflow_package_canvas(
            project.clone(),
            original_draft.id,
            original_draft.revision,
            package,
            BTreeMap::from([(
                "pipeline".into(),
                WorkflowCanvasEdit {
                    layout: layout.clone(),
                    update_definitions: BTreeMap::new(),
                    definition_copies: BTreeMap::new(),
                },
            )]),
        )
        .await?;
    let publication = commit(&store, &project, &original_draft).await?;
    let saved = artifact(&publication, "pipeline");
    let version = saved.version.clone();
    let copy = store
        .duplicate_workflow_template_draft(project.clone(), version.clone())
        .await?;
    assert_ne!(copy.id, original_draft.id);
    assert!(copy.publication.is_none());
    assert!(copy.package.members[0].base.is_none());
    assert_eq!(copy.layouts["pipeline"], layout);
    let PackageArtifactBody::Pipeline { graph } = &copy.package.members[0].body else {
        anyhow::bail!("Expected a pipeline draft");
    };
    assert_eq!(graph.name, "pipeline copy");
    let key = copy.id.clone();
    let revision = copy.revision.clone();
    let pinned = store
        .execute(move |db| Ok(read_document(db, &key, &revision)?.1.definitions))
        .await?;
    assert_eq!(pinned["pipeline"], saved.definitions);
    assert!(
        store
            .duplicate_workflow_template_draft("foreign-project".into(), version)
            .await
            .is_err()
    );
    assert!(store.list_workflow_runs(project).await?.is_empty());
    Ok(())
}

async fn commit(
    store: &Store,
    project: &str,
    draft: &WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id.clone(), draft.revision.clone())
        .await?
    else {
        anyhow::bail!("Expected a package publication")
    };
    Ok(publication)
}

fn artifact<'a>(
    publication: &'a WorkflowPackagePublication,
    key: &str,
) -> &'a PublishedPackageArtifact {
    &publication
        .members
        .iter()
        .find(|member| member.key == key)
        .unwrap()
        .artifact
}

async fn project_job_count(store: &Store, project: &str) -> Result<i64> {
    let project = project.to_owned();
    store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT count(*) FROM jobs WHERE project_id = ?1",
                [project],
                |row| row.get(0),
            )?)
        })
        .await
}

#[tokio::test]
async fn legacy_workflow_package_authoring_pins_the_qualified_source_run_and_reviewed_revision()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("protected.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let conversation = store
        .create_conversation(repo.checkout_id.clone(), "Package requests".into())
        .await?;
    let root = Path::new(&repo.checkout_root);
    let executable = root.join("codex.exe");
    fs::write(&executable, b"fixture")?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "fixture".into(),
        account_binding: None,
        checkout: root.into(),
        executable,
        cli_version: "fixture".into(),
        runner: "fixture-runner".into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("fixture-model".into()),
        effort: None,
    };
    let request = "Create a reviewer role and a matching pipeline";
    let draft = store
        .start_workflow_package_authoring(
            project.clone(),
            conversation.id.clone(),
            repo.checkout_id,
            scope.clone(),
            None,
            request.into(),
        )
        .await?;
    let request_id = draft.request.as_ref().unwrap().id.clone();
    store
        .execute({
            let request_id = request_id.clone();
            move |db| {
                db.execute(
                    "UPDATE workflow_package_requests SET structured_mcp = 0 WHERE id = ?1",
                    [request_id],
                )?;
                Ok(())
            }
        })
        .await?;
    assert!(draft.request.as_ref().unwrap().pending());
    assert_eq!(
        store
            .workflow_packages(project.clone(), Some(conversation.id.clone()))
            .await?
            .len(),
        1
    );
    assert!(
        store
            .workflow_packages(project.clone(), Some("another-conversation".into()))
            .await?
            .is_empty()
    );
    assert!(
        store
            .publish_reviewed_workflow_package(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone()
            )
            .await
            .is_err()
    );
    let output = package(vec![
        member("review", role()),
        member("pipeline", PackageArtifactBody::Pipeline { graph: graph() }),
    ]);
    let run = finish_background_authoring_job(
        &store,
        &conversation.id,
        &scope,
        serde_json::to_string(&output)?,
        &[request, &request_id],
    )
    .await?;
    assert!(
        store
            .save_workflow_package(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone(),
                package(vec![member("stale", role())])
            )
            .await
            .is_err()
    );
    let reviewed = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_ne!(reviewed.revision, draft.revision);
    assert_eq!(reviewed.package, output);
    assert_eq!(reviewed.request.as_ref().unwrap().state, "saved");
    assert!(
        store
            .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
            .await
            .is_err()
    );
    let publication = commit(&store, &project, &reviewed).await?;
    assert_eq!(publication.originating_request_id, Some(request_id));
    assert_eq!(publication.originating_run_id, Some(run));
    assert_eq!(publication.reviewed_revision, reviewed.revision);
    Ok(())
}

#[tokio::test]
async fn visual_canvas_positions_are_protected_revision_edits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("builder.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![member(
                "pipeline",
                PackageArtifactBody::Pipeline { graph: graph() },
            )]),
        )
        .await?;
    let canvas = || {
        serde_json::from_value(serde_json::json!({
            "pipeline": { "layout": {"start": {"x": 114.0, "y": 72.0}}, "updateDefinitions": {} }
        }))
    };
    let saved = store
        .save_workflow_package_canvas(
            project.clone(),
            draft.id.clone(),
            draft.revision.clone(),
            draft.package.clone(),
            canvas()?,
        )
        .await?;
    assert_ne!(saved.revision, draft.revision);
    assert_eq!(saved.package, draft.package);
    assert!(
        store
            .save_workflow_package_canvas(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone(),
                draft.package,
                canvas()?
            )
            .await
            .is_err()
    );
    drop(store);
    let reopened = Store::open(database)?
        .workflow_package(project, saved.id.clone())
        .await?;
    assert_eq!(reopened.revision, saved.revision);
    assert_eq!(
        serde_json::to_value(&reopened.layouts)?,
        serde_json::json!({"pipeline": {"start": {"x": 114.0, "y": 72.0}}})
    );
    Ok(())
}

#[tokio::test]
async fn publishes_a_standalone_function_with_typed_ports_and_runs_it_from_a_pipeline() -> Result<()>
{
    use crate::workflows::{ArtifactValue, WorkflowLaunchRequest};
    use serde_json::json;

    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("standalone-function.sqlite"))?;
    let checkout = approved_delivery_repository(&store, temp.path()).await?;
    let project = checkout.project_id;
    let schema = member(
        "message-schema",
        serde_json::from_value(json!({
            "kind":"schema", "fields":[{"name":"message","required":true,"type":"string"}]
        }))?,
    );
    let draft = store
        .create_workflow_package(project.clone(), None, package(vec![schema]))
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let schema = PackageReference::Published {
        version: artifact(&published, "message-schema").version.clone(),
    };

    let mut function = member(
        "function",
        serde_json::from_value(json!({
            "kind":"subflow", "graph":{
                "name":"Forward message",
                "nodes":[
                    {"id":"start","kind":"start","configuration":{"kind":"subflow_entry","outputs":[
                        {"name":"ready","value":{"kind":"control"}},
                        {"name":"inputs","value":{"kind":"data","artifact":"json_object"}}
                    ]}},
                    {"id":"return","kind":"end","configuration":{"kind":"end"}}
                ],
                "wires":[
                    {"from":{"node":"start","port":"ready"},"to":{"node":"return","port":"ready"}},
                    {"from":{"node":"start","port":"inputs"},"to":{"node":"return","port":"result"}}
                ]
            },
            "inputs":{"ready":{"node":"start","port":"ready"},"input":{"node":"start","port":"inputs"}},
            "outputs":{"completed":{"node":"return","port":"completed"},"result":{"node":"return","port":"result"}}
        }))?,
    );
    function.name = "Forward message".into();
    function.references = BTreeMap::from([
        ("start:schema".into(), schema.clone()),
        ("return:schema".into(), schema.clone()),
    ]);
    let draft = store
        .create_workflow_package(project.clone(), None, package(vec![function]))
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let version = artifact(&published, "function").version.clone();
    let description = store
        .describe_workflow_reusable_node(project.clone(), version.clone())
        .await?;
    assert_eq!(
        description.definition.input_schemas["input"]["properties"]["message"]["type"],
        "string"
    );
    assert_eq!(
        description.definition.output_schemas["result"],
        description.definition.input_schemas["input"]
    );

    let mut pipeline = member(
        "pipeline",
        serde_json::from_value(json!({
            "kind":"pipeline", "graph":{
                "name":"Call the Function",
                "nodes":[
                    {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
                    {"id":"forward","kind":"subflow","configuration":{"kind":"none"}},
                    {"id":"end","kind":"end","configuration":{"kind":"end"}}
                ],
                "wires":[
                    {"from":{"node":"start","port":"start"},"to":{"node":"forward","port":"ready"}},
                    {"from":{"node":"start","port":"inputs"},"to":{"node":"forward","port":"input"}},
                    {"from":{"node":"forward","port":"completed"},"to":{"node":"end","port":"ready"}},
                    {"from":{"node":"forward","port":"result"},"to":{"node":"end","port":"result"}}
                ]
            }
        }))?,
    );
    pipeline.references = BTreeMap::from([
        ("start:schema".into(), schema.clone()),
        ("forward".into(), PackageReference::Published { version }),
        ("end:schema".into(), schema),
    ]);
    let draft = store
        .create_workflow_package(project.clone(), None, package(vec![pipeline]))
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let pipeline = &artifact(&published, "pipeline").version;
    let input = json!({"message": "Preserve the caller input"});
    let request: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project, "checkoutId":checkout.checkout_id, "requestId":"standalone-function",
        "workflow":{"id":pipeline.id,"revision":pipeline.revision}, "inputs":input
    }))?;
    let run = store.launch_workflow_request(request).await?;
    let snapshot = store
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("Missing Function pipeline run")?;
    for id in ["forward::return", "end"] {
        assert!(snapshot.outputs.iter().any(|output| output.current && output.artifact.node_instance_id == id
            && matches!(&output.artifact.value, ArtifactValue::JsonObject(value) if json!(value) == input)));
    }
    Ok(())
}
