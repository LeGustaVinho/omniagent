use super::*;
use crate::conversations::tests::setup;
use crate::{
    attachments::CapturedPayload, conversations::ContinuationInspection, jobs::AdmissionEnvironment,
};

#[tokio::test]
async fn checkout_context_is_scoped_and_preserves_the_existing_draft() -> Result<()> {
    let (_repo, store, chat, scope) = setup().await?;
    std::fs::write(scope.checkout.join("source.txt"), "context")?;
    store
        .save_conversation_draft(chat.id.clone(), "Unsent text".into())
        .await?;

    assert!(
        store
            .add_conversation_checkout_paths(
                chat.id.clone(),
                "other checkout".into(),
                vec!["source.txt".into()]
            )
            .await
            .is_err()
    );
    assert!(
        store
            .add_conversation_checkout_paths(
                chat.id.clone(),
                chat.checkout_id.clone(),
                vec!["source.txt".into(), "../outside.txt".into()]
            )
            .await
            .is_err()
    );
    assert!(
        store
            .conversation_attachment_draft(chat.id.clone())
            .await?
            .files
            .is_empty()
    );

    let paths = vec!["source.txt".to_owned()];
    let added = store
        .add_conversation_checkout_paths(chat.id.clone(), chat.checkout_id.clone(), paths.clone())
        .await?;
    assert_eq!(added.files.len(), 1);
    assert!(matches!(
        added.files[0].source,
        AttachmentSource::Checkout { .. }
    ));
    assert_eq!(
        store
            .add_conversation_checkout_paths(chat.id.clone(), chat.checkout_id, paths)
            .await?,
        added
    );
    assert_eq!(store.conversation_draft(chat.id).await?, "Unsent text");
    Ok(())
}

#[tokio::test]
async fn native_selections_keep_individual_sources_and_restore_the_attachment_draft() -> Result<()>
{
    let (_repo, store, chat, scope) = setup().await?;
    let external_dir = tempfile::tempdir()?;
    let internal = scope.checkout.join("inside.txt");
    let external = dunce::canonicalize(external_dir.path())?.join("outside.txt");
    std::fs::write(&internal, "checkout")?;
    std::fs::write(&external, "external")?;
    store
        .save_conversation_draft(chat.id.clone(), "Unsent text".into())
        .await?;
    assert!(
        store
            .add_conversation_attachment_paths(chat.id.clone(), vec![scope.checkout.clone()])
            .await
            .is_err()
    );
    let selected = store
        .add_conversation_attachment_paths(
            chat.id.clone(),
            vec![internal.clone(), external.clone()],
        )
        .await?;
    assert_eq!(
        selected.files[0].source,
        AttachmentSource::Checkout {
            relative_path: "inside.txt".into()
        }
    );
    assert_eq!(
        selected.files[1].source,
        AttachmentSource::SelectedFile { path: external }
    );
    assert_eq!(
        store
            .add_conversation_attachment_paths(chat.id.clone(), vec![internal])
            .await?,
        selected
    );
    let other = store
        .create_conversation(chat.checkout_id, "Other draft".into())
        .await?;
    assert!(
        store
            .conversation_attachment_draft(other.id)
            .await?
            .files
            .is_empty()
    );
    let revision = selected
        .revision
        .clone()
        .context("Missing draft revision")?;
    assert!(
        store
            .edit_conversation_attachment_draft(
                chat.id.clone(),
                "stale".into(),
                selected.files[0].id.clone(),
                DraftAttachmentEdit::Remove
            )
            .await
            .is_err()
    );
    let db_path = scope.checkout.join("store.sqlite");
    drop(store);
    let reopened = Store::open(db_path)?;
    assert_eq!(
        reopened
            .conversation_attachment_draft(chat.id.clone())
            .await?,
        selected
    );
    assert_eq!(
        reopened.conversation_draft(chat.id.clone()).await?,
        "Unsent text"
    );
    let remaining = reopened
        .edit_conversation_attachment_draft(
            chat.id,
            revision,
            selected.files[0].id.clone(),
            DraftAttachmentEdit::Remove,
        )
        .await?;
    assert_eq!(remaining.files, [selected.files[1].clone()]);
    assert_ne!(remaining.revision, selected.revision);
    Ok(())
}

#[tokio::test]
async fn attachment_review_preserves_the_exact_excerpt_and_rejects_changes_before_queueing()
-> Result<()> {
    let (_repo, store, chat, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let file = scope.checkout.join("excerpt.txt");
    let original = "Keep café\nIgnore this line.";
    std::fs::write(&file, original)?;
    let draft = store
        .add_conversation_attachment_paths(chat.id.clone(), vec![file.clone()])
        .await?;
    let draft = store
        .edit_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.unwrap(),
            draft.files[0].id.clone(),
            DraftAttachmentEdit::SetDelivery {
                delivery: Some(AttachmentDelivery::Excerpt {
                    start: 0,
                    end: "Keep café".len() as u64,
                }),
            },
        )
        .await?;
    let prompt = "Report the selected excerpt.";
    store
        .save_conversation_draft(chat.id.clone(), prompt.into())
        .await?;
    let review = store
        .review_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.clone().unwrap(),
            scope.clone(),
            prompt.into(),
        )
        .await?;
    let preview = review.preview_id.context("Missing reviewed excerpt")?;
    let row = &review.files[0];
    assert_eq!(
        row.excerpt.as_ref().context("Missing preview text")?.text,
        "Keep café"
    );
    assert_eq!(row.metadata.as_ref().unwrap().size, original.len() as u64);
    assert!(row.alternatives.contains(&AttachmentDelivery::Reference));
    let submit = |scope, preview| {
        store.submit_conversation_with_files(
            chat.id.clone(),
            chat.context_id.clone(),
            scope,
            prompt.into(),
            Vec::new(),
            Some(preview),
        )
    };
    let mut changed = scope.clone();
    changed.model = Some("new-model".into());
    assert!(submit(changed, preview.clone()).await.is_err());
    std::fs::write(&file, "Changed at the same path")?;
    assert!(submit(scope.clone(), preview.clone()).await.is_err());
    assert_eq!(
        store.conversation_attachment_draft(chat.id.clone()).await?,
        draft
    );
    assert_eq!(store.conversation_draft(chat.id.clone()).await?, prompt);
    std::fs::write(&file, original)?;
    assert!(matches!(
        submit(scope.clone(), preview).await?,
        ContinuationInspection::Queued { .. }
    ));
    assert!(
        store
            .conversation_attachment_draft(chat.id.clone())
            .await?
            .files
            .is_empty()
    );
    std::fs::write(file, "Changed after queue admission")?;
    let plan = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner,
            bridge_available: false,
        })
        .await?
        .context("Reviewed turn was not admitted")?;
    let [(_, saved)] = plan.attachments.as_slice() else {
        anyhow::bail!("Expected exactly one attachment")
    };
    assert_eq!(saved.metadata, *row.metadata.as_ref().unwrap());
    assert!(
        matches!(&saved.payload, CapturedPayload::Excerpt { excerpt } if Some(excerpt) == row.excerpt.as_ref())
    );
    Ok(())
}
