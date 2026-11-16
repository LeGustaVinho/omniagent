use super::*;

#[tokio::test]
async fn attachment_composer_review_matches_saved_dispatch_after_profile_change() -> Result<()> {
    use crate::attachments::{
        AttachmentDelivery, AttachmentFormat, AttachmentInput, NativeAttachmentEncoding,
    };
    use crate::conversations::draft_files::DraftAttachmentEdit;
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
        .agent_dispatch(chat.id.clone(), run)
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
    Ok(())
}
