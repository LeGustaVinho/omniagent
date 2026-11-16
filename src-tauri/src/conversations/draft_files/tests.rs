use super::*;
use crate::conversations::tests::setup;

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
