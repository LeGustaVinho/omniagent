use std::path::Path;

use serde_json::json;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery},
    jobs::AdmissionEnvironment,
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    providers::{Provider, SessionMode},
    runs::RunStatus,
    workflows::{
        BuiltinNode, DeliveryRoles, Endpoint, ManualWorkflowLaunch, NodeConfiguration,
        NodeInstance, Wire, WorkflowGraph, WorkflowLaunchSource, WorkflowRevision,
        builtin_delivery_graph, tests::approved_delivery_repository,
    },
};

use super::*;

const PIPELINE: &str = "pipeline";
const ROLE: &str = "reviewer";
const SUBFLOW: &str = "context";
const CONTEXT_NODE: &str = "load-context";
const MANUAL_TITLE: &str = "Repository review with the user's title";
const FEEDBACK: &str = "Keep the pipeline title and give the reusable context a more precise name.";
const EDITED_SUBFLOW: &str = "Load the pinned repository note";

fn node(id: &str, kind: BuiltinNode) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration: NodeConfiguration::None,
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn wire(from: (&str, &str), to: (&str, &str)) -> Wire {
    Wire {
        from: endpoint(from.0, from.1),
        to: endpoint(to.0, to.1),
    }
}

fn member(key: &str, destination: PackageDestination, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn requested_members() -> Result<Vec<PackageMember>> {
    let plan = builtin_delivery_graph(DeliveryRoles::default())
        .nodes
        .into_iter()
        .find(|node| node.id == "plan")
        .context("Missing canonical Plan node")?;
    let mut pipeline = member(
        PIPELINE,
        PackageDestination::Project,
        PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: "Repository review".into(),
                nodes: vec![
                    node("start", BuiltinNode::Start),
                    node(CONTEXT_NODE, BuiltinNode::Subflow),
                    plan,
                ],
                wires: vec![
                    wire(("start", "start"), (CONTEXT_NODE, "ready")),
                    wire(("start", "document"), (CONTEXT_NODE, "document")),
                    wire((CONTEXT_NODE, "loaded"), ("plan", "ready")),
                    wire((CONTEXT_NODE, "context"), ("plan", "context")),
                ],
            },
        },
    );
    pipeline.references = BTreeMap::from([
        ("plan".into(), PackageReference::Draft { key: ROLE.into() }),
        (
            CONTEXT_NODE.into(),
            PackageReference::Draft {
                key: SUBFLOW.into(),
            },
        ),
    ]);
    let reviewer = member(
        ROLE,
        PackageDestination::Project,
        serde_json::from_value(json!({
            "kind":"role", "template": {
                "name":"Repository reviewer", "role":"agent",
                "model":{"provider":"codex","profileId":"package-chat-fixture","modelId":"review-model"},
                "instructions":"Read the exact supplied Markdown revision.", "toolScope":"read_only",
                "completionContract":"Return a scoped review plan.", "reviewPolicy":"human_approval"
            }
        }))?,
    );
    let reusable = member(
        SUBFLOW,
        PackageDestination::Library,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Load context".into(),
                nodes: vec![
                    node("start", BuiltinNode::Start),
                    node("load", BuiltinNode::LoadContext),
                ],
                wires: vec![
                    wire(("start", "start"), ("load", "ready")),
                    wire(("start", "document"), ("load", "document")),
                ],
            },
            inputs: BTreeMap::from([
                ("ready".into(), endpoint("start", "start")),
                ("document".into(), endpoint("start", "document")),
            ]),
            outputs: BTreeMap::from([
                ("loaded".into(), endpoint("load", "loaded")),
                ("context".into(), endpoint("load", "context")),
            ]),
        },
    );
    Ok(vec![pipeline, reviewer, reusable])
}

async fn start_dispatch(
    store: &Store,
    scope: &SessionScope,
    draft: &WorkflowPackageDraft,
    brief: &str,
) -> Result<(RunBridge, String)> {
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        })
        .await?
        .context("The authoring request did not reach the scheduler")?;
    let request = draft
        .request
        .as_ref()
        .context("Missing authoring request")?;
    assert_eq!(attempt.scope.profile_id, scope.profile_id);
    let prompt = &attempt.input[0].text;
    assert!(
        prompt.contains(brief) && prompt.contains(&draft.revision) && prompt.contains(&request.id)
    );
    let submitted = serde_json::to_vec(prompt)?;
    let run = attempt.run.id;
    store.mark_agent_running(run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": attempt.context_id, "scope": attempt.scope,
        "submittedMessages": attempt.input, "nativeHistory": {"mode": "fresh"}
    }))?;
    let dispatch = store
        .prepare_dispatch(run.clone(), snapshot, vec![submitted])
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let bridge = RunBridge::start(store, run.clone()).await?;
    let catalog =
        invoke_test_mcp_tool(&bridge, "read_authoring_catalog", json!({"view":"draft"})).await?;
    assert_eq!(
        catalog["result"]["structuredContent"]["revision"], draft.revision,
        "{catalog}"
    );
    Ok((bridge, run))
}

