use std::path::Path;

use serde_json::json;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery},
    jobs::AdmissionEnvironment,
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    providers::{Provider, SessionMode},
    runs::RunStatus,
    workflows::{Endpoint, Wire, tests::approved_delivery_repository},
};

use super::*;

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
