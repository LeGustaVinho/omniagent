use std::{fs, path::Path};

use crate::{
    conversations::SessionScope,
    jobs::JobRole,
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode},
    workflows::{
        BuiltinNode, CommandConfiguration, CommandOutputFormat, DeliveryRoles,
        ManualWorkflowLaunch, NodeConfiguration, NodeInstance, WorkflowGraph, WorkflowLaunchSource,
        builtin_delivery_graph,
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

#[tokio::test]
async fn reviewed_workflow_package_is_atomic_and_pins_related_artifacts_after_restart() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("protected.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let mut pipeline = member(
        "pipeline",
        PackageArtifactBody::Pipeline {
            graph: builtin_delivery_graph(DeliveryRoles::default()),
        },
    );
    pipeline
        .references
        .insert("plan".into(), reference("preset"));
    let mut preset = member(
        "preset",
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::Plan,
            configuration: NodeConfiguration::AgentRole {
                role: "Pinned planner".into(),
                model: None,
                requirements: Default::default(),
            },
            editable_fields: vec![],
        },
    );
    preset.destination = PackageDestination::Library;
    let role = member("role", role());
    let mut subflow = member(
        "subflow",
        PackageArtifactBody::Subflow {
            graph: graph(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        },
    );
    subflow.destination = PackageDestination::Library;
    subflow.references.insert(
        "local-helper".into(),
        PackageReference::Parameter {
            name: "Local helper".into(),
            kind: PackageArtifactKind::Command,
        },
    );
    let mut command = member(
        "command",
        PackageArtifactBody::Command {
            binding: PackageCommandBinding::Required {
                name: "PowerShell".into(),
            },
            arguments: vec![],
            input: Default::default(),
            output: CommandOutputFormat::Text,
        },
    );
    command.destination = PackageDestination::Library;
    let mcp = member(
        "mcp",
        PackageArtifactBody::McpTool {
            binding: "Repo tools".into(),
            tool: "inspect".into(),
            input_schema: serde_json::from_value(
                serde_json::json!({"type": "object", "properties": {}}),
            )?,
            output_schema: None,
        },
    );
    let initial = package(vec![
        pipeline.clone(),
        role.clone(),
        preset.clone(),
        subflow.clone(),
        command,
        mcp,
    ]);
    let draft = store
        .create_workflow_package(project.clone(), None, initial.clone())
        .await?;
    let receipt = commit(&store, &project, &draft).await?;
    assert_eq!(receipt.reviewed_revision, draft.revision);
    assert_eq!(receipt.members.len(), 6);
    assert_eq!(
        artifact(&receipt, "pipeline").references["plan"],
        ResolvedPackageReference::Published {
            version: artifact(&receipt, "preset").version.clone()
        }
    );
    assert!(
        artifact(&receipt, "command")
            .blockers
            .iter()
            .any(|blocker| blocker.contains("PowerShell"))
    );
    assert!(
        artifact(&receipt, "mcp")
            .blockers
            .iter()
            .any(|blocker| blocker.contains("Repo tools"))
    );
    assert!(
        artifact(&receipt, "subflow")
            .blockers
            .iter()
            .any(|blocker| blocker.contains("Local helper"))
    );
    let version = artifact(&receipt, "pipeline").version.clone();
    let template = artifact(&receipt, "role").version.clone();
    let role_read = store
        .conversation_template_revision(
            project.clone(),
            template.id.clone(),
            template.revision.clone(),
        )
        .await?;
    assert_eq!(
        role_read.revision.template.instructions,
        "Private review instructions"
    );
    let workflow = WorkflowRevision {
        id: version.id,
        revision: version.revision,
    };
    assert!(store.list_workflow_runs(project.clone()).await?.is_empty());
    let definition = store
        .workflow_definition(project.clone(), workflow.clone())
        .await?
        .unwrap();
    assert!(definition.nodes.iter().any(|node| node.instance.id == "plan"
        && matches!(&node.instance.configuration, NodeConfiguration::AgentRole { role, .. } if role == "Pinned planner")));
    store
        .execute({
            let scope = project.clone();
            let workflow = workflow.clone();
            move |db| ensure_package_runnable(db, &scope, &workflow)
        })
        .await?;
    let note = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let source = WorkflowLaunchSource {
        checkout_id: repo.checkout_id.clone(),
        revision_id: note.revision.id,
    };
    let started = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow,
            source: source.clone(),
            request_id: new_id(),
            work_id: None,
        })
        .await?;
    assert_eq!(store.list_workflow_runs(project.clone()).await?, [started]);

    // Resolving the second member must roll back the role revision created for
    // the first member when the library edge is outside its destination.
    subflow
        .references
        .insert("project-role".into(), reference("role"));
    let invalid = store
        .create_workflow_package(project.clone(), None, package(vec![role, subflow]))
        .await?;
    assert!(commit(&store, &project, &invalid).await.is_err());
    let mut library_graph = graph();
    library_graph.nodes.push(NodeInstance {
        id: "requires-local".into(),
        kind: BuiltinNode::Command,
        configuration: NodeConfiguration::Command(CommandConfiguration {
            registration_id: "only-in-this-project".into(),
            registration_revision: "a".repeat(64),
            arguments: Vec::new(),
            input: Default::default(),
            output: CommandOutputFormat::Text,
        }),
    });
    let mut local_in_library = member(
        "unsafe-local",
        PackageArtifactBody::Subflow {
            graph: library_graph,
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
        },
    );
    local_in_library.destination = PackageDestination::Library;
    let unsafe_draft = store
        .create_workflow_package(project.clone(), None, package(vec![local_in_library]))
        .await?;
    assert!(
        commit(&store, &project, &unsafe_draft)
            .await
            .unwrap_err()
            .to_string()
            .contains("requires a named local binding")
    );
    assert_eq!(
        store
            .list_conversation_templates(project.clone())
            .await?
            .len(),
        1
    );
    assert!(
        store
            .workflow_package(project.clone(), invalid.id)
            .await?
            .publication
            .is_none()
    );

    let mut stale = initial.clone();
    let role_member = stale
        .members
        .iter_mut()
        .find(|member| member.key == "role")
        .unwrap();
    role_member.base = Some(template.clone());
    stale.members.truncate(2);
    let stale = store
        .create_workflow_package(project.clone(), None, stale)
        .await?;
    let edited_role = store
        .save_conversation_template(
            project.clone(),
            Some(crate::workflows::role_templates::TemplateBase {
                template_id: template.id.clone(),
                expected_revision: template.revision.clone(),
            }),
            ConversationTemplate {
                name: "Changed independently".into(),
                ..role_read.revision.template
            },
        )
        .await?;
    assert!(matches!(
        edited_role,
        crate::workflows::role_templates::SaveConversationTemplate::Saved { .. }
    ));
    let WorkflowPackagePublishOutcome::StaleBase { member, current } = store
        .publish_reviewed_workflow_package(
            project.clone(),
            stale.id.clone(),
            stale.revision.clone(),
        )
        .await?
    else {
        anyhow::bail!("Expected stale role conflict")
    };
    assert_eq!(member, "role");
    assert_eq!(current.name, "Changed independently");
    assert_eq!(
        store
            .workflow_package(project.clone(), stale.id)
            .await?
            .revision,
        stale.revision
    );
    assert_eq!(
        store.workflow_authoring_bases(project.clone()).await?.len(),
        1
    );

    let mut blocked = pipeline;
    blocked.references.insert(
        "plan".into(),
        PackageReference::Published {
            version: artifact(&receipt, "subflow").version.clone(),
        },
    );
    let blocked = store
        .create_workflow_package(project.clone(), None, package(vec![blocked]))
        .await?;
    let blocked_receipt = commit(&store, &project, &blocked).await?;
    let blocked_version = artifact(&blocked_receipt, "pipeline").version.clone();
    let blocked_workflow = WorkflowRevision {
        id: blocked_version.id,
        revision: blocked_version.revision,
    };
    assert!(
        store
            .execute({
                let scope = project.clone();
                let workflow = blocked_workflow.clone();
                move |db| ensure_package_runnable(db, &scope, &workflow)
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("Local helper")
    );
    let queued_before = project_job_count(&store, &project).await?;
    assert!(
        store
            .launch_workflow_manual(ManualWorkflowLaunch {
                project_id: project.clone(),
                workflow: blocked_workflow,
                source,
                request_id: new_id(),
                work_id: None,
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("Local helper")
    );
    assert_eq!(store.list_workflow_runs(project.clone()).await?.len(), 1);
    assert_eq!(project_job_count(&store, &project).await?, queued_before);
    drop(store);

    let store = Store::open(database)?;
    assert_eq!(
        store
            .workflow_package(project.clone(), draft.id.clone())
            .await?
            .publication,
        Some(receipt.clone())
    );
    assert_eq!(commit(&store, &project, &draft).await?, receipt);
    assert!(
        store
            .published_workflow_package_artifact("other-project".into(), template)
            .await?
            .is_none()
    );
    assert_eq!(
        store
            .published_workflow_package_artifact(
                project,
                artifact(&receipt, "preset").version.clone()
            )
            .await?
            .as_ref(),
        Some(artifact(&receipt, "preset"))
    );
    Ok(())
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
async fn visual_data_pipeline_simulates_and_runs_the_same_typed_result() -> Result<()> {
    use crate::workflows::{ArtifactValue, WorkflowLaunchRequest};
    use serde_json::json;

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("visual-result.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let graph: WorkflowGraph = serde_json::from_value(json!({
        "name": "Summarize work",
        "nodes": [
            {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[
                {"name":"label","valueType":"text","required":true}
            ]}},
            {"id":"text","kind":"build_text","configuration":{"kind":"build_text","template":"Status for {{label}}"}},
            {"id":"result","kind":"summarize_result","configuration":{"kind":"none"}}
        ],
        "wires":[
            {"from":{"node":"start","port":"start"},"to":{"node":"text","port":"ready"}},
            {"from":{"node":"start","port":"inputs"},"to":{"node":"text","port":"values"}},
            {"from":{"node":"text","port":"completed"},"to":{"node":"result","port":"ready"}},
            {"from":{"node":"text","port":"text"},"to":{"node":"result","port":"summary"}}
        ]
    }))?;
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![member(
                "pipeline",
                PackageArtifactBody::Pipeline { graph },
            )]),
        )
        .await?;
    let layout = json!({"start": {"x": 96.5, "y": 89.5}, "text": {"x": 495.0, "y": 89.5},
        "result": {"x": 850.0, "y": 89.5}});
    let draft = store
        .save_workflow_package_canvas(
            project.clone(),
            draft.id.clone(),
            draft.revision,
            draft.package,
            serde_json::from_value(json!({"pipeline": {"layout": layout}}))?,
        )
        .await?;
    let trace = store
        .simulate_workflow_package_revision(
            project.clone(),
            draft.id.clone(),
            draft.revision.clone(),
            "pipeline".into(),
            serde_json::from_value(json!({"inputs":{"inputs":{"label":"Visual builder"}}}))?,
        )
        .await?;
    let visible = serde_json::to_value(trace)?;
    assert_eq!(visible["graph"]["layout"], layout);
    let result = visible["steps"]
        .as_array()
        .context("missing trace steps")?
        .iter()
        .find(|step| step["nodeId"] == "result")
        .context("missing result")?;
    assert_eq!(
        result["outputs"]["summary"]["value"]["evidence"]["summary"],
        "Status for Visual builder"
    );
    assert!(store.list_workflow_runs(project.clone()).await?.is_empty());

    let publication = commit(&store, &project, &draft).await?;
    let published = artifact(&publication, "pipeline").version.clone();
    let request: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId": project, "checkoutId": repo.checkout_id, "requestId": "visual-builder-result",
        "workflow": {"id": published.id, "revision": published.revision},
        "inputs": {"label":"Visual builder"}
    }))?;
    let run = store.launch_workflow_request(request.clone()).await?;
    assert_eq!(store.launch_workflow_request(request).await?.id, run.id);
    drop(store);
    let reopened = Store::open(database)?;
    let evidence = reopened
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("missing published execution")?;
    assert_eq!(serde_json::to_value(&evidence.layout)?, layout);
    assert!(evidence.outputs.iter().any(|output| matches!(
        &output.artifact.value, ArtifactValue::ResultSummary { summary, .. } if summary == "Status for Visual builder"
    )));
    Ok(())
}
