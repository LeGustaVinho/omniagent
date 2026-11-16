use super::*;
use crate::{
    conversations::tests::{confirm, prepare, setup},
    dispatch::{Delivery, DispatchSourceRevision, EventSource},
    jobs::AdmissionEnvironment,
    providers::{
        ProviderEvent, ProviderEventData, ProviderLocation, SessionIntent, TurnOutcome,
        capabilities::CapabilityAssessment,
    },
    runs::RunStatus,
};

const EARLY: &str = "Older dialogue retained in the transcript.";

async fn three_turns(store: &Store, chat: &Conversation, scope: &SessionScope) -> Result<()> {
    for text in [
        EARLY.repeat(80),
        "Recent user instruction one".into(),
        "Recent user instruction two".into(),
    ] {
        let attempt = prepare(store, &chat.id, scope.clone(), &text).await?;
        confirm(store, &attempt).await?;
    }
    Ok(())
}

fn environment(scope: &SessionScope) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    }
}

async fn finish_turn(
    store: &Store,
    attempt: super::super::AttemptPlan,
    answer: &str,
    capabilities: Option<CapabilityAssessment>,
) -> Result<()> {
    let run = attempt.run.id;
    store.mark_agent_running(run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(serde_json::json!({
        "version": 14, "contextId": attempt.context_id, "scope": attempt.scope, "baseCommit": null,
        "submittedMessages": attempt.input, "nativeHistory": { "mode": "fresh" }, "capabilities": capabilities,
    }))?;
    let dispatch = store
        .prepare_dispatch(run.clone(), snapshot, vec![b"{\"fixture\":true}".to_vec()])
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(run, RunStatus::Succeeded, Some(answer.into()))
        .await
}

#[tokio::test]
async fn compacted_seed_survives_restart_preserves_history_and_requires_its_covered_prefix()
-> Result<()> {
    let (_temp, store, chat, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    three_turns(&store, &chat, &scope).await?;
    let prior = store.conversation_history(chat.id.clone()).await?;
    let settings = store
        .conversation_compaction(chat.id.clone(), None, String::new())
        .await?
        .settings;
    assert_eq!(
        settings,
        CompactionSettings {
            automatic: true,
            threshold_percent: 80
        }
    );
    let no_budget = store
        .request_conversation_compaction(
            chat.id.clone(),
            prior.context_id.clone(),
            scope.clone(),
            scope.clone(),
            String::new(),
            true,
        )
        .await?;
    assert!(no_budget.request.is_none());
    let queued = store
        .request_conversation_compaction(
            chat.id.clone(),
            prior.context_id.clone(),
            scope.clone(),
            scope.clone(),
            String::new(),
            false,
        )
        .await?;
    let request = queued.request.context("Missing summary request")?;
    assert_eq!(request.status, "queued");
    assert_eq!(
        store.conversation(chat.id.clone()).await?.context_id,
        prior.context_id
    );
    let summary = store
        .claim_next_job(environment(&scope))
        .await?
        .context("Missing summary job")?;
    assert_ne!(summary.conversation_id, chat.id);
    assert_eq!(summary.scope.mode, SessionMode::ReadOnly);
    assert!(summary.input[0].text.contains(EARLY));
    assert!(!summary.input[0].text.contains("Recent user instruction"));
    let summary_run = summary.run.id.clone();
    finish_turn(
        &store,
        summary,
        "A compact summary retaining the original goal and verified decision.",
        None,
    )
    .await?;
    drop(store);

    let store = Store::open(scope.checkout.join("store.sqlite"))?;
    let status = store
        .conversation_compaction(chat.id.clone(), None, String::new())
        .await?;
    assert_eq!(
        status
            .request
            .as_ref()
            .context("Missing applied request")?
            .status,
        "activated"
    );
    assert_ne!(status.context_id, prior.context_id);
    assert_eq!(
        store.conversation_history(chat.id.clone()).await?.messages,
        prior.messages
    );
    let Some(ConversationArtifactRevision::Summary {
        id,
        source_context_id,
        run,
        covered,
        ..
    }) = status.active_summary
    else {
        anyhow::bail!("The summary did not become active")
    };
    assert_eq!(id, request.artifact_id);
    assert_eq!(source_context_id, prior.context_id);
    assert_eq!(run.run_id, summary_run);
    assert_eq!(run.requested_model, scope.model);
    assert_eq!(covered.len(), 2);
    assert_eq!(covered[0].origin.revision_id, prior.messages[0].revision_id);

    let fork = store
        .fork_conversation(
            chat.id.clone(),
            status.context_id.clone(),
            None,
            "Whole prefix".into(),
        )
        .await?;
    assert!(
        store
            .conversation_compaction(fork.id, None, String::new())
            .await?
            .active_summary
            .is_some()
    );
    let early = store
        .fork_conversation(
            chat.id.clone(),
            status.context_id.clone(),
            Some(prior.messages[0].revision_id.clone()),
            "Short prefix".into(),
        )
        .await?;
    assert!(
        store
            .conversation_compaction(early.id, None, String::new())
            .await?
            .active_summary
            .is_none()
    );

    let prompt = "Proceed from the compacted history";
    store
        .save_conversation_draft(chat.id.clone(), prompt.into())
        .await?;
    assert!(matches!(
        store
            .submit_conversation_continuation(
                chat.id.clone(),
                status.context_id.clone(),
                scope.clone(),
                prompt.into(),
            )
            .await?,
        super::super::ContinuationInspection::Queued { .. }
    ));
    let turn = store
        .claim_next_job(environment(&scope))
        .await?
        .context("Missing compacted continuation")?;
    assert!(matches!(turn.intent, SessionIntent::Start));
    assert_eq!(turn.input[0].revision_id, id);
    assert!(turn.input[0].text.contains("verified decision"));
    assert!(
        turn.input
            .iter()
            .any(|message| message.text == "Recent user instruction one")
    );
    assert!(
        turn.input
            .iter()
            .any(|message| message.text == "Recent user instruction two")
    );
    assert!(
        turn.input
            .iter()
            .all(|message| !message.text.contains(EARLY))
    );
    let run_id = turn.run.id.clone();
    finish_turn(&store, turn, "Main answer", None).await?;
    let lineage = store
        .dispatch_lineage(chat.id.clone(), run_id)
        .await?
        .context("Missing compacted dispatch")?;
    assert!(lineage.sources.iter().any(|source| matches!(source,
        DispatchSourceRevision::CompactedConversation { artifact_revision_id, .. } if artifact_revision_id == &id
    )));
    let history = store.conversation_history(chat.id.clone()).await?;
    store
        .rewind_conversation_history(
            chat.id.clone(),
            history.context_id,
            Some(prior.messages[0].revision_id.clone()),
        )
        .await?;
    assert!(
        store
            .conversation_compaction(chat.id, None, String::new())
            .await?
            .active_summary
            .is_none()
    );
    Ok(())
}
