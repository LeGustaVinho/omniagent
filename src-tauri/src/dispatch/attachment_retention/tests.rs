use std::{fs, path::Path, process::Command};

use crate::{
    attachments::{AttachmentPreview, AttachmentReference, AttachmentSource, CapturedPayload},
    conversations::{AttemptPlan, SessionScope},
    dispatch::{Delivery, EventSource, NativeHistory},
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionIntent, SessionMode,
        TurnOutcome, questions::QuestionRoute,
    },
    repository,
    runs::RunStatus,
};

use super::*;

const SECRET: &str = "frozen confidential excerpt";

#[tokio::test]
async fn attachment_deletion_keeps_audit_ids_and_applies_to_later_backups() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    let database = root.join("main.sqlite");
    let store = Store::open(database)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let checkout = repository::register(&store, root.clone()).await?;
    let chat = store
        .create_conversation(checkout.checkout_id.clone(), "Source conversation".into())
        .await?;
    let other = store
        .create_conversation(checkout.checkout_id, "Independent conversation".into())
        .await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "attachment-fixture".into(),
        account_binding: Some("attachment-account".into()),
        checkout: root.clone(),
        executable: root.join("codex.exe"),
        cli_version: "1.0.0".into(),
        runner: "fixture".into(),
        permission_revision: "fixture-permissions".into(),
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    };
    fs::write(root.join("source.txt"), SECRET)?;
    let previews = || {
        [
            AttachmentDelivery::Excerpt {
                start: 0,
                end: SECRET.len() as u64,
            },
            AttachmentDelivery::Reference,
        ]
        .into_iter()
        .map(|delivery| {
            AttachmentPreview::read_with_delivery(
                &root,
                AttachmentSource::Checkout {
                    relative_path: "source.txt".into(),
                },
                delivery,
            )
        })
        .collect::<Result<Vec<_>>>()
    };

    let original = store
        .prepare_agent_attempt_with_attachments(
            chat.id.clone(),
            chat.context_id.clone(),
            scope.clone(),
            "Original message".into(),
            previews()?,
        )
        .await?;
    let original_dispatch = dispatch(&store, &original, &root, &scope, "source-session").await?;
    store
        .execute({
            let legacy = original_dispatch.clone();
            move |db| {
                db.execute(
                    "DELETE FROM dispatch_lineage WHERE dispatch_id = ?1",
                    [legacy],
                )?;
                Ok(())
            }
        })
        .await?;
    let target = AttachmentHistoryTarget {
        conversation_id: chat.id.clone(),
        message_revision_id: original.input[0].revision_id.clone(),
        attachment_ordinal: 0,
    };
    let cascade = AttachmentHistoryRemoval::WithDerivedCopies;
    let selected = AttachmentHistoryRemoval::Selected;

    let other_attempt = store
        .prepare_agent_attempt_with_attachments(
            other.id.clone(),
            other.context_id,
            scope.clone(),
            "Independent capture".into(),
            previews()?,
        )
        .await?;
    dispatch(&store, &other_attempt, &root, &scope, "other-session").await?;
    assert!(
        store
            .preview_attachment_history_removal(
                AttachmentHistoryTarget {
                    conversation_id: other.id.clone(),
                    ..target.clone()
                },
                cascade
            )
            .await
            .is_err()
    );

    let context = store.conversation(chat.id.clone()).await?.context_id;
    let stale = store
        .preview_attachment_history_removal(target.clone(), cascade)
        .await?;
    assert_eq!(
        (
            stale.saved_revisions,
            stale.dispatches,
            stale.native_sessions
        ),
        (1, 1, 1)
    );
    let edited = store
        .replace_conversation_message(
            chat.id.clone(),
            context,
            original.input[0].message_id.clone(),
            "Edited message".into(),
        )
        .await?;
    let fork = store
        .fork_conversation(chat.id.clone(), edited, None, "Fork".into())
        .await?;
    let fork_attempt = store
        .prepare_agent_attempt(
            fork.id.clone(),
            fork.context_id.clone(),
            scope.clone(),
            "Inspect the fork".into(),
        )
        .await?;
    let blocked = store
        .preview_attachment_history_removal(target.clone(), cascade)
        .await?;
    assert_eq!(
        (blocked.saved_revisions, blocked.unsettled_attempts),
        (3, 1)
    );
    assert!(
        store
            .confirm_attachment_history_removal(target.clone(), cascade, stale.revision)
            .await
            .is_err()
    );
    assert!(
        store
            .confirm_attachment_history_removal(target.clone(), cascade, blocked.revision)
            .await
            .is_err()
    );
    let fork_dispatch = dispatch(&store, &fork_attempt, &root, &scope, "fork-session").await?;
    fs::write(root.join("source.txt"), "different source content")?;
    let before = root.join("before.sqlite");
    backup(&store, &before).await?;

    let preview = store
        .preview_attachment_history_removal(target.clone(), selected)
        .await?;
    assert_eq!((preview.saved_revisions, preview.dispatches), (1, 0));
    store
        .confirm_attachment_history_removal(target.clone(), selected, preview.revision)
        .await?;
    assert!(
        stored_attachment_methods(&store, &target.message_revision_id).await?
            == [AttachmentDelivery::Reference]
    );
    assert!(
        stored_attachment_methods(&store, &fork_attempt.input[0].revision_id)
            .await?
            .len()
            == 2
    );
    assert_original_dispatch(&store, &chat.id, &original, &original_dispatch).await?;

    let fresh = store
        .prepare_agent_attempt(
            fork.id.clone(),
            store.conversation(fork.id.clone()).await?.context_id,
            scope.clone(),
            "Continue locally".into(),
        )
        .await?;
    assert!(matches!(fresh.intent, SessionIntent::Start));
    store
        .finish_agent_attempt(fresh.run.id, RunStatus::Cancelled, None)
        .await?;
    let late = store
        .record_agent_observation(
            original.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some("source-session".into()),
                    ..Default::default()
                },
                data: ProviderEventData::ContextRead {
                    path: "source.txt".into(),
                    tool: "late fixture read".into(),
                },
            },
        )
        .await;
    assert!(
        late.unwrap_err()
            .to_string()
            .contains("revoked by history deletion")
    );

    let preview = store
        .preview_attachment_history_removal(target.clone(), cascade)
        .await?;
    assert_eq!(
        (
            preview.saved_revisions,
            preview.dispatches,
            preview.native_sessions,
            preview.unsettled_attempts
        ),
        (2, 2, 2, 0)
    );
    store
        .confirm_attachment_history_removal(target, cascade, preview.revision)
        .await?;
    let after = root.join("after.sqlite");
    backup(&store, &after).await?;
    drop(store);

    let previous = Store::open(before)?;
    assert_original_dispatch(&previous, &chat.id, &original, &original_dispatch).await?;
    assert_original_dispatch(&previous, &fork.id, &fork_attempt, &fork_dispatch).await?;
    let restored = Store::open(after)?;
    for (conversation, attempt, dispatch) in [
        (&chat, &original, &original_dispatch),
        (&fork, &fork_attempt, &fork_dispatch),
    ] {
        let evidence = restored
            .agent_dispatch(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .context("dispatch must retain its audit identity")?;
        assert_eq!(&evidence.id, dispatch);
        assert_eq!(evidence.delivery, Delivery::Submitted);
        assert_eq!(evidence.removed_attachment_count, 1);
        assert!(evidence.frames.is_empty());
        assert_eq!(evidence.snapshot.attachments.len(), 1);
        assert_eq!(evidence.attachments[0].ordinal, 1);
        assert!(matches!(
            evidence.snapshot.attachments[0].input,
            AttachmentInput::Reference(_)
        ));
        let lineage = restored
            .dispatch_lineage(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .unwrap();
        assert!(lineage.sources.iter().any(|source| matches!(
            source,
            crate::dispatch::DispatchSourceRevision::Attachment {
                attachment_index: 0,
                ..
            }
        )));
        let identities = restored
            .conversation_sessions(conversation.id.clone())
            .await?;
        assert!(
            identities
                .iter()
                .any(|identity| identity.provider_session_id.is_some())
        );
    }
    let independent = restored
        .agent_dispatch(other.id, other_attempt.run.id)
        .await?
        .unwrap();
    assert_eq!(independent.frames, vec![SECRET.as_bytes().to_vec()]);
    let continued = restored
        .prepare_agent_attempt(
            fork.id.clone(),
            restored.conversation(fork.id.clone()).await?.context_id,
            scope,
            "Continue without the deleted excerpt".into(),
        )
        .await?;
    assert!(matches!(continued.intent, SessionIntent::Start));
    assert_eq!(continued.attachments.len(), 1);
    assert_eq!(
        continued.attachments[0].1.delivery(),
        AttachmentDelivery::Reference
    );
    assert_eq!(
        fs::read_to_string(root.join("source.txt"))?,
        "different source content"
    );
    Ok(())
}

async fn dispatch(
    store: &Store,
    attempt: &AttemptPlan,
    root: &Path,
    scope: &SessionScope,
    session: &str,
) -> Result<String> {
    let attachments = attempt
        .attachments
        .iter()
        .map(|(revision, captured)| AttachmentDispatch {
            message_revision_id: revision.clone(),
            metadata: captured.metadata.clone(),
            qualification_id: None,
            input: match &captured.payload {
                CapturedPayload::Excerpt { excerpt } => AttachmentInput::Excerpt(excerpt.clone()),
                CapturedPayload::Reference { .. } => {
                    AttachmentInput::Reference(AttachmentReference {
                        name: "source.txt".into(),
                        format: captured.metadata.format,
                        size: captured.metadata.size,
                        path: root.join("source.txt"),
                        uri: None,
                    })
                }
                CapturedPayload::Native { .. } => panic!("unexpected native attachment"),
            },
        })
        .collect();
    let draft = ContextSnapshot {
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
        filesystem_skills: vec![],
        reported_skills: None,
        instructions: None,
        scoped_launch: None,
        requested_skill: None,
        plugin_inventory: None,
        attachments,
        task_context: None,
        repository_markdown: None,
        question_route: QuestionRoute::Native,
        answered_question: None,
    };
    store.mark_agent_running(attempt.run.id.clone()).await?;
    let id = store
        .prepare_dispatch(
            attempt.run.id.clone(),
            draft,
            vec![SECRET.as_bytes().to_vec()],
        )
        .await?;
    store
        .settle_dispatch(id.clone(), Delivery::Submitted)
        .await?;
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
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Succeeded, None)
        .await?;
    Ok(id)
}

async fn stored_attachment_methods(
    store: &Store,
    revision: &str,
) -> Result<Vec<AttachmentDelivery>> {
    let revision = revision.to_owned();
    store
        .execute(move |db| {
            Ok(captured_for_revision(db, &revision)?
                .iter()
                .map(|file| file.delivery())
                .collect())
        })
        .await
}

async fn assert_original_dispatch(
    store: &Store,
    conversation: &str,
    attempt: &AttemptPlan,
    id: &str,
) -> Result<()> {
    let evidence = store
        .agent_dispatch(conversation.into(), attempt.run.id.clone())
        .await?
        .context("missing backup dispatch")?;
    assert_eq!(evidence.id, id);
    assert_eq!(evidence.frames, vec![SECRET.as_bytes().to_vec()]);
    assert!(
        matches!(&evidence.snapshot.attachments[0].input, AttachmentInput::Excerpt(excerpt) if excerpt.text == SECRET)
    );
    Ok(())
}

async fn backup(store: &Store, path: &Path) -> Result<()> {
    let path = path.to_string_lossy().into_owned();
    store
        .execute(move |db| {
            db.execute("VACUUM INTO ?1", [path])?;
            Ok(())
        })
        .await
}
