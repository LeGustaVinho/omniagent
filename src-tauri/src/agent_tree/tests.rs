use std::process::Command;

use anyhow::{Context, Result, ensure};

use crate::{
    conversations::SessionScope,
    dispatch::EventSource,
    jobs::{
        AdmissionEnvironment, JobPermission, JobRequest, JobRequirements, JobRole, JobRule,
        JobWorkspace, RecoveryAction,
    },
    providers::{
        ChildDetails, Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionMode,
    },
    repository,
    runs::RunStatus,
    store::Store,
};

fn request() -> JobRequest {
    JobRequest {
        checkout_id: None,
        task_id: None,
        mode: SessionMode::ReadOnly,
        instructions: "Review the recorded subtree".into(),
        dependencies: Vec::new(),
        requires_bridge: false,
    }
}

#[tokio::test]
async fn preserves_real_delegation_retry_and_native_scope_after_reopen() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let checkout = temp.path().join("project");
    std::fs::create_dir(&checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    let database = temp.path().join("agent-tree.sqlite");
    let store = Store::open(database.clone())?;
    let location = repository::register(&store, dunce::canonicalize(checkout)?).await?;
    let conversation = store
        .create_conversation(location.checkout_id.clone(), "Agent tree fixture".into())
        .await?;
    let project = location.project_id;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "fixture".into(),
        model: Some("requested-model".into()),
        account_binding: Some("fixture-account".into()),
        checkout: location.checkout_root.into(),
        executable: std::env::current_exe()?,
        cli_version: "fixture".into(),
        runner: "fixture".into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        effort: None,
    };
    let parent = store
        .prepare_agent_attempt(
            conversation.id.clone(),
            conversation.context_id,
            scope,
            "Root".into(),
        )
        .await?;
    let root = parent.run.id;
    store.mark_agent_running(root.clone()).await?;
    store
        .set_project_job_rules(
            project.clone(),
            Vec::new(),
            vec![JobRule {
                role: JobRole::Agent,
                operation: SessionMode::ReadOnly,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: JobRequirements::default(),
            }],
        )
        .await?;
    let child_job = store
        .submit_agent_job(root.clone(), "child".into(), request())
        .await?;
    let queued = store
        .conversation_agent_tree(conversation.id.clone(), root.clone())
        .await?;
    assert_eq!(queued.managed.len(), 1);
    assert_eq!(queued.managed[0].id, child_job.id);
    assert!(queued.managed[0].attempts.is_empty());
    let runner = AdmissionEnvironment {
        runner: "fixture".into(),
        bridge_available: false,
    };
    let child = store
        .claim_next_job(runner.clone())
        .await?
        .context("managed child was not admitted")?;
    store.mark_agent_running(child.run.id.clone()).await?;
    let grandchild = store
        .submit_agent_job(child.run.id.clone(), "grandchild".into(), request())
        .await?;
    let native = ProviderEvent {
        location: ProviderLocation {
            session_id: Some("provider-session".into()),
            ..Default::default()
        },
        data: ProviderEventData::ChildActivity {
            child_id: Some("provider-child".into()),
            status: Some("running".into()),
            details: Some(ChildDetails {
                parent_id: Some("provider-parent".into()),
                ..Default::default()
            }),
        },
    };
    store
        .record_agent_observation(child.run.id.clone(), EventSource::Provider, native.clone())
        .await?;
    store
        .record_agent_observation(
            child.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::ModelObserved {
                    model: Some("observed-model".into()),
                    effort: None,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            child.run.id.clone(),
            RunStatus::Failed,
            Some("Fixture failure".into()),
        )
        .await?;
    store
        .queue_job_recovery(
            project,
            child_job.id.clone(),
            child.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let queued_retry = store
        .conversation_agent_tree(conversation.id.clone(), root.clone())
        .await?;
    assert!(queued_retry.managed[0].queued_recovery);
    let retry = store
        .claim_next_job(runner)
        .await?
        .context("managed retry was not admitted")?;
    store.mark_agent_running(retry.run.id.clone()).await?;
    assert!(
        store
            .conversation_agent_tree("unrelated-conversation".into(), root.clone())
            .await
            .is_err()
    );

    drop(store);
    let store = Store::open(database)?;
    let tree = store
        .conversation_agent_tree(conversation.id, root.clone())
        .await?;
    let branch = &tree.managed[0];
    assert_eq!(branch.parent.run_id, root);
    assert_eq!(branch.attempts.len(), 2);
    assert_eq!(branch.attempts[0].status, RunStatus::Failed);
    assert_eq!(
        branch.attempts[0].reported_model.as_deref(),
        Some("observed-model")
    );
    assert_eq!(branch.attempts[1].link.run_id, retry.run.id);
    assert_eq!(
        branch.attempts[1].previous_run_id.as_deref(),
        Some(child.run.id.as_str())
    );
    assert_eq!(branch.attempts[1].ordinal, 2);
    let nested = tree
        .managed
        .iter()
        .find(|branch| branch.id == grandchild.id)
        .context("grandchild was lost")?;
    assert_eq!(nested.parent.run_id, child.run.id);
    assert_eq!(tree.native_observations.len(), 1);
    assert_eq!(tree.native_observations[0].run_id, child.run.id);
    assert_eq!(tree.native_observations[0].event, native);
    let selected = store
        .conversation_agent_tree(retry.conversation_id, retry.run.id)
        .await?;
    assert_eq!(selected.attempts.len(), 2);
    assert_eq!(
        selected.parent.context("missing origin parent")?.run_id,
        root
    );
    Ok(())
}
