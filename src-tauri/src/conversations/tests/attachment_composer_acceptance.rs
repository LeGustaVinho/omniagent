use super::*;

#[tokio::test]
async fn attachment_composer_review_matches_saved_dispatch_after_profile_change() -> Result<()> {
    use crate::attachments::{
        AttachmentDelivery, AttachmentFormat, AttachmentInput, AttachmentLimits, AttachmentSource,
        NativeAttachmentEncoding,
    };
    use crate::conversations::draft_files::{
        DraftAttachmentEdit, ReuseSavedConversationAttachment,
    };
    use crate::dispatch::{AttachmentHistoryRemoval, AttachmentHistoryTarget};
    use crate::jobs::AdmissionEnvironment;
    use base64::{Engine, engine::general_purpose::STANDARD};

    struct AttachmentRunner(FixtureRunner);

    impl Runner for AttachmentRunner {
        fn identity(&self) -> &str {
            DirectWindowsRunner.identity()
        }

        fn execution_platform(&self) -> Option<ExecutionPlatform> {
            self.0.execution_platform()
        }

        fn executable_revision(&self, path: &Path) -> Option<String> {
            DirectWindowsRunner.executable_revision(path)
        }

        fn attachment_path(&self, path: &Path) -> Option<PathBuf> {
            self.0.attachment_path(path)
        }

        fn attachment_staging(
            &self,
            scope: &crate::runner::AttachmentRunScope<'_>,
        ) -> io::Result<Box<dyn crate::runner::AttachmentStaging>> {
            self.0.attachment_staging(scope)
        }

        fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
            self.0.launch(request)
        }
    }

    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ9sAAAAASUVORK5CYII=";
    let (_checkout, store, chat, mut native_scope) = setup().await?;
    native_scope.runner = DirectWindowsRunner.identity().into();
    std::fs::write(&native_scope.executable, b"Codex wire fixture")?;
    let runner_for = |conversation: &str| {
        Arc::new(AttachmentRunner(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.into(),
            frames: Arc::default(),
        })))
    };
    let runner = runner_for(&chat.id);
    let baseline = prepare(&store, &chat.id, native_scope.clone(), "Establish profile").await?;
    let baseline_run = baseline.run.id.clone();
    assert_eq!(
        start_managed_turn(store.clone(), runner.clone(), baseline, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    let assessment = store
        .agent_dispatch(chat.id.clone(), baseline_run)
        .await?
        .context("Baseline dispatch missing")?
        .snapshot
        .capabilities
        .context("Baseline assessment missing")?;
    let descriptor = assessment
        .entries
        .into_iter()
        .find(|entry| {
            entry.descriptor.subject
                == CapabilitySubject::NativeAttachment {
                    format: AttachmentFormat::Png,
                    encoding: NativeAttachmentEncoding::CodexLocalImage,
                }
        })
        .context("Codex image descriptor missing")?
        .descriptor;
    let qualification = store
        .record_capability_evidence(
            assessment.scope,
            descriptor,
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Succeeded,
            "simulated-attachment-composer-profile".into(),
        )
        .await?;

    let image = native_scope.checkout.join("image.png");
    std::fs::write(&image, STANDARD.decode(PNG)?)?;
    let external_dir = tempfile::tempdir()?;
    let external = dunce::canonicalize(external_dir.path())?.join("selected.txt");
    let sibling = external.with_file_name("private.txt");
    std::fs::write(&external, "café\nThe remainder stays local.")?;
    std::fs::write(&sibling, "Unselected external file")?;
    let draft = store
        .add_conversation_attachment_paths(chat.id.clone(), vec![image.clone(), external.clone()])
        .await?;
    let image_id = draft.files[0].id.clone();
    let text_id = draft.files[1].id.clone();
    let draft = store
        .edit_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.context("Missing initial draft")?,
            text_id.clone(),
            DraftAttachmentEdit::Remove,
        )
        .await?;
    assert_eq!(draft.files.len(), 1);
    let native_prompt = "Identify the image.";
    store
        .save_conversation_draft(chat.id.clone(), native_prompt.into())
        .await?;
    let native = store
        .review_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.clone().unwrap(),
            native_scope.clone(),
            native_prompt.into(),
        )
        .await?;
    assert_eq!(native.files[0].delivery, Some(AttachmentDelivery::Native));
    assert_eq!(native.files[0].media.as_deref(), Some(PNG));
    let submit = |context, scope, text: &str, preview| {
        store.submit_conversation_with_files(
            chat.id.clone(),
            context,
            scope,
            text.into(),
            Vec::new(),
            Some(preview),
        )
    };
    let admit = |scope: &SessionScope| {
        store.claim_next_job(AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        })
    };
    let mut fallback_scope = native_scope.clone();
    fallback_scope.profile_id = "another-profile".into();
    let context = store.conversation(chat.id.clone()).await?.context_id;
    let preview = native
        .preview_id
        .context("Expected a native review receipt")?;
    assert!(
        submit(
            context.clone(),
            fallback_scope.clone(),
            native_prompt,
            preview.clone()
        )
        .await
        .is_err()
    );
    assert!(matches!(
        submit(context, native_scope.clone(), native_prompt, preview).await?,
        ContinuationInspection::Queued { .. }
    ));
    let plan = admit(&native_scope)
        .await?
        .context("Native submission did not reach the queue")?;
    let run = plan.run.id.clone();
    assert_eq!(
        start_managed_turn(store.clone(), runner.clone(), plan, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    let dispatch = store
        .agent_dispatch(chat.id.clone(), run.clone())
        .await?
        .context("Native dispatch missing")?;
    let [sent] = dispatch.snapshot.attachments.as_slice() else {
        anyhow::bail!("Expected one native attachment")
    };
    assert_eq!(Some(&sent.metadata), native.files[0].metadata.as_ref());
    assert_eq!(
        sent.qualification_id.as_deref(),
        Some(qualification.as_str())
    );
    assert_eq!(sent.input.native().unwrap().content, PNG);
    let source_run = run;
    let origin = dispatch.attachments[0].clone();

    let draft = store
        .add_conversation_attachment_paths(chat.id.clone(), vec![image.clone(), external.clone()])
        .await?;
    assert_ne!(draft.files[0].id, image_id);
    assert_ne!(draft.files[1].id, text_id);
    let draft = store
        .edit_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.unwrap(),
            draft.files[0].id.clone(),
            DraftAttachmentEdit::SetDelivery {
                delivery: Some(AttachmentDelivery::Native),
            },
        )
        .await?;
    let fallback_prompt = "Read the exact excerpt and follow the reviewed reference.";
    store
        .save_conversation_draft(chat.id.clone(), fallback_prompt.into())
        .await?;
    let blocked = store
        .review_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.clone().unwrap(),
            fallback_scope.clone(),
            fallback_prompt.into(),
        )
        .await?;
    assert!(blocked.preview_id.is_none());
    assert!(blocked.files[0].blocker.is_some());
    assert!(
        blocked.files[0]
            .alternatives
            .contains(&AttachmentDelivery::Reference)
    );
    let draft = store
        .edit_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.unwrap(),
            draft.files[0].id.clone(),
            DraftAttachmentEdit::SetDelivery {
                delivery: Some(AttachmentDelivery::Reference),
            },
        )
        .await?;
    let draft = store
        .edit_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.unwrap(),
            draft.files[1].id.clone(),
            DraftAttachmentEdit::SetDelivery {
                delivery: Some(AttachmentDelivery::Excerpt {
                    start: 0,
                    end: "café".len() as u64,
                }),
            },
        )
        .await?;
    let fallback = store
        .review_conversation_attachment_draft(
            chat.id.clone(),
            draft.revision.unwrap(),
            fallback_scope.clone(),
            fallback_prompt.into(),
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&fallback.files[0])?["reference"],
        json!({"kind":"checkout_path","path":image})
    );
    assert_eq!(fallback.files[1].excerpt.as_ref().unwrap().text, "café");
    let context = store.conversation(chat.id.clone()).await?.context_id;
    let ContinuationInspection::ReviewRequired { preview_id, .. } = submit(
        context,
        fallback_scope.clone(),
        fallback_prompt,
        fallback
            .preview_id
            .context("Expected a fallback review receipt")?,
    )
    .await?
    else {
        anyhow::bail!("The profile switch must require continuation review");
    };
    let continued = store
        .confirm_conversation_continuation(
            chat.id.clone(),
            preview_id,
            ContinuationChoice::Fresh,
            fallback_scope.clone(),
            fallback_prompt.into(),
        )
        .await?;
    assert_ne!(continued.conversation.id, chat.id);
    let plan = admit(&fallback_scope)
        .await?
        .context("Fallback submission did not reach the queue")?;
    let run = plan.run.id.clone();
    assert_eq!(
        start_managed_turn(
            store.clone(),
            runner_for(&continued.conversation.id),
            plan,
            None
        )
        .wait()
        .await?,
        RunStatus::Succeeded
    );
    let dispatch = store
        .agent_dispatch(continued.conversation.id, run)
        .await?
        .context("Fallback dispatch missing")?;
    assert_eq!(
        dispatch.snapshot.scope.profile_id,
        fallback_scope.profile_id
    );
    let [reference, excerpt] = dispatch.snapshot.attachments.as_slice() else {
        anyhow::bail!("Expected the two reviewed files")
    };
    assert_eq!(
        Some(&reference.metadata),
        fallback.files[0].metadata.as_ref()
    );
    assert_eq!(reference.input.path(), Some(image.as_path()));
    assert_eq!(Some(&excerpt.metadata), fallback.files[1].metadata.as_ref());
    assert!(
        matches!(&excerpt.input, AttachmentInput::Excerpt(value) if Some(value) == fallback.files[1].excerpt.as_ref())
    );
    let frame: Value = serde_json::from_slice(&dispatch.frames[0])?;
    let wire = frame["params"]["input"].to_string();
    assert!(wire.contains("café"));
    assert!(!wire.contains("The remainder stays local") && !wire.contains("private.txt"));
    assert_eq!(
        std::fs::read_to_string(sibling)?,
        "Unselected external file"
    );

    std::fs::remove_file(&image)?;
    let destination = store
        .create_conversation(chat.checkout_id.clone(), "Reuse the recorded image".into())
        .await?;
    let reuse = |destination: &str, sha256: String| ReuseSavedConversationAttachment {
        source_conversation_id: chat.id.clone(),
        run_id: source_run.clone(),
        ordinal: origin.ordinal,
        message_revision_id: origin.message_revision_id.clone(),
        source_sha256: sha256,
        destination_conversation_id: destination.into(),
    };
    assert!(
        store
            .reuse_saved_conversation_attachment(reuse(&destination.id, "wrong".into()))
            .await
            .is_err()
    );
    let unrelated_root = tempfile::tempdir()?;
    let unrelated_root = dunce::canonicalize(unrelated_root.path())?;
    assert!(
        Command::new("git.exe")
            .current_dir(&unrelated_root)
            .args(["init", "--quiet"])
            .status()?
            .success()
    );
    let unrelated = crate::repository::register(&store, unrelated_root).await?;
    let unrelated = store
        .create_conversation(unrelated.checkout_id, "Unrelated project".into())
        .await?;
    assert!(
        store
            .reuse_saved_conversation_attachment(reuse(&unrelated.id, origin.source_sha256.clone()))
            .await
            .is_err()
    );
    let draft = store
        .reuse_saved_conversation_attachment(reuse(&destination.id, origin.source_sha256.clone()))
        .await?;
    assert_eq!(
        draft,
        store
            .reuse_saved_conversation_attachment(reuse(
                &destination.id,
                origin.source_sha256.clone()
            ))
            .await?
    );
    assert!(
        matches!(&draft.files[0].source, AttachmentSource::SavedNative { conversation_id, run_id, name, .. }
        if conversation_id == &chat.id && run_id == &source_run && name == "image.png")
    );
    let revision = draft
        .revision
        .context("Missing reused attachment draft revision")?;
    let reuse_prompt = "Identify these archived bytes.";
    store
        .save_conversation_draft(destination.id.clone(), reuse_prompt.into())
        .await?;
    let review = || {
        store.review_conversation_attachment_draft(
            destination.id.clone(),
            revision.clone(),
            native_scope.clone(),
            reuse_prompt.into(),
        )
    };
    let source_checkout = chat.checkout_id.clone();
    let project_id = store
        .execute(move |db| {
            Ok(db.query_row(
                "SELECT project_id FROM checkouts WHERE id = ?1",
                [source_checkout],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await?;
    let project_limits = store.project_attachment_limits(project_id.clone()).await?;
    store
        .set_project_attachment_limits(
            project_id.clone(),
            AttachmentLimits {
                bytes_per_file: 1,
                ..project_limits
            },
        )
        .await?;
    let blocked = review().await?;
    assert!(blocked.preview_id.is_none());
    assert!(
        blocked.files[0]
            .native_unavailable
            .as_deref()
            .unwrap_or("")
            .contains("file limit")
    );
    store
        .set_project_attachment_limits(project_id, project_limits)
        .await?;
    let reviewed = review().await?;
    assert_eq!(reviewed.files[0].alternatives, [AttachmentDelivery::Native]);
    assert_eq!(reviewed.files[0].media.as_deref(), Some(PNG));
    let reusable = reviewed
        .preview_id
        .context("Expected reviewed archived bytes")?;
    assert!(!image.exists());
    assert!(matches!(
        store
            .submit_conversation_with_files(
                destination.id.clone(),
                destination.context_id.clone(),
                native_scope.clone(),
                reuse_prompt.into(),
                Vec::new(),
                Some(reusable)
            )
            .await?,
        ContinuationInspection::Queued { .. }
    ));
    let plan = admit(&native_scope)
        .await?
        .context("The archived bytes were not admitted")?;
    let reused_run = plan.run.id.clone();
    assert_eq!(
        start_managed_turn(store.clone(), runner_for(&destination.id), plan, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    let reused = store
        .agent_dispatch(destination.id.clone(), reused_run.clone())
        .await?
        .context("The reuse dispatch is missing")?;
    let [reused_evidence] = reused.attachments.as_slice() else {
        anyhow::bail!("Expected one reused attachment")
    };
    assert_eq!(reused_evidence.source_sha256, origin.source_sha256);
    assert_eq!(
        reused.snapshot.attachments[0]
            .input
            .native()
            .unwrap()
            .content,
        PNG
    );
    assert_eq!(
        reused.snapshot.attachments[0].qualification_id.as_deref(),
        Some(qualification.as_str())
    );
    assert!(!image.exists());

    let deferred = store
        .create_conversation(
            chat.checkout_id.clone(),
            "Reviewed re-use before retirement".into(),
        )
        .await?;
    let reusing_derived = || ReuseSavedConversationAttachment {
        source_conversation_id: destination.id.clone(),
        run_id: reused_run.clone(),
        ordinal: reused_evidence.ordinal,
        message_revision_id: reused_evidence.message_revision_id.clone(),
        source_sha256: reused_evidence.source_sha256.clone(),
        destination_conversation_id: deferred.id.clone(),
    };
    let draft = store
        .reuse_saved_conversation_attachment(reusing_derived())
        .await?;
    let review = || {
        store.review_conversation_attachment_draft(
            deferred.id.clone(),
            draft.revision.clone().unwrap(),
            native_scope.clone(),
            reuse_prompt.into(),
        )
    };
    let preview = review()
        .await?
        .preview_id
        .context("Expected a reviewed derived attachment")?;
    let target = AttachmentHistoryTarget {
        conversation_id: chat.id.clone(),
        message_revision_id: origin.message_revision_id,
        attachment_ordinal: origin.ordinal,
    };
    let removal = AttachmentHistoryRemoval::Selected;
    let confirmed = store
        .preview_attachment_history_removal(target.clone(), removal)
        .await?;
    store
        .confirm_attachment_history_removal(target, removal, confirmed.revision)
        .await?;
    let stale = store
        .submit_conversation_with_files(
            deferred.id.clone(),
            deferred.context_id,
            native_scope.clone(),
            reuse_prompt.into(),
            Vec::new(),
            Some(preview),
        )
        .await;
    assert!(matches!(stale, Err(error) if error.to_string().contains("removed")));
    let retired = review().await?;
    assert!(retired.preview_id.is_none());
    assert!(
        retired.files[0]
            .blocker
            .as_deref()
            .unwrap_or("")
            .contains("removed")
    );
    assert!(
        store
            .reuse_saved_conversation_attachment(reusing_derived())
            .await
            .is_err()
    );
    Ok(())
}
