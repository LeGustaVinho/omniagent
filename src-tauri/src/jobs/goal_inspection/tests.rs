use std::{path::PathBuf, process::Command};

use anyhow::{Result, ensure};

use crate::{
    conversations::SessionScope,
    jobs::{GoalStopAction, JobContext},
    providers::SessionMode,
    repository::register,
};

use super::*;

#[tokio::test]
async fn overview_returns_confirmed_controls_and_keeps_replaced_goals_after_reopening() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let checkout = dunce::canonicalize(root.path())?;
    ensure!(
        Command::new("git.exe")
            .current_dir(&checkout)
            .args(["init", "--quiet"])
            .status()?
            .success(),
        "initialize goal fixture"
    );
    let path = checkout.join("app.sqlite");
    let store = Store::open(path.clone())?;
    let project = register(&store, checkout).await?;
    let chat = store
        .create_conversation(project.checkout_id, "Release".into())
        .await?;
    let scope = SessionScope {
        provider: Provider::Claude,
        profile_id: "goal-fixture".into(),
        account_binding: None,
        executable: root.path().join("claude.exe"),
        checkout: PathBuf::from(project.checkout_root),
        cli_version: "1.0.0".into(),
        runner: "fixture".into(),
        permission_revision: "goal".into(),
        mode: SessionMode::ReadOnly,
        model: Some("review-model".into()),
        effort: None,
    };
    let context = JobContext {
        conversation_id: chat.id.clone(),
        context_id: chat.context_id,
    };
    let first = GoalDefinition {
        objective: "Review release".into(),
        criteria: vec!["Confirm the artifact".into()],
        max_turns: Some(2),
        max_seconds: None,
        token_budget: None,
    };
    let draft = "/goal Review release".to_owned();
    store
        .save_conversation_draft(chat.id.clone(), draft.clone())
        .await?;
    let initial = store
        .start_conversation_goal(
            context.clone(),
            scope.clone(),
            None,
            first.clone(),
            Some(draft),
        )
        .await?;
    assert_eq!(store.conversation_draft(chat.id.clone()).await?, "");
    let view = store
        .conversation_goal_overview(chat.id.clone())
        .await?
        .unwrap();
    assert_eq!(view.goal.id, initial.id);
    assert_eq!(view.goal.definition, first);
    assert_eq!(view.goal.continuation_owner, GoalContinuationOwner::Managed);
    assert!(view.managed_reason.is_some());
    assert_eq!(
        store
            .request_goal_stop(
                project.project_id.clone(),
                initial.id.clone(),
                GoalStopAction::Pause
            )
            .await?,
        GoalControlState::Paused
    );
    let paused = store
        .conversation_goal_overview(chat.id.clone())
        .await?
        .unwrap();
    assert_eq!(paused.goal.control_state, GoalControlState::Paused);
    let review = paused.transition.unwrap();
    assert_eq!(review.definition, first);
    assert_eq!(review.scope.checkout_root, scope.checkout);
    assert_eq!(review.scope.model, scope.model);
    assert_eq!(
        review.checkpoint_context_id.as_deref(),
        Some(context.context_id.as_str())
    );
    let stale = store
        .resume_goal_after_review(
            project.project_id.clone(),
            initial.id.clone(),
            "a stale checkpoint".into(),
        )
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("checkpoint or scope changed"));
    store
        .resume_goal_after_review(
            project.project_id.clone(),
            initial.id.clone(),
            review.review_token,
        )
        .await?;
    assert_eq!(
        store
            .conversation_goal_overview(chat.id.clone())
            .await?
            .unwrap()
            .goal
            .control_state,
        GoalControlState::Active
    );
    assert_eq!(
        store
            .request_goal_stop(
                project.project_id,
                initial.id.clone(),
                GoalStopAction::Cancel
            )
            .await?,
        GoalControlState::Cancelled
    );
    let stopped = store
        .conversation_goal_overview(chat.id.clone())
        .await?
        .unwrap();
    assert_eq!(stopped.goal.control_state, GoalControlState::Cancelled);
    let second = GoalDefinition {
        objective: "Review the next package".into(),
        ..first.clone()
    };
    let replacement = store
        .start_conversation_goal(context, scope, None, second, None)
        .await?;
    drop(store);
    let reopened = Store::open(path)?;
    let restored = reopened.conversation_goal_overview(chat.id).await?.unwrap();
    assert_eq!(restored.goal.id, replacement.id);
    assert_eq!(restored.history.len(), 1);
    assert_eq!(restored.history[0].objective, first.objective);
    assert_eq!(
        restored.history[0].control_state,
        GoalControlState::Cancelled
    );
    Ok(())
}
