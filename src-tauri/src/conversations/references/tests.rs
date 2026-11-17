use std::{fs, path::Path};

use crate::{
    conversations::{
        ConversationMentionDraftChange, MessageRole, activate_context, insert_message,
        write_context,
    },
    dispatch::{ByteRange, HistoryDeletionMode, HistoryDeletionTarget},
    repository::{self, ProjectCheckout},
    store::DraftKey,
};

use super::*;

#[tokio::test]
async fn automatic_reference_pins_a_budgeted_unicode_suffix_and_updates_the_draft_atomically()
-> Result<()> {
    let scratch = tempfile::tempdir()?;
    let database = scratch.path().join("mention.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = init(&store, &scratch.path().join("checkout")).await?;
    let source = store
        .create_conversation(checkout.checkout_id.clone(), "Release".into())
        .await?;
    let destination = store
        .create_conversation(checkout.checkout_id, "Destination".into())
        .await?;
    let message = format!("{}complete 🦀", "é\n".repeat(MAX_DRAFT_REFERENCE_BYTES));
    let (context, latest) = store
        .execute({
            let source = source.id.clone();
            let message = message.clone();
            move |db| {
                let older = insert_message(
                    db,
                    &source,
                    MessageRole::User,
                    protection::protect(b"older")?,
                )?;
                let latest = insert_message(
                    db,
                    &source,
                    MessageRole::Assistant,
                    protection::protect(message.as_bytes())?,
                )?;
                Ok((
                    activate_context(db, &source, &[older, latest.clone()])?,
                    latest,
                ))
            }
        })
        .await?;
    let preview = store
        .preview_conversation_reference(
            destination.id.clone(),
            source.id.clone(),
            ConversationReferenceSelection::Automatic,
        )
        .await?;
    assert!(serde_json::to_vec(&preview.artifact)?.len() <= MAX_DRAFT_REFERENCE_BYTES);
    match &preview.artifact {
        ConversationArtifactRevision::Excerpt {
            source_context_id,
            excerpts,
            ..
        } => {
            assert_eq!(source_context_id, &context);
            let [excerpt] = excerpts.as_slice() else {
                panic!("expected one clipped excerpt")
            };
            assert_eq!(excerpt.source.origin.revision_id, latest);
            let ByteRange { start, end } = excerpt.source.byte_range;
            assert!(start > 0 && message.is_char_boundary(start));
            assert_eq!(end, message.len());
            assert_eq!(excerpt.text, message[start..]);
        }
        _ => panic!("expected an excerpt"),
    }
    let original = "Read @Rel before approving";
    let replacement = "Read @Release before approving";
    let destination_id = destination.id;
    let draft = || DraftKey::Conversation(destination_id.clone());
    store
        .save_draft(draft(), original.as_bytes().to_vec())
        .await?;
    let attach = |expected: &str| ConversationMentionDraftChange {
        expected: expected.into(),
        replacement: replacement.into(),
    };
    assert!(
        store
            .edit_conversation_reference_and_draft(
                destination_id.clone(),
                preview.id.clone(),
                true,
                Some(attach("stale"))
            )
            .await
            .is_err()
    );
    assert!(
        store
            .conversation_reference_draft(destination_id.clone())
            .await?
            .is_empty()
    );
    assert_eq!(
        store.load_draft(draft()).await?.as_deref(),
        Some(original.as_bytes())
    );
    store
        .edit_conversation_reference_and_draft(
            destination_id.clone(),
            preview.id.clone(),
            true,
            Some(attach(original)),
        )
        .await?;
    drop(store);

    let reopened = Store::open(database)?;
    assert_eq!(
        reopened.load_draft(draft()).await?.as_deref(),
        Some(replacement.as_bytes())
    );
    assert_eq!(
        reopened
            .conversation_reference_draft(destination_id.clone())
            .await?,
        [preview.clone()]
    );
    let target = HistoryDeletionTarget::Message {
        conversation_id: source.id,
        message_revision_id: latest,
    };
    let mode = HistoryDeletionMode::WithDerivedCopies;
    let deletion = reopened
        .preview_conversation_history_deletion(target.clone(), mode)
        .await?;
    assert!(deletion.reference_ids.contains(&preview.id));
    reopened
        .confirm_conversation_history_deletion(target, mode, deletion.revision)
        .await?;
    assert!(
        reopened
            .conversation_reference_draft(destination_id.clone())
            .await?
            .is_empty()
    );
    assert!(
        reopened
            .edit_conversation_reference_draft(destination_id, preview.id, true)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn scopes_reference_preview_to_project_and_destination_and_replays_exact_bytes_after_restart()
-> Result<()> {
    let scratch = tempfile::tempdir()?;
    let database = scratch.path().join("references.sqlite");
    let store = Store::open(database.clone())?;
    let first = init(&store, &scratch.path().join("first")).await?;
    let linked_path = scratch.path().join("linked");
    git(
        Path::new(&first.checkout_root),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked_path.to_str().unwrap(),
        ],
    )?;
    let linked = repository::register(&store, linked_path).await?;
    assert_eq!(linked.project_id, first.project_id);
    assert_ne!(linked.checkout_id, first.checkout_id);
    let other = init(&store, &scratch.path().join("other")).await?;
    let source = store
        .create_conversation(first.checkout_id.clone(), "Original source".into())
        .await?;
    let destination = store
        .create_conversation(linked.checkout_id.clone(), "Destination".into())
        .await?;
    let other_destination = store
        .create_conversation(first.checkout_id.clone(), "Another destination".into())
        .await?;
    let foreign = store
        .create_conversation(other.checkout_id, "Foreign".into())
        .await?;
    let (context, selected, hidden, hidden_context) = store
        .execute({
            let id = source.id.clone();
            move |db| {
                let selected = insert_message(
                    db,
                    &id,
                    MessageRole::User,
                    protection::protect("café 🦀".as_bytes())?,
                )?;
                let hidden = insert_message(
                    db,
                    &id,
                    MessageRole::Assistant,
                    protection::protect(b"hidden event canary")?,
                )?;
                let hidden_context = write_context(db, &id, std::slice::from_ref(&hidden), false)?;
                let context = activate_context(db, &id, std::slice::from_ref(&selected))?;
                Ok((context, selected, hidden, hidden_context))
            }
        })
        .await?;

    for disallowed in [&foreign.id, &source.id] {
        assert!(
            store
                .conversation_reference_source(disallowed.clone(), source.id.clone(), None)
                .await
                .is_err()
        );
    }
    let available = store
        .conversation_reference_source(destination.id.clone(), source.id.clone(), None)
        .await?;
    assert_eq!(available.project_id, first.project_id);
    assert_eq!(available.project_name, "first");
    assert_eq!(available.retained_context_ids, [context.clone()]);
    assert_eq!(available.messages[0].revision_id, selected);
    assert_eq!(available.messages.len(), 1);
    assert!(available.summaries.is_empty());
    let selection =
        |context_id: String, revision_id: String, range| ConversationReferenceSelection::Messages {
            context_id,
            spans: vec![SelectedMessageSpan {
                revision_id,
                byte_range: range,
            }],
        };
    assert!(
        store
            .preview_conversation_reference(
                foreign.id.clone(),
                source.id.clone(),
                selection(context.clone(), selected.clone(), None)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .preview_conversation_reference(
                destination.id.clone(),
                source.id.clone(),
                selection(context.clone(), hidden.clone(), None)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .preview_conversation_reference(
                destination.id.clone(),
                source.id.clone(),
                selection(hidden_context, hidden, None)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .preview_conversation_reference(
                destination.id.clone(),
                source.id.clone(),
                selection(
                    context.clone(),
                    selected.clone(),
                    Some(ByteRange { start: 4, end: 5 })
                )
            )
            .await
            .is_err()
    );

    let preview = store
        .preview_conversation_reference(
            destination.id.clone(),
            source.id.clone(),
            selection(
                context.clone(),
                selected.clone(),
                Some(ByteRange { start: 3, end: 5 }),
            ),
        )
        .await?;
    assert_eq!(preview.source_title, "Original source");
    assert!(
        matches!(&preview.artifact, ConversationArtifactRevision::Excerpt { source_context_id, excerpts, .. }
        if source_context_id == &context && excerpts.len() == 1 && excerpts[0].text == "é"
          && excerpts[0].source.origin.revision_id == selected && excerpts[0].source.origin.checkout_id == first.checkout_id)
    );
    store
        .rewind_conversation_history(source.id.clone(), context.clone(), None)
        .await?;
    let retained = store
        .conversation_reference_source(
            destination.id.clone(),
            source.id.clone(),
            Some(context.clone()),
        )
        .await?;
    assert_eq!(retained.messages[0].revision_id, selected);
    assert!(retained.retained_context_ids.contains(&context));

    drop(store);
    let reopened = Store::open(database)?;
    reopened.execute(move |db| {
        let recorded = load_conversation_reference(db, &destination.id, &first.project_id, &preview.id)?;
        assert_eq!(recorded, preview);
        assert!(load_conversation_reference(db, &other_destination.id, &first.project_id, &preview.id).is_err());
        assert!(load_conversation_reference(db, &destination.id, &other.project_id, &preview.id).is_err());
        let mut tampered = recorded.clone();
        if let ConversationArtifactRevision::Excerpt { excerpts, .. } = &mut tampered.artifact {
            excerpts[0].text = "fake replacement".into();
        }
        assert!(recheck_conversation_reference(db, &destination.id, &first.project_id, &tampered).is_err());
        recheck_conversation_reference(db, &destination.id, &first.project_id, &recorded)?;
        recheck_conversation_reference(db, &other_destination.id, &first.project_id, &recorded)?;
        assert!(recheck_conversation_reference(db, &foreign.id, &other.project_id, &recorded).is_err());
        db.execute("UPDATE conversations SET checkout_id = (SELECT checkout_id FROM conversations WHERE id = ?2) WHERE id = ?1",
            params![source.id, foreign.id])?;
        assert!(load_conversation_reference(db, &destination.id, &first.project_id, &preview.id).is_err());
        Ok(())
    }).await
}

async fn init(store: &Store, root: &Path) -> Result<ProjectCheckout> {
    fs::create_dir(root)?;
    git(root, &["init", "--quiet", "--initial-branch=main"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "--quiet",
            "--allow-empty",
            "-m",
            "Initial",
        ],
    )?;
    repository::register(store, root.to_owned()).await
}

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git.exe")
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
