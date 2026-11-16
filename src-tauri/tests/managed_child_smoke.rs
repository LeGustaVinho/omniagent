use std::{path::Path, process::Command, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{Conversation, MessageRole, SessionScope},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, Job, JobAttempt, JobScheduler, SubagentAuditPolicy,
        SubagentLimitMode,
    },
    providers::{
        Provider, ProviderEventData, SessionMode,
        profiles::{ConfiguredExecutable, DESKTOP_PERMISSION_REVISION, desktop_profile},
        runtime::start_managed_turn,
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

const LONG_PROMPT: &str = "Write a numbered list of 240 distinct short sentences, beginning with STARTED. Do not use tools.";
const DEADLINE: Duration = Duration::from_secs(120);

async fn fixture(root: &Path, provider: Provider) -> Result<(Store, Conversation, SessionScope)> {
    let path = root.join("checkout");
    std::fs::create_dir(&path)?;
    ensure!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()?
            .success(),
        "could not initialize the fixture checkout"
    );
    let checkout = dunce::canonicalize(path)?;
    let store = Store::open(root.join("queue.sqlite"))?;
    let project = repository::register(&store, checkout.clone()).await?;
    let conversation = store
        .create_conversation(project.checkout_id, "Managed child fixture".into())
        .await?;
    let desktop = desktop_profile(
        ConfiguredExecutable {
            provider,
            executable: None,
        },
        (provider == Provider::Antigravity).then(|| "gemini-3.8-flash".into()),
    )?;
    let selected = desktop
        .models
        .into_iter()
        .find(|model| {
            let id = model.option.selection.model_id.as_str();
            model.option.selectable
                && match provider {
                    Provider::Codex => id == "gpt-6-luna",
                    Provider::Claude => id == "claude-haiku-5-5" || id == "haiku",
                    Provider::Antigravity => id == "gemini-3.8-flash",
                }
        })
        .context("the CLI did not offer the fixture model")?;
    let effort = selected
        .efforts
        .iter()
        .any(|level| level == "low")
        .then(|| "low".into());
    let scope = SessionScope {
        provider,
        profile_id: selected.option.selection.profile_id,
        account_binding: desktop.account_binding,
        checkout,
        executable: desktop.executable,
        cli_version: desktop.version,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: DESKTOP_PERMISSION_REVISION.into(),
        mode: SessionMode::ReadOnly,
        model: Some(selected.option.selection.model_id),
        effort,
    };
    Ok((store, conversation, scope))
}

fn environment() -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: false,
    }
}

async fn approve_if_required(store: &Store, job: &Job) -> Result<()> {
    for blocker in store
        .inspect_job(job.project_id.clone(), job.id.clone(), environment())
        .await?
        .context("missing fixture job")?
        .blockers
    {
        if let AdmissionBlocker::Approval { approval_id } = blocker {
            store
                .decide_job_approval(job.project_id.clone(), approval_id, true)
                .await?;
        }
    }
    Ok(())
}

