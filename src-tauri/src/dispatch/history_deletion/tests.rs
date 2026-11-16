use std::process::Command;

use anyhow::{Context, Result};

use crate::{
    conversations::{AttemptPlan, SessionScope, references::ConversationReferenceSelection},
    dispatch::{
        ContextSnapshot, ConversationArtifactRevision, ConversationSelection, Delivery,
        EventSource, NativeHistory, SelectedMessageSpan,
    },
    jobs::AdmissionEnvironment,
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionIntent, SessionMode,
        TurnOutcome,
        questions::QuestionRoute,
        suggestions::{SuggestionOutcome, SuggestionSource},
    },
    repository,
    runs::RunStatus,
};

use super::*;

const PROMPT: &str = "private source transcript 8f9e";
const SUMMARY: &str = "summary repeats private source transcript 8f9e";

#[tokio::test]
async fn previewed_history_deletion_distinguishes_payload_from_derivatives_after_restart()
-> Result<()> {
    for mode in [
        HistoryDeletionMode::Selected,
        HistoryDeletionMode::WithDerivedCopies,
    ] {
        exercise_mode(mode).await?;
    }
    Ok(())
}

async fn exercise_mode(mode: HistoryDeletionMode) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    let database = root.join("history.sqlite");
    let store = Store::open(database.clone())?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let checkout = repository::register(&store, root.clone()).await?;
    let source = store
        .create_conversation(checkout.checkout_id.clone(), "Source".into())
        .await?;
    let destination = store
        .create_conversation(checkout.checkout_id, "Reference recipient".into())
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "history-deletion-fixture".into(),
        account_binding: None,
        checkout: root,
        executable: std::env::current_exe()?,
        cli_version: "1.0.0".into(),
        runner: "fixture".into(),
        permission_revision: "fixture".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    };
    let original = store
        .prepare_agent_attempt(
            source.id.clone(),
            source.context_id.clone(),
            scope.clone(),
            PROMPT.into(),
        )
        .await?;
    let original_dispatch = complete(
        &store,
        &scope,
        &original,
        "native-source",
        "Independent answer",
    )
    .await?;
    let history = store.conversation_history(source.id.clone()).await?;
    let revision = history.messages[0].revision_id.clone();
    let fork = store
        .fork_conversation(
            source.id.clone(),
            history.context_id.clone(),
            Some(revision.clone()),
            "Retained copy".into(),
        )
        .await?;
    let fork_revision = store.conversation_history(fork.id.clone()).await?.messages[0]
        .revision_id
        .clone();
    let selection = ConversationSelection {
        conversation_id: source.id.clone(),
        context_id: history.context_id.clone(),
        spans: vec![SelectedMessageSpan {
            revision_id: revision.clone(),
            byte_range: None,
        }],
    };
    let queued = store
        .enqueue_conversation_summary(selection.clone(), scope.clone())
        .await?;
    let summary_attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        })
        .await?
        .context("summary was not claimed")?;
    let summary_dispatch =
        complete(&store, &scope, &summary_attempt, "native-summary", SUMMARY).await?;
    let summary = store
        .conversation_artifact_revision(source.id.clone(), queued.request_id.clone())
        .await?
        .context("summary result was not retained")?;
    let output = match summary {
        ConversationArtifactRevision::Summary { run, .. } => run.output_revision_id,
        _ => anyhow::bail!("expected a retained summary"),
    };
    let reference = store
        .preview_conversation_reference(
            destination.id.clone(),
            source.id.clone(),
            ConversationReferenceSelection::Summary {
                artifact_id: queued.request_id.clone(),
            },
        )
        .await?;
    let target = HistoryDeletionTarget::Message {
        conversation_id: source.id.clone(),
        message_revision_id: revision.clone(),
    };
    let preview = store
        .preview_conversation_history_deletion(target.clone(), mode)
        .await?;
    assert!(preview.unsettled_run_ids.is_empty());
    assert!(preview.message_revision_ids.contains(&revision));
    assert!(preview.native_sessions > 0);
    let cascade = mode == HistoryDeletionMode::WithDerivedCopies;
    assert_eq!(preview.artifact_ids.contains(&queued.request_id), cascade);
    assert_eq!(preview.dispatch_ids.contains(&summary_dispatch), cascade);
    assert_eq!(preview.dispatch_ids.contains(&original_dispatch), cascade);
    assert_eq!(preview.message_revision_ids.contains(&output), cascade);
    assert_eq!(
        preview.message_revision_ids.contains(&fork_revision),
        cascade
    );
    assert_eq!(preview.reference_ids.contains(&reference.id), cascade);

    let late = store
        .prepare_agent_attempt(
            source.id.clone(),
            history.context_id.clone(),
            scope.clone(),
            "Follow-up".into(),
        )
        .await?;
    let changed = store
        .preview_conversation_history_deletion(target.clone(), mode)
        .await?;
    assert!(changed.unsettled_run_ids.contains(&late.run.id));
    assert_ne!(changed.revision, preview.revision);
    assert!(
        store
            .confirm_conversation_history_deletion(target.clone(), mode, preview.revision)
            .await
            .is_err()
    );
    assert!(
        store
            .confirm_conversation_history_deletion(target.clone(), mode, changed.revision)
            .await
            .is_err()
    );
    store
        .finish_agent_attempt(late.run.id, RunStatus::Failed, None)
        .await?;

    let preview = store
        .preview_conversation_history_deletion(target.clone(), mode)
        .await?;
    store
        .confirm_conversation_history_deletion(target, mode, preview.revision)
        .await?;
    drop(store);
    let store = Store::open(database)?;

    let visible = store.conversation_history(source.id.clone()).await?;
    let removed = visible
        .messages
        .iter()
        .find(|message| message.revision_id == revision)
        .unwrap();
    assert!(removed.text.is_empty());
    assert!(
        visible
            .messages
            .iter()
            .any(|message| message.text == "Independent answer")
    );
    assert!(
        !visible
            .messages
            .iter()
            .any(|message| message.text.contains(PROMPT))
    );
    let forked = store.conversation_history(fork.id).await?;
    assert_eq!(forked.messages[0].revision_id, fork_revision);
    assert_eq!(forked.messages[0].text == PROMPT, !cascade);
    assert!(store.capture_conversation_excerpt(selection).await.is_err());
    let results = store
        .conversation_reference_source(destination.id.clone(), source.id.clone(), None)
        .await?;
    assert!(
        !results
            .messages
            .iter()
            .any(|item| item.revision_id == revision)
    );
    assert_eq!(
        results.summaries.iter().any(|item| item.text == SUMMARY),
        !cascade
    );
    assert_eq!(
        store
            .preview_conversation_reference(
                destination.id,
                source.id.clone(),
                ConversationReferenceSelection::Summary {
                    artifact_id: queued.request_id.clone()
                },
            )
            .await
            .is_ok(),
        !cascade
    );
    for (owner, run, dispatch, secret) in [
        (&source.id, &original.run.id, &original_dispatch, PROMPT),
        (
            &summary_attempt.conversation_id,
            &summary_attempt.run.id,
            &summary_dispatch,
            PROMPT,
        ),
    ] {
        let evidence = store
            .agent_dispatch(owner.clone(), run.clone())
            .await?
            .context("missing audit dispatch")?;
        assert_eq!(evidence.id, *dispatch);
        assert_eq!(evidence.history_payload_deleted, cascade);
        assert_eq!(
            evidence
                .frames
                .iter()
                .any(|frame| String::from_utf8_lossy(frame).contains(secret)),
            !cascade
        );
        assert_eq!(
            evidence
                .snapshot
                .submitted_messages
                .iter()
                .any(|message| message.text.contains(secret)),
            !cascade
        );
    }
    let links = store
        .dispatches_using_message(source.id.clone(), revision.clone())
        .await?;
    assert!(
        links
            .iter()
            .any(|entry| entry.dispatch_id == original_dispatch)
    );
    assert!(
        links
            .iter()
            .any(|entry| entry.dispatch_id == summary_dispatch)
    );
    let original_events = store
        .agent_observations(source.id.clone(), original.run.id.clone(), 0)
        .await?;
    assert_eq!(
        original_events.iter().any(|item| matches!(&item.event.data,
        ProviderEventData::PromptSuggestion { suggestion }
        if matches!(&suggestion.outcome, SuggestionOutcome::Available { text } if text == PROMPT))),
        !cascade
    );
    let observed = store
        .agent_observations(summary_attempt.conversation_id, summary_attempt.run.id, 0)
        .await?;
    assert_eq!(
        observed.iter().any(|item| matches!(&item.event.data,
        ProviderEventData::AssistantFinal { text } if text == SUMMARY)),
        !cascade
    );
    assert!(
        store
            .record_agent_observation(
                original.run.id,
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::AssistantFinal {
                        text: PROMPT.into()
                    }
                },
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked")
    );
    let next = store
        .prepare_agent_attempt(source.id, visible.context_id, scope, "Continue".into())
        .await?;
    assert!(matches!(next.intent, SessionIntent::Start));
    assert!(
        !next
            .input
            .iter()
            .any(|message| message.revision_id == revision)
    );
    assert!(
        !next
            .input
            .iter()
            .any(|message| message.text.contains(PROMPT))
    );
    Ok(())
}

