use std::{fs, path::Path, process::Command};

use super::*;
use crate::{
    conversations::{MessageRole, activate_context, insert_message},
    repository::{
        self, ProjectCheckout,
        writers::{WriterConflict, writer_conflict},
    },
};

#[tokio::test]
async fn all_checkout_policies_keep_source_changes_and_bind_only_reviewed_fork_files() -> Result<()>
{
    let scratch = tempfile::tempdir()?;
    let database = scratch.path().join("forks.sqlite");
    let store = Store::open(database.clone())?;
    let initial = init(&store, &scratch.path().join("repo")).await?;
    let worktrees = scratch.path().join("worktrees");
    let task = store
        .create_project_task(initial.project_id.clone(), "Writer owner".into())
        .await?;
    let original = repository::provision_task_worktree(
        &store,
        worktrees.clone(),
        initial.project_id.clone(),
        task.id.clone(),
        initial.checkout_id,
    )
    .await?;
    let source = store
        .create_conversation(original.checkout_id.clone(), "Source".into())
        .await?;
    let (context, first) = store
        .execute({
            let id = source.id.clone();
            move |db| {
                let first = insert_message(
                    db,
                    &id,
                    MessageRole::User,
                    protection::protect(b"first revision")?,
                )?;
                let second = insert_message(
                    db,
                    &id,
                    MessageRole::Assistant,
                    protection::protect(b"later response")?,
                )?;
                let context = activate_context(db, &id, &[first.clone(), second])?;
                Ok((context, first))
            }
        })
        .await?;
    let request = |title: &str, preview_id: Option<String>| ForkConversationRequest {
        conversation_id: source.id.clone(),
        expected_context_id: context.clone(),
        source_context_id: None,
        through_revision: None,
        title: title.into(),
        edited_text: None,
        checkout_preview_id: preview_id,
    };
    let default = store
        .conversation_fork_checkout_settings(source.id.clone())
        .await?;
    assert_eq!(default.effective, ForkCheckoutPolicy::ReuseCurrentCheckout);
    let reused = store
        .fork_conversation_with_checkout(request("Reuse heading", None))
        .await?;
    assert_eq!(reused.checkout_id, original.checkout_id);
    assert_eq!(
        store
            .conversation_history(reused.id.clone())
            .await?
            .messages
            .len(),
        2
    );

    store
        .set_conversation_fork_checkout_policy(
            source.id.clone(),
            ContinuationLevel::Project,
            Some(ForkCheckoutPolicy::AskForCheckout),
        )
        .await?;
    let ask = store
        .preview_conversation_fork_checkout(
            source.id.clone(),
            context.clone(),
            None,
            Vec::new(),
            worktrees.clone(),
        )
        .await?;
    assert!(ask.preview_id.is_none());
    assert!(ask.choice.is_none());
    assert!(
        store
            .fork_conversation_with_checkout(request("Implicit ask", None))
            .await
            .is_err()
    );
    let explicit = store
        .preview_conversation_fork_checkout(
            source.id.clone(),
            context.clone(),
            Some(ForkCheckoutChoice::ReuseCurrentCheckout),
            Vec::new(),
            worktrees.clone(),
        )
        .await?;
    let answered = store
        .fork_conversation_with_checkout(request("Asked reuse", explicit.preview_id))
        .await?;
    assert_eq!(answered.checkout_id, original.checkout_id);

    let override_value = store
        .set_conversation_fork_checkout_policy(
            source.id.clone(),
            ContinuationLevel::Conversation,
            Some(ForkCheckoutPolicy::CreateSeparateWorktree),
        )
        .await?;
    assert_eq!(
        override_value.project_default,
        ForkCheckoutPolicy::AskForCheckout
    );
    assert_eq!(
        override_value.effective,
        ForkCheckoutPolicy::CreateSeparateWorktree
    );
    fs::write(original.root.join("tracked.txt"), b"included raw\r\n")?;
    git(&original.root, &["add", "--", "tracked.txt"])?;
    fs::write(original.root.join("added.txt"), b"included addition")?;
    fs::write(original.root.join("skip.txt"), b"not included")?;
    fs::remove_file(original.root.join("remove.txt"))?;
    let selection = vec![
        "tracked.txt".into(),
        "added.txt".into(),
        "remove.txt".into(),
    ];
    let reviewed = store
        .preview_conversation_fork_checkout(
            source.id.clone(),
            context.clone(),
            None,
            selection.clone(),
            worktrees.clone(),
        )
        .await?;
    assert_eq!(reviewed.selected_paths, selection);
    assert!(reviewed.dirty_paths.contains(&"skip.txt".to_owned()));
    let root = reviewed
        .target_checkout_root
        .context("missing planned fork root")?;
    let branch = reviewed
        .target_branch
        .context("missing planned fork branch")?;
    let base = reviewed.base_commit.context("missing base")?;
    assert!(!root.exists());
    let before = git(&original.root, &["status", "--porcelain"])?;
    let mut message_fork = request("Separate edited message", reviewed.preview_id.clone());
    message_fork.through_revision = Some(first);
    message_fork.edited_text = Some("edited in fork only".into());
    drop(store);
    let store = Store::open(database)?;
    let separated = store.fork_conversation_with_checkout(message_fork).await?;
    assert_ne!(separated.checkout_id, original.checkout_id);
    assert_eq!(git(&root, &["branch", "--show-current"])?, branch);
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, base);
    assert_eq!(git(&original.root, &["rev-parse", "HEAD"])?, base);
    assert_eq!(git(&original.root, &["status", "--porcelain"])?, before);
    assert_eq!(fs::read(root.join("tracked.txt"))?, b"included raw\r\n");
    assert_eq!(fs::read(root.join("added.txt"))?, b"included addition");
    assert!(!root.join("remove.txt").exists());
    assert!(!root.join("skip.txt").exists());
    assert_eq!(
        store
            .conversation_history(source.id.clone())
            .await?
            .messages[0]
            .text,
        "first revision"
    );
    let fork_history = store.conversation_history(separated.id.clone()).await?;
    assert_eq!(fork_history.messages.len(), 1);
    assert_eq!(fork_history.messages[0].text, "edited in fork only");
    assert!(
        store
            .fork_conversation_with_checkout(request("Duplicate", reviewed.preview_id))
            .await
            .is_err()
    );

    let stale = store
        .preview_conversation_fork_checkout(
            source.id.clone(),
            context.clone(),
            None,
            vec!["added.txt".into()],
            worktrees.clone(),
        )
        .await?;
    fs::write(
        original.root.join("added.txt"),
        b"independent newer content",
    )?;
    assert!(
        store
            .fork_conversation_with_checkout(request("Stale", stale.preview_id))
            .await
            .is_err()
    );
    assert!(!stale.target_checkout_root.unwrap().exists());
    assert_eq!(
        fs::read(original.root.join("added.txt"))?,
        b"independent newer content"
    );

    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO app_worktree_writers VALUES (?1,'shared-writer',?2,?3)",
                params![original.checkout_id, original.project_id, task.id],
            )?;
            assert!(matches!(
                writer_conflict(db, &original.checkout_id, &task.id)?,
                Some(WriterConflict::App(_))
            ));
            assert!(matches!(
                writer_conflict(db, &reused.checkout_id, &task.id)?,
                Some(WriterConflict::App(_))
            ));
            assert!(matches!(
                writer_conflict(db, &answered.checkout_id, &task.id)?,
                Some(WriterConflict::App(_))
            ));
            assert!(writer_conflict(db, &separated.checkout_id, &task.id)?.is_none());
            Ok(())
        })
        .await
}

async fn init(store: &Store, root: &Path) -> Result<ProjectCheckout> {
    fs::create_dir(root)?;
    git(root, &["init", "--quiet", "--initial-branch=main"])?;
    git(root, &["config", "core.autocrlf", "false"])?;
    fs::write(root.join("tracked.txt"), b"base\n")?;
    fs::write(root.join("remove.txt"), b"remove in fork\n")?;
    git(root, &["add", "--", "."])?;
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
            "-m",
            "Base",
        ],
    )?;
    repository::register(store, root.to_owned()).await
}
