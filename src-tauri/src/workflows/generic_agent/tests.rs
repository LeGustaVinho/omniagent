use serde_json::json;

use crate::{
    dispatch::{ContextSnapshot, Delivery, EventSource},
    jobs::{GoalTurnAction, GoalTurnReport},
    providers::{ProviderEvent, ProviderEventData, ProviderLocation, TurnOutcome},
};

use super::*;

pub(in crate::workflows) async fn complete_response(
    store: &Store,
    attempt: &crate::conversations::AttemptPlan,
    answer: &str,
    goal_action: Option<GoalTurnAction>,
) -> Result<String> {
    let source_run = &attempt.run.id;
    store.mark_agent_running(source_run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version":1, "contextId":attempt.context_id, "scope":attempt.scope,
        "submittedMessages":attempt.input, "nativeHistory":{"mode":"fresh"},
        "questionRoute": attempt.question_route,
    }))?;
    let dispatch = store
        .prepare_dispatch(
            source_run.clone(),
            snapshot,
            vec![b"agent fixture".to_vec()],
        )
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    store
        .record_agent_observation(
            source_run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some(format!("fixture-session-{source_run}")),
                    turn_id: Some(format!("fixture-turn-{source_run}")),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    if let Some(action) = goal_action {
        assert!(
            store
                .record_goal_turn_report(
                    source_run.clone(),
                    GoalTurnReport {
                        action,
                        evidence: "The claimed result is ready for operator review".into()
                    }
                )
                .await?
        );
    }
    store
        .finish_observed_agent_attempt(
            source_run.clone(),
            RunStatus::Succeeded,
            Some(answer.into()),
            None,
            true,
        )
        .await?;
    Ok(dispatch)
}