async fn wait_for_attempt(store: &Store, job: &Job, dispatched: bool) -> Result<JobAttempt> {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let current = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("missing fixture job")?;
            if let Some(attempt) = current.latest_attempt {
                if attempt.status.is_terminal() {
                    return Ok(attempt);
                }
                if dispatched
                    && let Some(conversation) = &attempt.conversation_id
                    && let Some(dispatch) = store
                        .agent_dispatch(conversation.clone(), attempt.run_id.clone())
                        .await?
                    && dispatch.delivery == omniagent::dispatch::Delivery::Submitted
                {
                    return Ok(attempt);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

async fn managed_children(provider: Provider) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (store, conversation, scope) = fixture(temp.path(), provider).await?;
    let parent = store
        .prepare_agent_attempt(
            conversation.id.clone(),
            conversation.context_id,
            scope.clone(),
            "Originating delegation".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    store
        .set_project_subagent_audit(
            parent.run.project_id.clone(),
            SubagentAuditPolicy::Available,
        )
        .await?;
    store
        .set_conversation_subagent_quota(
            conversation.id.clone(),
            Some(SubagentLimitMode::Managed),
            1,
            None,
        )
        .await?;
    store.set_job_queue_capacity(4).await?;
    let first = store
        .enqueue_selected_agent_job(
            parent.run.project_id.clone(),
            parent.run.checkout_id.clone(),
            Some(parent.run.id.clone()),
            scope.clone(),
            LONG_PROMPT.into(),
        )
        .await?;
    approve_if_required(&store, &first).await?;
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let active = wait_for_attempt(&store, &first, true).await?;
    ensure!(
        !active.status.is_terminal(),
        "{provider:?} child ended before interruption: {:?}",
        store
            .job_failure(first.project_id.clone(), active.run_id.clone())
            .await?
    );

    let second = store
        .enqueue_selected_agent_job(
            parent.run.project_id.clone(),
            parent.run.checkout_id.clone(),
            Some(parent.run.id.clone()),
            scope,
            "Reply exactly DELEGATED_OK without tools.".into(),
        )
        .await?;
    approve_if_required(&store, &second).await?;
    let pending = store
        .inspect_job(second.project_id.clone(), second.id.clone(), environment())
        .await?
        .context("missing second child")?;
    assert!(pending.job.latest_attempt.is_none());
    assert!(pending.blockers.contains(&AdmissionBlocker::SubagentQuota {
        conversation_id: conversation.id.clone(),
        active: 1,
        limit: 1,
    }));
    store
        .request_project_job_interrupt(
            first.project_id.clone(),
            first.id.clone(),
            active.run_id.clone(),
        )
        .await?;
    let stopped = wait_for_attempt(&store, &first, false).await?;
    assert_eq!(stopped.run_id, active.run_id);
    assert_eq!(stopped.status, RunStatus::Cancelled);
    let completed = wait_for_attempt(&store, &second, false).await?;
    ensure!(
        completed.status == RunStatus::Succeeded,
        "{provider:?} replacement child failed: {:?}",
        store
            .job_failure(second.project_id, completed.run_id)
            .await?
    );
    let messages = store
        .conversation_messages(
            completed
                .conversation_id
                .context("missing child conversation")?,
        )
        .await?;
    assert!(
        messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant
                && message.text.contains("DELEGATED_OK"))
    );
    let quota = store
        .conversation_subagent_quota(conversation.id.clone())
        .await?;
    assert_eq!(
        (
            quota.managed.active,
            quota.managed.queued,
            quota.managed.historical
        ),
        (0, 0, 2)
    );
    let tree = store
        .conversation_agent_tree(conversation.id.clone(), parent.run.id.clone())
        .await?;
    for (id, status) in [
        (active.run_id, RunStatus::Cancelled),
        (completed.run_id, RunStatus::Succeeded),
    ] {
        let (job, attempt) = tree
            .managed
            .iter()
            .find_map(|job| {
                job.attempts
                    .iter()
                    .find(|attempt| attempt.link.run_id == id)
                    .map(|attempt| (job, attempt))
            })
            .context("the managed history omitted the captured child")?;
        assert_eq!(job.parent.run_id, parent.run.id);
        assert_eq!(attempt.status, status);
        assert_eq!(attempt.prompt_delivery.as_deref(), Some("submitted"));
        assert!(attempt.context_recorded);
    }
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Succeeded, None)
        .await?;
    scheduler.shutdown().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a signed-in Codex CLI with GPT-6 Luna"]
async fn codex_managed_children_share_quota_and_stop_independently() -> Result<()> {
    managed_children(Provider::Codex).await
}

#[tokio::test]
#[ignore = "requires a signed-in Claude Code CLI with Haiku 5.5"]
async fn claude_managed_children_share_quota_and_stop_independently() -> Result<()> {
    managed_children(Provider::Claude).await
}

#[tokio::test]
#[ignore = "requires a signed-in Antigravity CLI with Gemini 3.8 Flash"]
async fn antigravity_managed_children_share_quota_and_stop_independently() -> Result<()> {
    managed_children(Provider::Antigravity).await
}

#[tokio::test]
#[ignore = "requires a signed-in Codex CLI with verified native delegation controls"]
async fn codex_applies_strict_total_and_the_independent_audit_modes() -> Result<()> {
    for (policy, expected_native) in [
        (SubagentAuditPolicy::Available, 1),
        (SubagentAuditPolicy::Complete, 0),
    ] {
        let temp = tempfile::tempdir()?;
        let (store, conversation, scope) = fixture(temp.path(), Provider::Codex).await?;
        store
            .set_conversation_subagent_quota(
                conversation.id.clone(),
                Some(SubagentLimitMode::Strict),
                1,
                Some(1),
            )
            .await?;
        store
            .set_conversation_subagent_audit(conversation.id.clone(), Some(policy))
            .await?;
        let plan = store
            .prepare_agent_attempt(
                conversation.id.clone(),
                conversation.context_id,
                scope.clone(),
                LONG_PROMPT.into(),
            )
            .await?;
        let run = plan.run.id.clone();
        let project = plan.run.project_id.clone();
        let checkout = plan.run.checkout_id.clone();
        let turn = start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), plan, None);
        let quota = tokio::time::timeout(DEADLINE, async {
            loop {
                let quota = store
                    .conversation_subagent_quota(conversation.id.clone())
                    .await?;
                if quota.native_sessions.iter().any(|session| session.verified)
                    && let Some(dispatch) = store
                        .agent_dispatch(conversation.id.clone(), run.clone())
                        .await?
                    && dispatch.delivery == omniagent::dispatch::Delivery::Submitted
                {
                    return Ok::<_, anyhow::Error>(quota);
                }
                ensure!(
                    !quota.native_sessions.is_empty(),
                    "Codex stopped before confirming the strict allocation"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await??;
        assert_eq!(quota.native_sessions[0].allowance, expected_native);
        let child = store
            .enqueue_selected_agent_job(
                project.clone(),
                checkout,
                Some(run.clone()),
                scope,
                "Reply exactly AUDITED_CHILD without tools.".into(),
            )
            .await?;
        approve_if_required(&store, &child).await?;
        let inspected = store
            .inspect_job(project, child.id, environment())
            .await?
            .context("missing audited child")?;
        let quota_blocked = inspected
            .blockers
            .iter()
            .any(|blocker| matches!(blocker, AdmissionBlocker::SubagentQuota { .. }));
        assert_eq!(quota_blocked, policy == SubagentAuditPolicy::Available);
        assert!(
            inspected
                .blockers
                .iter()
                .all(|blocker| matches!(blocker, AdmissionBlocker::SubagentQuota { .. }))
        );
        turn.control().interrupt()?;
        assert_eq!(
            tokio::time::timeout(DEADLINE, turn.wait()).await??,
            RunStatus::Cancelled
        );
        assert!(
            store
                .agent_dispatch(conversation.id.clone(), run)
                .await?
                .is_some()
        );
        assert!(
            store
                .conversation_subagent_quota(conversation.id)
                .await?
                .native_sessions
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires a signed-in Codex CLI with verified native delegation controls"]
async fn codex_reports_native_child_history_under_verified_controls() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (store, conversation, scope) = fixture(temp.path(), Provider::Codex).await?;
    store
        .set_conversation_subagent_quota(
            conversation.id.clone(),
            Some(SubagentLimitMode::Strict),
            1,
            Some(1),
        )
        .await?;
    store
        .set_conversation_subagent_audit(
            conversation.id.clone(),
            Some(SubagentAuditPolicy::Available),
        )
        .await?;
    let plan = store.prepare_agent_attempt(
        conversation.id.clone(), conversation.context_id, scope,
        "Use your native spawn_agent tool exactly once. Give the read-only child the task of replying with NATIVE_CHILD_REPORTED. Wait for the child's reply and confirm it in your final answer. Do not edit files.".into(),
    ).await?;
    let run_id = plan.run.id.clone();
    let task = start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), plan, None);
    let observed = tokio::time::timeout(DEADLINE, async {
        let mut verified_control = false;
        loop {
            let tree = store.conversation_agent_tree(conversation.id.clone(), run_id.clone()).await?;
            let quota = store.conversation_subagent_quota(conversation.id.clone()).await?;
            verified_control |= quota.native_scope.as_ref().is_some_and(|scope| {
                scope.run_id == run_id && scope.allowance == Some(1)
            }) && quota.native_sessions.iter().any(|session| {
                session.provider == Provider::Codex && session.verified
                    && session.strict_total && session.allowance == 1
            });
            if verified_control && tree.native_observations.iter().any(|observation| {
                observation.run_id == run_id && observation.event.location.session_id.is_some()
                    && matches!(&observation.event.data, ProviderEventData::ChildActivity {
                        child_id: Some(child_id), ..
                    } if !child_id.is_empty())
            }) {
                return Ok::<_, anyhow::Error>(());
            }
            if tree.attempts.last().is_some_and(|attempt| attempt.status.is_terminal()) {
                let history = store.agent_observations(conversation.id.clone(), run_id.clone(), 0).await?;
                let responses: Vec<_> = history.into_iter().filter_map(|event| match event.event.data {
                    ProviderEventData::AssistantFinal { text } | ProviderEventData::Error { message: text } =>
                        Some(omniagent::diagnostics::preview(&text)),
                    _ => None,
                }).collect();
                anyhow::bail!(
                    "Codex ended without an identified child: verified allocation {verified_control}, native events {:?}, response {:?}",
                    tree.native_observations, responses,
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await;
    if let Err(error) = observed
        .context("native delegation observation timed out")
        .and_then(|result| result)
    {
        let _ = task.control().interrupt();
        let _ = tokio::time::timeout(DEADLINE, task.wait()).await;
        return Err(error);
    }
    assert_eq!(
        tokio::time::timeout(DEADLINE, task.wait()).await??,
        RunStatus::Succeeded
    );
    drop(store);
    let saved = Store::open(temp.path().join("queue.sqlite"))?;
    let history = saved
        .conversation_agent_tree(conversation.id.clone(), run_id.clone())
        .await?;
    assert!(history.managed.is_empty());
    let identified = history
        .native_observations
        .iter()
        .filter_map(|observation| match &observation.event.data {
            ProviderEventData::ChildActivity {
                child_id: Some(id), ..
            } => Some(id),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(identified.len(), 1);
    let reported_statuses = history
        .native_observations
        .iter()
        .filter_map(|observation| match &observation.event.data {
            ProviderEventData::ChildActivity {
                child_id: Some(_),
                status,
                ..
            } => status.as_deref(),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(reported_statuses.contains("started"));
    assert!(reported_statuses.contains("completed"));
    let finalized = saved.conversation_subagent_quota(conversation.id).await?;
    assert!(finalized.native_sessions.is_empty());
    let reported = finalized
        .native
        .iter()
        .find(|item| item.provider == Provider::Codex)
        .context("the Codex child history is missing from the native counters")?;
    assert_eq!(reported.reported.historical, 1);
    assert_eq!(reported.reported.active, 0);
    Ok(())
}
