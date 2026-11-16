use std::{fs, process::Command};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    dispatch::EventSource,
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, JobRequest, ProjectSubagentPolicy, SubagentCounts,
        SubagentLimitMode,
    },
    providers::{Provider, ProviderEvent, ProviderEventData, SessionMode},
    repository,
    runner::DirectWindowsRunner,
    runs::RunStatus,
    store::Store,
};

#[tokio::test]
async fn inspected_child_uses_the_scheduler_quota_of_its_originating_conversation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let repository = directory.path().join("repo");
    fs::create_dir(&repository)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()?
            .success()
    );
    let store = Store::open(directory.path().join("store.sqlite"))?;
    let checkout = repository::register(&store, repository.clone()).await?;
    let root = store
        .create_conversation(checkout.checkout_id, "Originating conversation".into())
        .await?;
    store
        .set_project_subagent_policy(
            checkout.project_id.clone(),
            ProjectSubagentPolicy {
                mode: SubagentLimitMode::Managed,
                maximum: 2,
            },
        )
        .await?;
    let strict = Some(SubagentLimitMode::Strict);
    store
        .set_conversation_subagent_quota(root.id.clone(), strict, 3, Some(1))
        .await?;
    let parent = store
        .prepare_agent_attempt(
            root.id.clone(),
            root.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(repository)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner::IDENTITY.into(),
                permission_revision: "fixture".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            "Start the root CLI".into(),
        )
        .await?;
    let parent_run = parent.run.id;
    store.mark_agent_running(parent_run.clone()).await?;
    let state = store
        .conversation_run_subagent_quota(root.id.clone(), parent_run.clone())
        .await?;
    assert_eq!((state.requested, state.authorized), (3, 2));
    assert_eq!(state.managed, SubagentCounts::default());
    let native = state.native_scope.context("root CLI allocation missing")?;
    assert_eq!(native.allowance, Some(1));
    assert_eq!(serde_json::to_value(native.enforcement)?, "pending");

    let child = store
        .submit_agent_job(
            parent_run.clone(),
            "child-request".into(),
            JobRequest {
                mode: SessionMode::ReadOnly,
                checkout_id: None,
                task_id: None,
                instructions: "Review the change".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
        )
        .await?;
    let environment = AdmissionEnvironment {
        runner: DirectWindowsRunner::IDENTITY.into(),
        bridge_available: false,
    };
    let inspected = store
        .inspect_job(
            checkout.project_id.clone(),
            child.id.clone(),
            environment.clone(),
        )
        .await?
        .unwrap();
    let [AdmissionBlocker::Approval { approval_id }] = inspected.blockers.as_slice() else {
        anyhow::bail!(
            "expected approval before child admission, got {:?}",
            inspected.blockers
        );
    };
    store
        .decide_job_approval(checkout.project_id, approval_id.clone(), true)
        .await?;
    let queued = store
        .conversation_run_subagent_quota(root.id.clone(), parent_run.clone())
        .await?;
    assert_eq!(
        queued.managed,
        SubagentCounts {
            active: 0,
            queued: 1,
            historical: 0
        }
    );

    let attempt = store
        .claim_next_job(environment)
        .await?
        .context("managed slot did not open")?;
    store.mark_agent_running(attempt.run.id.clone()).await?;
    assert_ne!(attempt.conversation_id, root.id);
    assert!(
        store
            .conversation_run_subagent_quota(root.id.clone(), attempt.run.id.clone())
            .await
            .is_err()
    );
    let owned = store
        .conversation_run_subagent_quota(attempt.conversation_id.clone(), attempt.run.id.clone())
        .await?;
    assert_eq!(owned.conversation_id, root.id);
    assert_eq!(
        owned.managed,
        SubagentCounts {
            active: 1,
            queued: 0,
            historical: 0
        }
    );
    assert_eq!(
        owned
            .native_scope
            .context("child CLI allocation missing")?
            .allowance,
        Some(0)
    );

    store
        .set_conversation_subagent_quota(root.id.clone(), strict, 1, Some(1))
        .await?;
    store
        .record_agent_observation(
            parent_run,
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::ChildActivity {
                    child_id: Some("native-child".into()),
                    status: Some("running".into()),
                    details: None,
                },
            },
        )
        .await?;
    let updated = store
        .conversation_run_subagent_quota(attempt.conversation_id.clone(), attempt.run.id.clone())
        .await?;
    assert_eq!((updated.requested, updated.authorized), (1, 1));
    assert!(updated.configuration_pending);
    assert_eq!(updated.managed.active, 1);
    let [native] = updated.native.as_slice() else {
        anyhow::bail!("native telemetry should have a separate row");
    };
    assert_eq!(native.provider, Provider::Codex);
    assert_eq!(
        native.reported,
        SubagentCounts {
            active: 1,
            queued: 0,
            historical: 0
        }
    );

    store
        .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    let ended = store
        .conversation_run_subagent_quota(attempt.conversation_id, attempt.run.id)
        .await?;
    assert_eq!(
        ended.managed,
        SubagentCounts {
            active: 0,
            queued: 0,
            historical: 1
        }
    );
    let historical_scope = ended
        .native_scope
        .context("historical CLI allocation missing")?;
    assert_eq!(historical_scope.allowance, Some(0));
    assert_eq!(serde_json::to_value(historical_scope.enforcement)?, "ended");
    Ok(())
}
