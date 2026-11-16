use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    jobs::{
        GoalContinuationOwner, GoalControlState, GoalDefinition, GoalStopAction, JobContext,
        JobScheduler,
    },
    providers::{
        Provider, SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};

#[tokio::test]
#[ignore = "requires an installed authenticated Codex profile and the OmniAgent bridge"]
async fn codex_managed_goal_restores_the_submitted_draft_and_owned_run() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let checkout = directory.path().join("checkout");
    std::fs::create_dir(&checkout)?;
    ensure!(
        Command::new("git.exe")
            .current_dir(&checkout)
            .args(["init", "--quiet", "--template="])
            .status()?
            .success(),
        "Could not initialize the managed goal fixture"
    );
    let database = directory.path().join("goal.sqlite");
    let store = Store::open(database.clone())?;
    let project = repository::register(&store, checkout).await?;
    let configured = store.configured_executable(Provider::Codex).await?;
    let model =
        std::env::var("OMNIAGENT_CODEX_GOAL_SMOKE_MODEL").unwrap_or_else(|_| "gpt-6-luna".into());
    let profile = desktop_profile(configured, Some(model.clone()))?;
    let selected = profile
        .models
        .iter()
        .find(|option| option.option.selectable && option.option.selection.model_id == model)
        .context("The installed Codex profile does not offer the fixture model")?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: selected.option.selection.profile_id.clone(),
        account_binding: profile.account_binding,
        executable: profile.executable,
        checkout: PathBuf::from(project.checkout_root),
        cli_version: profile.version,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: DESKTOP_PERMISSION_REVISION.into(),
        mode: SessionMode::ReadOnly,
        model: Some(model),
        effort: None,
    };
    let conversation = store
        .create_conversation(project.checkout_id, "Managed Goal smoke".into())
        .await?;
    let objective = "Reply with ORCHID_CHECKPOINT. Use no tool other than report_goal_progress to report this exact marker as evidence with action review.";
    let draft = format!("/goal {objective}");
    store
        .save_conversation_draft(conversation.id.clone(), draft.clone())
        .await?;
    let goal = store
        .start_conversation_goal(
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: conversation.context_id,
            },
            scope,
            None,
            GoalDefinition {
                objective: objective.into(),
                criteria: Vec::new(),
                max_turns: Some(1),
                max_seconds: Some(150),
                token_budget: None,
            },
            Some(draft),
        )
        .await?;
    assert_eq!(goal.continuation_owner, GoalContinuationOwner::Managed);
    assert_eq!(store.conversation_draft(conversation.id.clone()).await?, "");
    assert_eq!(
        store
            .request_goal_stop(
                project.project_id.clone(),
                goal.id.clone(),
                GoalStopAction::Pause
            )
            .await?,
        GoalControlState::Paused
    );
    drop(store);

    let store = Store::open(database.clone())?;
    store.recover_agent_attempts_after_restart().await?;
    let saved = store
        .conversation_goal_overview(conversation.id.clone())
        .await?
        .context("Missing restored paused goal")?;
    let saved = serde_json::to_value(saved)?;
    assert_eq!(saved["goal"]["id"], goal.id);
    assert_eq!(saved["goal"]["controlState"], "paused");
    let review_token = saved["transition"]["reviewToken"]
        .as_str()
        .context("Missing protected resume review")?;
    store
        .resume_goal_after_review(
            project.project_id.clone(),
            goal.id.clone(),
            review_token.into(),
        )
        .await?;
    let scheduler = JobScheduler::start(
        store.clone(),
        Arc::new(DirectWindowsRunner),
        Some(PathBuf::from(env!("CARGO_BIN_EXE_omniagent"))),
    );
    let outcome = tokio::time::timeout(Duration::from_secs(110), async {
        loop {
            let current = store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .context("Goal disappeared while running")?;
            if let Some(run) = current
                .job
                .latest_attempt
                .as_ref()
                .filter(|run| run.status.is_terminal())
            {
                ensure!(
                    run.status == RunStatus::Succeeded,
                    "Managed Goal attempt finished with {:?}",
                    run.status
                );
                let checkpoint = current
                    .checkpoint
                    .context("Successful Goal run has no persisted checkpoint")?;
                ensure!(checkpoint.run_id == run.run_id && checkpoint.next_action == "review");
                ensure!(
                    checkpoint
                        .report
                        .is_some_and(|report| report.evidence.contains("ORCHID_CHECKPOINT")),
                    "The provider did not submit its Goal progress through the bridge"
                );
                return Ok::<_, anyhow::Error>(run.run_id.clone());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    })
    .await;
    scheduler.shutdown().await?;
    let run = outcome.context("The live managed Goal did not reach a checkpoint")??;
    drop(store);

    let reopened = Store::open(database)?;
    reopened.recover_agent_attempts_after_restart().await?;
    let active = reopened
        .conversation_goal_overview(conversation.id.clone())
        .await?
        .context("Missing restored evidence")?;
    let active = serde_json::to_value(active)?;
    assert_eq!(active["goal"]["id"], goal.id);
    assert_eq!(active["goal"]["continuationOwner"], "managed");
    assert_eq!(active["goal"]["controlState"], "active");
    assert_eq!(active["activity"]["attempt"]["runId"], run);
    let dispatch = reopened
        .agent_dispatch(conversation.id.clone(), run)
        .await?
        .context("Missing original provider input")?;
    let launch = dispatch
        .snapshot
        .scoped_launch
        .context("Missing profile launch evidence")?;
    assert!(
        launch
            .inputs
            .iter()
            .any(|item| item.contents == "features.goals=false")
    );
    assert_eq!(
        reopened
            .request_goal_stop(project.project_id, goal.id, GoalStopAction::Cancel)
            .await?,
        GoalControlState::Cancelled
    );
    let cancelled = reopened
        .conversation_goal_overview(conversation.id)
        .await?
        .context("Missing historical Goal after cancellation")?;
    assert_eq!(
        serde_json::to_value(cancelled)?["goal"]["controlState"],
        "cancelled"
    );
    Ok(())
}