async fn submit_for_review(
    bridge: &RunBridge,
    revision: &str,
    name: &str,
    members: &[PackageMember],
) -> Result<String> {
    let mut operations = vec![json!({"operation":"rename_package", "name":name})];
    operations.extend(
        members
            .iter()
            .map(|member| json!({"operation":"put_member", "member":member})),
    );
    let changed = invoke_test_mcp_tool(
        bridge,
        "apply_authoring_operations",
        json!({
            "requestId":"edit-package", "expectedRevision":revision, "operations":operations,
        }),
    )
    .await?;
    let revision = changed["result"]["structuredContent"]["revision"]
        .as_str()
        .with_context(|| format!("The authoring operations did not save a revision: {changed}"))?;
    let submitted = invoke_test_mcp_tool(
        bridge,
        "submit_authoring_for_review",
        json!({
            "requestId":"submit-package", "expectedRevision":revision,
        }),
    )
    .await?;
    assert_eq!(
        submitted["result"]["structuredContent"],
        json!({"status":"submitted", "revision":revision})
    );
    Ok(revision.into())
}

async fn publish(
    store: &Store,
    project: &str,
    draft: WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The selected package was not published by the user");
    };
    Ok(publication)
}

fn artifact<'a>(
    publication: &'a WorkflowPackagePublication,
    key: &str,
) -> Result<&'a PublishedPackageArtifact> {
    publication
        .members
        .iter()
        .find(|member| member.key == key)
        .map(|member| &member.artifact)
        .with_context(|| format!("Missing published member {key}"))
}

#[tokio::test]
async fn standalone_agent_submits_a_typed_pipeline_and_its_library_artifacts() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("standalone-authoring.sqlite"))?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let checkout = Path::new(&repo.checkout_root);
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "standalone-fixture".into(),
        account_binding: None,
        executable: checkout.join("codex.exe"),
        checkout: checkout.into(),
        cli_version: "fixture".into(),
        runner: "standalone-fixture".into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("author-model".into()),
        effort: None,
    };
    let brief = "Build a typed Pipeline with a Function Node, Node Prefab, Role, and Schema.";
    let initial = store
        .start_workflow_editor_authoring(
            project.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            None,
            brief.into(),
            Some(WorkflowAuthoringOutput::Package),
        )
        .await?;
    assert!(initial.conversation_id.is_none());
    assert!(initial.package.members.is_empty());
    let (bridge, run) = start_dispatch(&store, &scope, &initial, brief).await?;
    let definitions = invoke_test_mcp_tool(
        &bridge,
        "read_authoring_catalog",
        json!({"view":"node_definitions","kind":"typed_start"}),
    )
    .await?;
    assert_eq!(
        definitions["result"]["structuredContent"]["result"]["items"][0]["kind"],
        "typed_start"
    );

    let reference = |key| json!({"source":"draft","key":key});
    let start = |configuration| json!({"id":"start","kind":"start","configuration":configuration});
    let end = json!({"id":"end","kind":"end","configuration":{"kind":"end"}});
    let member = |key, name, body, references| {
        json!({
            "key":key,"name":name,"body":body,"references":references,
        })
    };
    let members: Vec<PackageMember> = serde_json::from_value(json!([
        member(
            "schema",
            "Reply Schema",
            json!({
                "kind":"schema","fields":[{"name":"reply","type":"string","required":true}]
            }),
            json!({})
        ),
        member(
            "role",
            "Reply Role",
            json!({"kind":"role","template":{
                "name":"Reply Role","role":"agent","model":{"provider":"codex","profileId":"standalone-fixture","modelId":"author-model"},
                "instructions":"Write one reply.","toolScope":"read_only","completionContract":"Return the reply.","reviewPolicy":"independent"
            }}),
            json!({})
        ),
        member(
            "prefab",
            "Reply Prefab",
            json!({
                "kind":"prompt","prompt":{"text":"Review {{input.reply}}","executor":"agent","schemaContract":true,"nodePrefab":true}
            }),
            json!({"role": reference("role"),"input_schema":reference("schema"),"output_schema":reference("schema")})
        ),
        member(
            "function",
            "Reply Function",
            json!({
                "kind":"subflow",
                "graph":{"name":"Reply Function","nodes":[
                    start(json!({"kind":"subflow_entry","outputs":[
                        {"name":"ready","value":{"kind":"control"}},
                        {"name":"inputs","value":{"kind":"data","artifact":"json_object"}}
                    ]})),
                    {"id":"reply","kind":"subflow","configuration":{"kind":"none"}}, end
                ],"wires":[
                    wire(("start","ready"),("reply","ready")), wire(("start","inputs"),("reply","input")),
                    wire(("reply","completed"),("end","ready")), wire(("reply","result"),("end","result"))
                ]},
                "inputs":{"ready":endpoint("start","ready"),"input":endpoint("start","inputs")},
                "outputs":{"completed":endpoint("end","completed"),"result":endpoint("end","result")}
            }),
            json!({"start:schema":reference("schema"),"reply":reference("prefab"),"end:schema":reference("schema")})
        ),
        member(
            "pipeline",
            "Reply Pipeline",
            json!({
                "kind":"pipeline","graph":{"name":"Reply Pipeline","nodes":[
                    {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
                    {"id":"reply","kind":"subflow","configuration":{"kind":"none"}}, end
                ],"wires":[
                    wire(("start","start"),("reply","ready")), wire(("start","inputs"),("reply","input")),
                    wire(("reply","completed"),("end","ready")), wire(("reply","result"),("end","result"))
                ]}
            }),
            json!({"start:schema":reference("schema"),"reply":reference("function"),"end:schema":reference("schema")})
        )
    ]))?;
    let submitted =
        submit_for_review(&bridge, &initial.revision, "Reply workflow", &members).await?;
    store
        .finish_agent_attempt(run, RunStatus::Succeeded, None)
        .await?;
    drop(bridge);
    let authored = store.workflow_package(project.clone(), initial.id).await?;
    assert_eq!(authored.revision, submitted);
    assert_eq!(authored.package.members, members);
    assert!(authored.publication.is_none());
    assert!(authored.conversation_id.is_none());
    assert!(store.list_conversations(repo.checkout_id).await?.is_empty());
    let published = publish(&store, project, authored).await?;
    assert_eq!(published.members.len(), 5);
    assert!(artifact(&published, "prefab")?.prompt_expansion.is_some());
    Ok(())
}