async fn complete(
    store: &Store,
    scope: &SessionScope,
    attempt: &AttemptPlan,
    session: &str,
    answer: &str,
) -> Result<String> {
    store.mark_agent_running(attempt.run.id.clone()).await?;
    let snapshot = ContextSnapshot {
        version: 13,
        context_id: attempt.context_id.clone(),
        scope: scope.clone(),
        session_id: None,
        job_policy_revision: None,
        role_context: None,
        base_commit: None,
        submitted_messages: attempt.input.clone(),
        conversation_references: Vec::new(),
        native_history: NativeHistory::Fresh,
        capabilities: None,
        filesystem_skills: Vec::new(),
        reported_skills: None,
        instructions: None,
        scoped_launch: None,
        requested_skill: None,
        plugin_inventory: None,
        attachments: Vec::new(),
        task_context: None,
        repository_markdown: None,
        question_route: QuestionRoute::Native,
        answered_question: None,
    };
    let id = store
        .prepare_dispatch(
            attempt.run.id.clone(),
            snapshot,
            vec![attempt.input[0].text.as_bytes().to_vec()],
        )
        .await?;
    store
        .settle_dispatch(id.clone(), Delivery::Submitted)
        .await?;
    if attempt.input[0].text == PROMPT {
        let suggestion = SuggestionSource {
            provider_session_id: session.into(),
            provider_turn_id: Some(format!("{session}-turn")),
            provider_result_id: None,
            message_id: attempt.input[0].message_id.clone(),
        }
        .settlement(SuggestionOutcome::Available {
            text: PROMPT.into(),
        });
        store
            .record_agent_observation(attempt.run.id.clone(), EventSource::Provider, suggestion)
            .await?;
    }
    for data in [
        ProviderEventData::AssistantFinal {
            text: answer.into(),
        },
        ProviderEventData::TurnFinished {
            outcome: TurnOutcome::Completed,
        },
    ] {
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some(session.into()),
                        turn_id: Some(format!("{session}-turn")),
                        ..Default::default()
                    },
                    data,
                },
            )
            .await?;
    }
    store
        .finish_agent_attempt(
            attempt.run.id.clone(),
            RunStatus::Succeeded,
            Some(answer.into()),
        )
        .await?;
    Ok(id)
}
