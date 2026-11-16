use std::{fs, path::Path, process::Command};

use super::*;
use crate::{
    attachments::{
        AttachmentDelivery, AttachmentFormat, AttachmentMetadata, AttachmentSource,
        CapturedAttachment, CapturedPayload,
    },
    conversations::{
        ContinuationChoice, ContinuationInspection, MessageRole, SessionScope, activate_context,
        continuation::ContinuationReason, draft_files::DraftAttachmentEdit, insert_message,
    },
    jobs::AdmissionEnvironment,
    providers::{Provider, SessionIntent, SessionMode},
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

#[tokio::test]
async fn explicit_checkout_changes_review_the_seed_and_admit_each_choice_in_the_destination()
-> Result<()> {
    let scratch = tempfile::tempdir()?;
    let database = scratch.path().join("switch.sqlite");
    let mut store = Store::open(database.clone())?;
    let original = init(&store, &scratch.path().join("original")).await?;
    let linked_path = scratch.path().join("linked");
    git(
        Path::new(&original.checkout_root),
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
    let foreign = init(&store, &scratch.path().join("foreign")).await?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "fixture".into(),
        account_binding: None,
        checkout: PathBuf::from(&linked.checkout_root),
        executable: std::env::current_exe()?,
        cli_version: "1".into(),
        runner: "fixture-runner".into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("test".into()),
        effort: None,
    };
    for choice in [
        ContinuationChoice::SameTab,
        ContinuationChoice::ForkWithContext,
        ContinuationChoice::Fresh,
    ] {
        let source = store
            .create_conversation(original.checkout_id.clone(), "Source".into())
            .await?;
        let context_id = store
            .execute({
                let id = source.id.clone();
                let historical_reference = (choice == ContinuationChoice::Fresh)
                    .then(|| Path::new(&original.checkout_root).join("tracked.txt"));
                move |db| {
                    let revision = insert_message(
                        db,
                        &id,
                        MessageRole::User,
                        protection::protect(b"Reviewed source history")?,
                    )?;
                    if let Some(path) = historical_reference {
                        let bytes = fs::read(&path)?;
                        use sha2::{Digest, Sha256};
                        let attachment = CapturedAttachment {
                            metadata: AttachmentMetadata {
                                source: AttachmentSource::Checkout {
                                    relative_path: "tracked.txt".into(),
                                },
                                size: bytes.len() as u64,
                                format: AttachmentFormat::Text,
                                sha256: format!("{:x}", Sha256::digest(bytes)),
                            },
                            payload: CapturedPayload::Reference {
                                reference_path: path,
                            },
                        };
                        db.execute(
                            "INSERT INTO message_attachments VALUES (?1, ?2)",
                            params![
                                revision,
                                protection::protect(&serde_json::to_vec(&[attachment])?)?
                            ],
                        )?;
                    }
                    activate_context(db, &id, &[revision])
                }
            })
            .await?;
        assert!(
            store
                .conversation_continuation_checkout_id(
                    source.id.clone(),
                    Some(foreign.checkout_id.clone())
                )
                .await
                .is_err()
        );
        let choices = store
            .conversation_continuation_checkouts(source.id.clone())
            .await?;
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].checkout_id, original.checkout_id);
        assert_eq!(choices[1].checkout_id, linked.checkout_id);
        store
            .save_conversation_draft(source.id.clone(), "Draft in the new checkout".into())
            .await?;
        let attachment_preview = if choice == ContinuationChoice::ForkWithContext {
            let path = Path::new(&original.checkout_root).join("tracked.txt");
            fs::write(&path, "old scope")?;
            fs::write(scope.checkout.join("tracked.txt"), "new scope")?;
            let draft = store
                .add_conversation_attachment_paths(source.id.clone(), vec![path])
                .await?;
            let draft = store
                .edit_conversation_attachment_draft(
                    source.id.clone(),
                    draft.revision.unwrap(),
                    draft.files[0].id.clone(),
                    DraftAttachmentEdit::SetDelivery {
                        delivery: Some(AttachmentDelivery::Excerpt { start: 0, end: 3 }),
                    },
                )
                .await?;
            let mut old_scope = scope.clone();
            old_scope.checkout = PathBuf::from(&original.checkout_root);
            let prior = store
                .review_conversation_attachment_draft(
                    source.id.clone(),
                    draft.revision.clone().unwrap(),
                    old_scope,
                    "Draft in the new checkout".into(),
                )
                .await?;
            assert_eq!(prior.files[0].excerpt.as_ref().unwrap().text, "old");
            assert!(
                store
                    .submit_conversation_with_files_at_checkout(
                        source.id.clone(),
                        context_id.clone(),
                        scope.clone(),
                        "Draft in the new checkout".into(),
                        Vec::new(),
                        prior.preview_id,
                        Some(linked.checkout_id.clone())
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .conversation_attachment_draft(source.id.clone())
                    .await?,
                draft
            );
            let target_review = store
                .review_conversation_attachment_draft_at_checkout(
                    source.id.clone(),
                    draft.revision.unwrap(),
                    scope.clone(),
                    "Draft in the new checkout".into(),
                    Some(linked.checkout_id.clone()),
                )
                .await?;
            assert_eq!(target_review.files[0].excerpt.as_ref().unwrap().text, "new");
            Some(
                target_review
                    .preview_id
                    .context("missing destination attachment preview")?,
            )
        } else {
            None
        };
        let review = store
            .submit_conversation_with_files_at_checkout(
                source.id.clone(),
                context_id,
                scope.clone(),
                "Draft in the new checkout".into(),
                Vec::new(),
                attachment_preview.clone(),
                Some(linked.checkout_id.clone()),
            )
            .await?;
        let ContinuationInspection::ReviewRequired {
            preview_id,
            reason: ContinuationReason::CheckoutChanged,
            target_checkout: Some(target),
            seed,
            ..
        } = review
        else {
            anyhow::bail!("checkout change did not offer review")
        };
        assert_eq!(target, linked);
        assert_eq!(seed.len(), 1);
        assert_eq!(
            !store
                .conversation_attachment_draft(source.id.clone())
                .await?
                .files
                .is_empty(),
            attachment_preview.is_some()
        );
        assert_eq!(
            store.conversation(source.id.clone()).await?.checkout_id,
            original.checkout_id
        );
        drop(store);
        store = Store::open(database.clone())?;
        assert_eq!(
            store
                .conversation_continuation_review_checkout(source.id.clone(), preview_id.clone())
                .await?,
            linked.checkout_id
        );
        if choice == ContinuationChoice::Fresh {
            let failed = store
                .confirm_conversation_continuation(
                    source.id.clone(),
                    preview_id.clone(),
                    ContinuationChoice::SameTab,
                    scope.clone(),
                    "Draft in the new checkout".into(),
                )
                .await;
            let Err(error) = failed else {
                anyhow::bail!("the old checkout reference should prevent carrying the saved seed")
            };
            assert!(error.to_string().contains("Choose a fresh conversation"));
            assert_eq!(
                store.conversation_draft(source.id.clone()).await?,
                "Draft in the new checkout"
            );
            assert_eq!(
                store.conversation(source.id.clone()).await?.checkout_id,
                original.checkout_id
            );
        }
        let resolved = store
            .confirm_conversation_continuation(
                source.id.clone(),
                preview_id,
                choice,
                scope.clone(),
                "Draft in the new checkout".into(),
            )
            .await?;
        assert_eq!(resolved.job.checkout_id, linked.checkout_id);
        assert_eq!(resolved.conversation.checkout_id, linked.checkout_id);
        assert_eq!(
            resolved.conversation.id == source.id,
            choice == ContinuationChoice::SameTab
        );
        assert_eq!(
            store.conversation(source.id.clone()).await?.checkout_id,
            if choice == ContinuationChoice::SameTab {
                &linked.checkout_id
            } else {
                &original.checkout_id
            }
            .as_str()
        );
        let plan = store
            .claim_next_job(AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: false,
            })
            .await?
            .context("missing continuation attempt")?;
        assert_eq!(plan.run.checkout_id, linked.checkout_id);
        assert!(matches!(plan.intent, SessionIntent::Start));
        assert_eq!(plan.input.last().unwrap().text, "Draft in the new checkout");
        assert!(
            store
                .conversation_attachment_draft(source.id)
                .await?
                .files
                .is_empty()
        );
        if attachment_preview.is_some() {
            let [(_, attachment)] = plan.attachments.as_slice() else {
                anyhow::bail!("missing exact reviewed attachment")
            };
            assert!(
                matches!(&attachment.payload, CapturedPayload::Excerpt { excerpt } if excerpt.text == "new")
            );
        } else {
            assert!(plan.attachments.is_empty());
        }
        assert_eq!(
            plan.input
                .iter()
                .any(|message| message.text == "Reviewed source history"),
            choice != ContinuationChoice::Fresh
        );
        store.mark_agent_running(plan.run.id.clone()).await?;
        store
            .finish_agent_attempt(plan.run.id, crate::runs::RunStatus::Succeeded, None)
            .await?;
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git.exe")
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim_end().to_owned())
}