#[tokio::test]
async fn chat_authorship_editor_changes_and_revision_publish_exact_project_and_library_references()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("from-chat.sqlite"))?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let checkout = Path::new(&repo.checkout_root);
    let chat = store
        .create_conversation(
            repo.checkout_id.clone(),
            "Repository review conversation".into(),
        )
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "package-chat-fixture".into(),
        account_binding: None,
        executable: checkout.join("codex.exe"),
        checkout: checkout.into(),
        cli_version: "fixture".into(),
        runner: "package-chat-fixture".into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("author-model".into()),
        effort: None,
    };
    let brief = "Create a review pipeline with a new reviewer role and reusable context subflow.";
    let initial = store
        .start_workflow_package_authoring(
            project.clone(),
            chat.id.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            None,
            brief.into(),
        )
        .await?;
    assert!(initial.package.members.is_empty());
    let (bridge, run) = start_dispatch(&store, &scope, &initial, brief).await?;
    let requested = requested_members()?;
    let authored_revision = submit_for_review(
        &bridge,
        &initial.revision,
        "Repository review proposal",
        &requested,
    )
    .await?;
    store
        .finish_agent_attempt(run, RunStatus::Succeeded, None)
        .await?;
    drop(bridge);
    let authored = store
        .workflow_package(project.clone(), initial.id.clone())
        .await?;
    assert_eq!(authored.revision, authored_revision);
    assert_eq!(authored.package.members, requested);
    assert_eq!(authored.conversation_id.as_deref(), Some(chat.id.as_str()));
    assert!(authored.publication.is_none());

    let mut package = authored.package;
    let pipeline = package
        .members
        .iter_mut()
        .find(|member| member.key == PIPELINE)
        .context("Missing authored pipeline")?;
    let PackageArtifactBody::Pipeline { graph } = &mut pipeline.body else {
        anyhow::bail!("Expected a pipeline graph");
    };
    graph.name = MANUAL_TITLE.into();
    let editor = store
        .save_workflow_package(
            project.clone(),
            initial.id.clone(),
            authored_revision.clone(),
            package,
        )
        .await?;
    assert_eq!(
        editor.parent_revision.as_deref(),
        Some(authored_revision.as_str())
    );
    let authoring = store
        .request_workflow_package_changes(
            project.clone(),
            initial.id.clone(),
            editor.revision.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            FEEDBACK.into(),
        )
        .await?;
    let request_id = authoring
        .request
        .as_ref()
        .context("Missing revision request")?
        .id
        .clone();
    let (bridge, revised_run) = start_dispatch(&store, &scope, &authoring, FEEDBACK).await?;
    let mut revised = authoring
        .package
        .members
        .iter()
        .find(|member| member.key == SUBFLOW)
        .context("Missing reusable subflow")?
        .clone();
    let PackageArtifactBody::Subflow { graph, .. } = &mut revised.body else {
        anyhow::bail!("Expected a reusable subflow graph");
    };
    graph.name = EDITED_SUBFLOW.into();
    let final_revision = submit_for_review(
        &bridge,
        &editor.revision,
        "Reviewed repository package",
        &[revised],
    )
    .await?;
    let refused = invoke_test_mcp_tool(
        &bridge,
        "publish_workflow_package",
        json!({ "reviewedRevision": final_revision }),
    )
    .await?;
    assert!(refused["error"].is_object(), "{refused}");
    store
        .finish_agent_attempt(revised_run.clone(), RunStatus::Succeeded, None)
        .await?;
    drop(bridge);

    let reviewed = store
        .workflow_package(project.clone(), initial.id.clone())
        .await?;
    assert_eq!(reviewed.revision, final_revision);
    assert_eq!(
        reviewed.parent_revision.as_deref(),
        Some(editor.revision.as_str())
    );
    assert!(reviewed.publication.is_none());
    let publication = publish(&store, project, reviewed).await?;
    assert_eq!(publication.reviewed_revision, final_revision);
    assert_eq!(
        publication.originating_request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(
        publication.originating_run_id.as_deref(),
        Some(revised_run.as_str())
    );
    assert_eq!(publication.decision_source, "desktop_user");
    assert!(
        store
            .workflow_packages(project.clone(), Some(chat.id))
            .await?
            .iter()
            .any(|entry| entry.id == initial.id && entry.published)
    );

    let pipeline = artifact(&publication, PIPELINE)?;
    let role = artifact(&publication, ROLE)?;
    let subflow = artifact(&publication, SUBFLOW)?;
    assert_eq!(pipeline.version.destination, PackageDestination::Project);
    assert_eq!(role.version.destination, PackageDestination::Project);
    assert_eq!(subflow.version.destination, PackageDestination::Library);
    assert_eq!(
        pipeline.references,
        BTreeMap::from([
            (
                "plan".into(),
                ResolvedPackageReference::Published {
                    version: role.version.clone()
                }
            ),
            (
                CONTEXT_NODE.into(),
                ResolvedPackageReference::Published {
                    version: subflow.version.clone()
                }
            ),
        ])
    );
    let doc = store
        .capture_repository_markdown(repo.checkout_id.clone(), checkout.join("brief.md"))
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: WorkflowRevision {
                id: pipeline.version.id.clone(),
                revision: pipeline.version.revision.clone(),
            },
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: doc.revision.id,
            },
            request_id: "published-from-chat".into(),
            work_id: None,
        })
        .await?;
    let inspected = store
        .inspect_workflow_execution(project.clone(), run.id)
        .await?
        .context("Missing published workflow run")?;
    assert_eq!(inspected.definition.name, MANUAL_TITLE);
    assert!(
        inspected
            .definition
            .subflows
            .iter()
            .any(|entry| entry.path == CONTEXT_NODE && entry.version == subflow.version)
    );
    assert!(
        inspected
            .role_pins
            .iter()
            .any(|entry| entry.node_id == "plan" && entry.version == role.version)
    );

    let empty = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Editor-created workflow package".into(),
                members: Vec::new(),
            },
        )
        .await?;
    let mut updated_subflow = member(SUBFLOW, PackageDestination::Library, subflow.body.clone());
    updated_subflow.base = Some(subflow.version.clone());
    let PackageArtifactBody::Subflow { graph, .. } = &mut updated_subflow.body else {
        anyhow::bail!("Missing saved subflow");
    };
    assert_eq!(graph.name, EDITED_SUBFLOW);
    graph.name = "Revised in the package editor".into();
    let new_pipeline = member(
        "editor-pipeline",
        PackageDestination::Project,
        PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: "Created in the package editor".into(),
                nodes: vec![node("start", BuiltinNode::Start)],
                wires: Vec::new(),
            },
        },
    );
    let changes = store
        .save_workflow_package(
            project.clone(),
            empty.id,
            empty.revision,
            WorkflowPackage {
                name: empty.package.name,
                members: vec![new_pipeline, updated_subflow],
            },
        )
        .await?;
    let edited = publish(&store, project, changes).await?;
    assert_eq!(
        artifact(&edited, "editor-pipeline")?.version.destination,
        PackageDestination::Project
    );
    let updated = artifact(&edited, SUBFLOW)?;
    assert_eq!(updated.version.destination, PackageDestination::Library);
    assert_eq!(updated.version.id, subflow.version.id);
    assert_ne!(updated.version.revision, subflow.version.revision);
    assert_eq!(
        store
            .published_workflow_package_artifact(project.clone(), subflow.version.clone())
            .await?
            .as_ref(),
        Some(subflow)
    );
    Ok(())
}
