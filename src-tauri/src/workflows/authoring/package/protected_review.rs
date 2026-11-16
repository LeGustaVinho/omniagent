use std::{fs, path::PathBuf};

use serde_json::json;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery},
    jobs::AdmissionEnvironment,
    mcp_bridge::{RunBridge, invoke_test_mcp_tool},
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode},
    runs::RunStatus,
    workflows::{
        CommandOutputFormat, DeliveryRoles, builtin_delivery_graph,
        tests::approved_delivery_repository,
    },
};

use super::*;

const BRIEF: &str = "private-authoring-brief-canary-479144";
const INSTRUCTIONS: &str = "private-role-instructions-canary-257117";
const COMMAND_INPUT: &str = "private-command-configuration-canary-611983";
const AUTHOR_PROFILE: &str = "local-authoring-profile-id";
const REVIEW_PROFILE: &str = "local-review-profile-id";

#[tokio::test]
async fn protected_author_review_recovers_its_exact_draft_provenance_and_publication() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("protected-authoring.sqlite");
    let store = Store::open(database.clone())?;
    let project_checkout = approved_delivery_repository(&store, directory.path()).await?;
    let project = project_checkout.project_id;
    let checkout = PathBuf::from(project_checkout.checkout_root);
    let chat = store
        .create_conversation(
            project_checkout.checkout_id.clone(),
            "Review a protected workflow package".into(),
        )
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: AUTHOR_PROFILE.into(),
        account_binding: Some("local-account-binding-id".into()),
        executable: checkout.join("codex.exe"),
        checkout,
        cli_version: "fixture".into(),
        runner: "protected-review-test".into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("author-model".into()),
        effort: None,
    };
    let original = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Original workflow".into(),
                members: vec![member(
                    "pipeline",
                    PackageArtifactBody::Pipeline {
                        graph: builtin_delivery_graph(DeliveryRoles::default()),
                    },
                )],
            },
        )
        .await?;
    let original_publication = publish(&store, &project, &original).await?;
    let original_artifact = &original_publication.members[0].artifact;
    let base = original_artifact.version.clone();
    let draft = store
        .start_workflow_package_authoring(
            project.clone(),
            chat.id.clone(),
            project_checkout.checkout_id,
            scope,
            Some(WorkflowRevision {
                id: base.id.clone(),
                revision: base.revision.clone(),
            }),
            BRIEF.into(),
        )
        .await?;
    let request = draft
        .request
        .as_ref()
        .context("authoring request was not saved")?;
    let request_id = request.id.clone();
    assert_eq!(draft.package.members[0].base.as_ref(), Some(&base));
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: "protected-review-test".into(),
            bridge_available: true,
        })
        .await?
        .context("authoring request was not admitted")?;
    assert_eq!(attempt.scope.profile_id, AUTHOR_PROFILE);
    assert_eq!(
        attempt.scope.account_binding.as_deref(),
        Some("local-account-binding-id")
    );
    let author_prompt = attempt.input[0].text.clone();
    assert!(author_prompt.contains(BRIEF));
    let run = attempt.run.id.clone();
    store.mark_agent_running(run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version":13, "contextId":attempt.context_id, "scope":attempt.scope,
        "submittedMessages":attempt.input, "nativeHistory":{"mode":"fresh"},
    }))?;
    let dispatch = store
        .prepare_dispatch(
            run.clone(),
            snapshot,
            vec![serde_json::to_vec(&author_prompt)?],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let bridge = RunBridge::start(&store, run.clone()).await?;

    let mut pipeline = draft.package.members[0].clone();
    let PackageArtifactBody::Pipeline { graph } = &mut pipeline.body else {
        anyhow::bail!("selected base is not a pipeline");
    };
    graph.name = "Workflow with a reviewed role".into();
    pipeline.references.insert(
        "review-plan".into(),
        PackageReference::Draft {
            key: "reviewer".into(),
        },
    );
    let role_model = ModelSelection {
        provider: Provider::Claude,
        profile_id: REVIEW_PROFILE.into(),
        model_id: "review-model".into(),
    };
    let role = member(
        "reviewer",
        serde_json::from_value(json!({"kind":"role","template":{
            "name":"Scoped reviewer", "role":"agent", "model":role_model,
            "instructions":INSTRUCTIONS, "toolScope":"read_only", "completionContract":"Review the exact saved plan.",
            "reviewPolicy":"human_approval"
        }}))?,
    );
    let command = member(
        "local-command",
        PackageArtifactBody::Command {
            binding: PackageCommandBinding::Required {
                name: "review fixture command".into(),
            },
            arguments: vec!["--scoped".into()],
            input: serde_json::from_value(json!({"input":COMMAND_INPUT}))?,
            output: CommandOutputFormat::Json,
        },
    );
    let response = invoke_test_mcp_tool(
        &bridge,
        "apply_authoring_operations",
        json!({
            "requestId":"protected-edit", "expectedRevision":draft.revision, "operations":[
                {"operation":"put_member","member":pipeline},
                {"operation":"put_member","member":role},
                {"operation":"put_member","member":command}
            ]
        }),
    )
    .await?;
    let applied = &response["result"]["structuredContent"];
    assert_eq!(applied["status"], "applied", "{response}");
    let reviewed = applied["revision"]
        .as_str()
        .context("no working revision")?
        .to_owned();
    let response = invoke_test_mcp_tool(
        &bridge,
        "submit_authoring_for_review",
        json!({
            "requestId":"protected-review", "expectedRevision":reviewed,
        }),
    )
    .await?;
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"status":"submitted","revision":reviewed})
    );
    let saved = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(saved.revision, reviewed);
    assert_eq!(saved.parent_revision.as_ref(), Some(&draft.revision));
    let review_status = saved.request.as_ref().context("review context was lost")?;
    assert_eq!(review_status.id, request_id);
    assert_eq!(review_status.run_id.as_ref(), Some(&run));
    assert_eq!(review_status.state, "saved");
    assert!(saved.publication.is_none());
    store
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
        .await?;
    drop(bridge);
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    let after_restart = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(after_restart, saved);
    assert_eq!(after_restart.conversation_id.as_ref(), Some(&chat.id));
    assert_eq!(after_restart.package.members, [pipeline, role, command]);
    assert!(
        store
            .workflow_packages(project.clone(), Some(chat.id))
            .await?
            .iter()
            .any(|listed| listed.id == draft.id
                && listed.revision == reviewed
                && !listed.published)
    );
    let early = store
        .published_workflow_package_artifact(project.clone(), base.clone())
        .await?;
    assert_eq!(early.as_ref(), Some(original_artifact));
    let history = store
        .agent_dispatch(attempt.conversation_id, run.clone())
        .await?
        .context("author dispatch was lost")?;
    assert_eq!(history.snapshot.scope.profile_id, AUTHOR_PROFILE);
    assert_eq!(
        history
            .snapshot
            .submitted_messages
            .first()
            .context("review input was lost")?
            .text,
        author_prompt
    );
    assert_protected_review(
        &store,
        &project,
        &draft,
        &saved,
        &request_id,
        &run,
        &author_prompt,
    )
    .await?;

    let receipt = publish(&store, &project, &after_restart).await?;
    assert_eq!(receipt.reviewed_revision, reviewed);
    assert_eq!(receipt.originating_request_id.as_ref(), Some(&request_id));
    assert_eq!(receipt.originating_run_id.as_ref(), Some(&run));
    assert_eq!(receipt.decision_source, "desktop_user");
    let pipeline = receipt
        .members
        .iter()
        .find(|entry| entry.key == "pipeline")
        .context("published pipeline missing")?;
    let role = &receipt
        .members
        .iter()
        .find(|entry| entry.key == "reviewer")
        .context("published role missing")?
        .artifact;
    assert_eq!(pipeline.previous.as_ref(), Some(&base));
    assert_eq!(pipeline.artifact.version.id, base.id);
    assert_ne!(pipeline.artifact.version.revision, base.revision);
    assert_eq!(
        pipeline.artifact.references["review-plan"],
        ResolvedPackageReference::Published {
            version: role.version.clone()
        }
    );
    let PackageArtifactBody::Role { template } = &role.body else {
        anyhow::bail!("published role body changed type");
    };
    assert_eq!(template.model, role_model);
    assert_eq!(
        serde_json::to_value(&template.model)?,
        json!({"provider":"claude","profileId":REVIEW_PROFILE,"modelId":"review-model"})
    );

    drop(store);
    let store = Store::open(database.clone())?;
    let published = store
        .workflow_package(project.clone(), draft.id.clone())
        .await?;
    assert_eq!(published.publication.as_ref(), Some(&receipt));
    assert_eq!(published.package, saved.package);
    assert_eq!(published.request, saved.request);
    assert_eq!(
        store
            .published_workflow_package_artifact(project.clone(), base)
            .await?
            .as_ref(),
        Some(original_artifact)
    );
    assert_protected_publication(&store, &project, &receipt).await?;
    drop(store);

    for suffix in ["", "-wal", "-shm"] {
        let mut path = database.clone().into_os_string();
        path.push(suffix);
        match fs::read(path) {
            Ok(bytes) => assert_no_plaintext(&bytes),
            Err(error) if !suffix.is_empty() && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
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

async fn publish(
    store: &Store,
    project: &str,
    draft: &WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id.clone(), draft.revision.clone())
        .await?
    else {
        anyhow::bail!("the selected draft did not publish");
    };
    Ok(publication)
}

fn assert_no_plaintext(bytes: &[u8]) {
    for canary in [BRIEF, INSTRUCTIONS, COMMAND_INPUT] {
        assert!(
            !bytes
                .windows(canary.len())
                .any(|window| window == canary.as_bytes()),
            "package plaintext was stored outside its protected payload"
        );
    }
}

async fn assert_protected_review(
    store: &Store,
    project: &str,
    draft: &WorkflowPackageDraft,
    saved: &WorkflowPackageDraft,
    request: &str,
    run: &str,
    author_prompt: &str,
) -> Result<()> {
    let (project, draft, revision, parent, request, run) = (
        project.to_owned(),
        draft.id.clone(),
        saved.revision.clone(),
        draft.revision.clone(),
        request.to_owned(),
        run.to_owned(),
    );
    let (request_payload, draft_payload): (Vec<u8>, Vec<u8>) = store.execute(move |db| {
        Ok(db.query_row("SELECT r.payload, v.payload FROM workflow_package_drafts d
            JOIN workflow_package_revisions v ON v.draft_id = d.id AND v.id = d.head_revision
            JOIN workflow_package_requests r ON r.draft_id = d.id AND r.parent_revision = v.parent_revision
            JOIN job_attempts a ON a.job_id = r.job_id AND a.run_id = v.run_id
            JOIN workflow_package_mcp_sessions m ON m.run_id = a.run_id AND m.authoring_request_id = r.id
                AND m.submitted_revision = v.id
            WHERE d.project_id = ?1 AND d.id = ?2 AND v.id = ?3 AND v.parent_revision = ?4 AND r.id = ?5 AND a.run_id = ?6",
            params![project, draft, revision, parent, request, run], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }).await?;
    assert_no_plaintext(&request_payload);
    assert_no_plaintext(&draft_payload);
    assert_eq!(open::<String>(&request_payload)?, author_prompt);
    assert_eq!(
        open::<PackageDocument>(&draft_payload)?.package,
        saved.package
    );
    Ok(())
}

async fn assert_protected_publication(
    store: &Store,
    project: &str,
    expected: &WorkflowPackagePublication,
) -> Result<()> {
    let project = project.to_owned();
    let expected = expected.clone();
    store
        .execute(move |db| {
            let payload: Vec<u8> = db.query_row(
                "SELECT payload FROM workflow_package_publications
            WHERE id = ?1 AND project_id = ?2 AND reviewed_revision = ?3
                AND originating_request_id = ?4 AND originating_run_id = ?5",
                params![
                    expected.id,
                    project,
                    expected.reviewed_revision,
                    expected.originating_request_id,
                    expected.originating_run_id
                ],
                |row| row.get(0),
            )?;
            assert_no_plaintext(&payload);
            assert_eq!(open::<WorkflowPackagePublication>(&payload)?, expected);
            for member in &expected.members {
                let version = &member.artifact.version;
                let payload: Vec<u8> = db.query_row(
                    "SELECT payload FROM workflow_package_artifact_revisions
                WHERE scope_key = ?1 AND kind = ?2 AND artifact_id = ?3 AND revision = ?4",
                    params![
                        version.destination.key(&project),
                        version.kind.sql(),
                        version.id,
                        version.revision
                    ],
                    |row| row.get(0),
                )?;
                assert_no_plaintext(&payload);
                assert_eq!(open::<PublishedPackageArtifact>(&payload)?, member.artifact);
            }
            Ok(())
        })
        .await
}
