use super::*;
use crate::{
    conversations::{
        references::ConversationReferenceSelection,
        tests::{confirm, prepare, setup},
    },
    dispatch::{
        ByteRange, ContextSnapshot, ConversationArtifactRevision, Delivery, DispatchSourceRevision,
        SelectedMessageSpan,
    },
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, GoalControlState, GoalDefinition, GoalStopAction,
        JobContext,
    },
    providers::SessionIntent,
};

#[tokio::test]
async fn fresh_pipeline_continuation_transfers_the_exact_draft_and_reference_without_dispatch()
-> Result<()> {
    let (_directory, store, original, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    confirm(
        &store,
        &prepare(&store, &original.id, scope.clone(), "Earlier discussion").await?,
    )
    .await?;
    let current = store.conversation(original.id).await?;
    let source = store
        .create_conversation(current.checkout_id.clone(), "Reference".into())
        .await?;
    confirm(
        &store,
        &prepare(&store, &source.id, scope.clone(), "Exact reference content").await?,
    )
    .await?;
    let reference = store
        .preview_conversation_reference(
            current.id.clone(),
            source.id,
            ConversationReferenceSelection::Automatic,
        )
        .await?;
    store
        .edit_conversation_reference_draft(current.id.clone(), reference.id.clone(), true)
        .await?;
    let text = "Review @Reference".to_owned();
    let mut next_scope = scope.clone();
    next_scope.model = Some("other-model".into());
    store
        .save_conversation_draft(current.id.clone(), text.clone())
        .await?;
    let ContinuationInspection::ReviewRequired { preview_id, .. } = store
        .submit_conversation_with_references(
            current.id.clone(),
            current.context_id.clone(),
            next_scope.clone(),
            text.clone(),
            vec![reference.id.clone()],
        )
        .await?
    else {
        bail!("Expected continuation approval")
    };

    store
        .save_conversation_draft(current.id.clone(), "More recent draft".into())
        .await?;
    let denied = store
        .continue_conversation_into_pipeline(
            current.id.clone(),
            preview_id.clone(),
            next_scope.clone(),
            text.clone(),
            ConversationPipelineChoice::RepositoryDelivery,
        )
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("draft changed"));
    assert_eq!(
        store
            .conversation_reference_draft(current.id.clone())
            .await?,
        [reference.clone()]
    );
    store
        .save_conversation_draft(current.id.clone(), text.clone())
        .await?;

    let destination = store
        .continue_conversation_into_pipeline(
            current.id.clone(),
            preview_id.clone(),
            next_scope.clone(),
            text.clone(),
            ConversationPipelineChoice::RepositoryDelivery,
        )
        .await?;
    assert!(matches!(
        destination.pipeline,
        Some(ConversationPipeline::RepositoryDelivery { .. })
    ));
    assert_eq!(destination.checkout_id, current.checkout_id);
    assert_eq!(
        store.conversation_draft(destination.id.clone()).await?,
        text
    );
    assert_eq!(store.conversation_draft(current.id.clone()).await?, "");
    assert!(
        store
            .conversation_reference_draft(current.id.clone())
            .await?
            .is_empty()
    );
    let carried = store
        .conversation_reference_draft(destination.id.clone())
        .await?;
    assert_eq!(carried.len(), 1);
    assert_ne!(carried[0].id, reference.id);
    assert_eq!(carried[0].destination_conversation_id, destination.id);
    assert_eq!(carried[0].artifact, reference.artifact);
    assert!(
        store
            .latest_conversation_run(destination.id.clone())
            .await?
            .is_none()
    );
    assert!(store.claim_next_job(environment(&scope)).await?.is_none());
    assert!(
        store
            .continue_conversation_into_pipeline(
                current.id,
                preview_id,
                next_scope,
                text,
                ConversationPipelineChoice::RepositoryDelivery
            )
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn an_unfinished_goal_keeps_its_checkpoint_and_checkout_when_forking() -> Result<()> {
    use std::process::Command;

    let (_directory, store, original, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let parent = tempfile::tempdir()?;
    let alternate = parent.path().join("goal-fork");
    let commit = Command::new("git.exe")
        .current_dir(&scope.checkout)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Start fixture",
        ])
        .status()?;
    ensure!(commit.success(), "could not initialize goal checkout");
    let worktree = Command::new("git.exe")
        .current_dir(&scope.checkout)
        .args(["worktree", "add", "--quiet", "--detach"])
        .arg(&alternate)
        .status()?;
    ensure!(worktree.success(), "could not create the linked checkout");
    let target = crate::repository::register(&store, alternate).await?;
    let previous = prepare(
        &store,
        &original.id,
        scope.clone(),
        "Preserve the saved discussion",
    )
    .await?;
    confirm(&store, &previous).await?;
    let source = store.conversation(original.id.clone()).await?;
    let definition = GoalDefinition {
        objective: "Verify the original release".into(),
        criteria: Vec::new(),
        max_turns: Some(2),
        max_seconds: None,
        token_budget: None,
    };
    let goal = store
        .start_conversation_goal(
            JobContext {
                conversation_id: source.id.clone(),
                context_id: source.context_id.clone(),
            },
            scope.clone(),
            None,
            definition.clone(),
            None,
        )
        .await?;
    assert_eq!(goal.job.project_id, target.project_id);
    let mut replacement = scope.clone();
    replacement.model = Some("replacement-model".into());
    replacement.checkout = std::path::PathBuf::from(&target.checkout_root);

    let draft = "Discuss a separate release".to_owned();
    store
        .save_conversation_draft(source.id.clone(), draft.clone())
        .await?;
    let preview = || {
        store.submit_conversation_with_files_at_checkout(
            source.id.clone(),
            source.context_id.clone(),
            replacement.clone(),
            draft.clone(),
            Vec::new(),
            None,
            Some(target.checkout_id.clone()),
        )
    };
    let ContinuationInspection::ReviewRequired {
        preview_id: stale, ..
    } = preview().await?
    else {
        bail!("The target checkout was not reviewed")
    };
    assert_eq!(
        store
            .request_goal_stop(
                target.project_id.clone(),
                goal.id.clone(),
                GoalStopAction::Pause
            )
            .await?,
        GoalControlState::Paused
    );
    let rejected = store
        .confirm_conversation_continuation(
            source.id.clone(),
            stale,
            ContinuationChoice::ForkWithContext,
            replacement.clone(),
            draft.clone(),
        )
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("conversation goal changed"));
    let ContinuationInspection::ReviewRequired {
        preview_id,
        goal_transition: Some(transition),
        ..
    } = preview().await?
    else {
        bail!("The retained goal and target were not reviewed together")
    };
    assert_eq!(transition.recorded_goal.revision_id, goal.id);
    assert_eq!(
        transition.recorded_goal.control_state,
        GoalControlState::Paused
    );
    assert_eq!(
        transition.recorded_goal.checkpoint_context_id.as_deref(),
        Some(source.context_id.as_str())
    );
    assert_eq!(transition.recorded_goal.scope.checkout_root, scope.checkout);
    assert_eq!(transition.recorded_goal.scope.model, scope.model);
    assert_eq!(
        transition.next_turn_scope.checkout_root,
        replacement.checkout
    );
    assert_eq!(transition.next_turn_scope.model, replacement.model);
    assert!(!transition.same_tab_available);
    let moved = store
        .confirm_conversation_continuation(
            source.id.clone(),
            preview_id.clone(),
            ContinuationChoice::SameTab,
            replacement.clone(),
            draft.clone(),
        )
        .await
        .unwrap_err();
    assert!(moved.to_string().contains("pending work in this checkout"));
    assert_eq!(store.conversation_draft(source.id.clone()).await?, draft);
    let fork = store
        .confirm_conversation_continuation(
            source.id.clone(),
            preview_id,
            ContinuationChoice::ForkWithContext,
            replacement.clone(),
            draft.clone(),
        )
        .await?;
    assert_ne!(fork.conversation.id, source.id);
    assert_eq!(fork.conversation.checkout_id, target.checkout_id);
    assert!(
        store
            .active_conversation_goal(fork.conversation.id.clone())
            .await?
            .is_none()
    );
    let turn = store
        .claim_next_job(environment(&replacement))
        .await?
        .context("Missing fork turn")?;
    assert_eq!(turn.conversation_id, fork.conversation.id);
    assert!(matches!(turn.intent, SessionIntent::Start));
    confirm(&store, &turn).await?;
    let fork = store.conversation(fork.conversation.id).await?;
    let separate = store
        .start_conversation_goal(
            JobContext {
                conversation_id: fork.id.clone(),
                context_id: fork.context_id,
            },
            replacement,
            None,
            GoalDefinition {
                objective: "Verify the second release".into(),
                ..definition
            },
            None,
        )
        .await?;
    assert_ne!(separate.goal_id, goal.goal_id);
    let initial = store
        .conversation_goal_overview(source.id.clone())
        .await?
        .context("Missing original goal")?;
    assert_eq!(initial.goal.id, goal.id);
    let recorded = initial.transition.context("Missing retained checkpoint")?;
    assert_eq!(recorded.scope.checkout_root, scope.checkout);
    assert_eq!(recorded.scope.model, scope.model);
    assert_eq!(recorded.control_state, GoalControlState::Paused);
    store
        .resume_goal_after_review(target.project_id, goal.id, recorded.review_token)
        .await?;
    let resumed = store
        .claim_next_job(AdmissionEnvironment {
            bridge_available: true,
            ..environment(&scope)
        })
        .await?
        .context("Missing retained goal attempt")?;
    assert_eq!(resumed.conversation_id, source.id);
    assert_eq!(resumed.run.checkout_id, source.checkout_id);
    assert!(resumed.scope == scope);
    Ok(())
}

fn environment(scope: &SessionScope) -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    }
}

#[tokio::test]
async fn an_explicit_compatible_send_resumes_the_verified_session_at_admission() -> Result<()> {
    let (_dir, store, original, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let attempt = prepare(&store, &original.id, scope.clone(), "Saved first message").await?;
    confirm(&store, &attempt).await?;
    let current = store.conversation(original.id).await?;
    assert!(matches!(
        store
            .inspect_conversation_continuation(
                current.id.clone(),
                current.context_id.clone(),
                scope.clone()
            )
            .await?,
        ContinuationInspection::Automatic
    ));
    let text = "New explicit send".to_owned();
    store
        .save_conversation_draft(current.id.clone(), text.clone())
        .await?;
    let queued = store
        .submit_conversation_continuation(
            current.id.clone(),
            current.context_id.clone(),
            scope.clone(),
            text,
        )
        .await?;
    assert!(matches!(queued, ContinuationInspection::Queued { .. }));
    let plan = store
        .claim_next_job(environment(&scope))
        .await?
        .context("The compatible send was not admitted")?;
    assert_eq!(plan.conversation_id, current.id);
    assert_eq!(
        plan.resume_cursor_id.as_deref(),
        Some(current.context_id.as_str())
    );
    assert_eq!(plan.agent_session_id, attempt.agent_session_id);
    assert!(
        matches!(plan.intent, SessionIntent::Resume { provider_session_id } if provider_session_id == "native-thread")
    );
    assert_eq!(
        plan.input
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["New explicit send"]
    );
    Ok(())
}

#[tokio::test]
async fn a_pinned_native_send_cannot_seed_history_if_its_verified_cursor_changes() -> Result<()> {
    let (_dir, store, original, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let attempt = prepare(&store, &original.id, scope.clone(), "Saved first message").await?;
    confirm(&store, &attempt).await?;
    let current = store.conversation(original.id).await?;
    let draft = "Continue with the verified session".to_owned();
    store
        .save_conversation_draft(current.id.clone(), draft.clone())
        .await?;
    let queued = store
        .submit_conversation_continuation(
            current.id.clone(),
            current.context_id.clone(),
            scope.clone(),
            draft,
        )
        .await?;
    let ContinuationInspection::Queued { job } = queued else {
        bail!("Compatible continuation was not queued")
    };
    store
        .execute(move |db| {
            db.execute(
                "UPDATE agent_sessions SET cursor_context_id = NULL WHERE id = ?1",
                [&attempt.agent_session_id],
            )?;
            Ok(())
        })
        .await?;
    assert_eq!(
        store.conversation(current.id.clone()).await?.context_id,
        current.context_id
    );
    assert!(store.claim_next_job(environment(&scope)).await?.is_none());
    let blocked = store
        .latest_conversation_job(current.id, environment(&scope))
        .await?
        .context("Missing continuation job")?;
    assert_eq!(blocked.job.id, job.id);
    assert!(blocked.blockers.iter().any(|blocker| matches!(blocker, AdmissionBlocker::Policy { reason } if reason.contains("verified provider session changed"))));
    Ok(())
}

#[tokio::test]
async fn verifies_native_cursor_and_pins_reference_delivery_for_all_three_continuations()
-> Result<()> {
    for choice in [
        ContinuationChoice::SameTab,
        ContinuationChoice::ForkWithContext,
        ContinuationChoice::Fresh,
    ] {
        let (_dir, store, original, scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        let attempt = prepare(&store, &original.id, scope.clone(), "Retained user message").await?;
        confirm(&store, &attempt).await?;
        let current = store.conversation(original.id).await?;
        let auto = store
            .inspect_conversation_continuation(
                current.id.clone(),
                current.context_id.clone(),
                scope.clone(),
            )
            .await?;
        assert!(matches!(auto, ContinuationInspection::Automatic));

        let defaults = store
            .conversation_continuation_settings(current.id.clone())
            .await?;
        assert_eq!(defaults.effective, ContinuationChoice::SameTab);
        store
            .set_conversation_continuation_policy(
                current.id.clone(),
                ContinuationLevel::Project,
                Some(ContinuationChoice::Fresh),
            )
            .await?;
        let mut changed = scope;
        changed.model = Some("replacement-model".into());
        let source = store
            .create_conversation(current.checkout_id.clone(), "Reference source".into())
            .await?;
        let source_text = "Unselected opening. café 🦀\nPinned answer. Unselected ending.";
        let quoted = "café 🦀\nPinned answer.";
        let start = source_text.find(quoted).context("missing test excerpt")?;
        let source_turn = prepare(&store, &source.id, changed.clone(), source_text).await?;
        confirm(&store, &source_turn).await?;
        let available = store
            .conversation_reference_source(current.id.clone(), source.id.clone(), None)
            .await?;
        let source_context = available.context_id;
        let source_revision = available.messages[0].revision_id.clone();
        let reference = store
            .preview_conversation_reference(
                current.id.clone(),
                source.id.clone(),
                ConversationReferenceSelection::Messages {
                    context_id: source_context.clone(),
                    spans: vec![SelectedMessageSpan {
                        revision_id: source_revision.clone(),
                        byte_range: Some(ByteRange {
                            start,
                            end: start + quoted.len(),
                        }),
                    }],
                },
            )
            .await?;
        assert_eq!(reference.destination_conversation_id, current.id);
        assert_eq!(reference.source_conversation_id, source.id);
        assert_eq!(reference.source_title, source.title);
        assert_eq!(reference.project_id, attempt.run.project_id);
        assert_eq!(reference.project_name, available.project_name);
        let ConversationArtifactRevision::Excerpt {
            source_context_id,
            excerpts,
            ..
        } = &reference.artifact
        else {
            bail!("The selected messages did not produce an excerpt");
        };
        assert_eq!(source_context_id, &source_context);
        assert!(matches!(excerpts.as_slice(), [excerpt]
            if excerpt.text == quoted && excerpt.source.origin.revision_id == source_revision));
        assert_eq!(
            store
                .edit_conversation_reference_draft(current.id.clone(), reference.id.clone(), true)
                .await?,
            [reference.clone()]
        );
        let prompt = "Next user message".to_owned();
        store
            .save_conversation_draft(current.id.clone(), prompt.clone())
            .await?;
        let inspected = store
            .submit_conversation_with_references(
                current.id.clone(),
                current.context_id.clone(),
                changed.clone(),
                prompt.clone(),
                vec![reference.id.clone()],
            )
            .await?;
        let ContinuationInspection::ReviewRequired {
            preview_id,
            context_id,
            reason,
            seed,
            settings,
            ..
        } = inspected
        else {
            bail!("A changed model reused a verified session");
        };
        assert!(matches!(reason, ContinuationReason::ProfileChanged));
        assert_eq!(context_id, current.context_id);
        assert_eq!(settings.effective, ContinuationChoice::Fresh);
        assert_eq!(
            seed.iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["Retained user message", "answer"]
        );
        let override_settings = store
            .set_conversation_continuation_policy(
                current.id.clone(),
                ContinuationLevel::Conversation,
                Some(choice),
            )
            .await?;
        assert_eq!(override_settings.effective, choice);
        assert_eq!(override_settings.project_default, ContinuationChoice::Fresh);

        assert_eq!(store.conversation_draft(current.id.clone()).await?, prompt);
        assert!(store.claim_next_job(environment(&changed)).await?.is_none());
        store
            .rewind_conversation_history(source.id.clone(), source_context.clone(), None)
            .await?;
        store
            .edit_conversation_reference_draft(current.id.clone(), reference.id.clone(), false)
            .await?;
        assert!(
            store
                .confirm_conversation_continuation(
                    current.id.clone(),
                    preview_id.clone(),
                    choice,
                    changed.clone(),
                    prompt.clone(),
                )
                .await
                .is_err()
        );
        assert_eq!(store.conversation_draft(current.id.clone()).await?, prompt);
        store
            .edit_conversation_reference_draft(current.id.clone(), reference.id.clone(), true)
            .await?;
        let result = store
            .confirm_conversation_continuation(
                current.id.clone(),
                preview_id.clone(),
                choice,
                changed.clone(),
                prompt.clone(),
            )
            .await?;
        assert_eq!(
            result.conversation.id == current.id,
            choice == ContinuationChoice::SameTab
        );
        assert_eq!(result.conversation.checkout_id, current.checkout_id);
        let queued_context = result.job.context.as_ref().context("No queued context")?;
        assert_eq!(queued_context.conversation_id, result.conversation.id);
        assert_eq!(queued_context.context_id, result.conversation.context_id);
        assert!(
            store
                .conversation_reference_draft(current.id.clone())
                .await?
                .is_empty()
        );
        assert!(
            store
                .conversation_draft(current.id.clone())
                .await?
                .is_empty()
        );
        assert!(
            store
                .confirm_conversation_continuation(
                    current.id.clone(),
                    preview_id,
                    choice,
                    changed.clone(),
                    prompt,
                )
                .await
                .is_err()
        );

        drop(store);
        let store = Store::open(changed.checkout.join("store.sqlite"))?;
        let plan = store
            .claim_next_job(environment(&changed))
            .await?
            .context("Reviewed continuation was not admitted")?;
        assert_eq!(result.conversation.id, plan.conversation_id);
        assert!(matches!(plan.intent, SessionIntent::Start));
        let received = plan
            .input
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>();
        if choice == ContinuationChoice::Fresh {
            assert_eq!(received, ["Next user message"]);
        } else {
            assert_eq!(
                received,
                ["Retained user message", "answer", "Next user message"]
            );
        }
        if choice == ContinuationChoice::ForkWithContext {
            assert_ne!(seed[0].revision_id, plan.input[0].revision_id);
        }
        if choice != ContinuationChoice::SameTab {
            assert_eq!(
                store.conversation(current.id.clone()).await?.context_id,
                current.context_id
            );
        }
        if choice == ContinuationChoice::SameTab {
            assert_eq!(seed[0].revision_id, plan.input[0].revision_id);
        }
        let submitted = store
            .conversation_dispatch_references(
                plan.run.id.clone(),
                plan.input
                    .iter()
                    .map(|message| message.revision_id.clone())
                    .collect(),
            )
            .await?;
        let [link] = submitted.as_slice() else {
            bail!("The reviewed reference did not follow the queued turn");
        };
        let new_revision = &plan
            .input
            .last()
            .context("Missing queued message")?
            .revision_id;
        assert_eq!(&link.message_revision_id, new_revision);
        assert_ne!(new_revision, &source_revision);
        assert_eq!(link.reference, reference);
        store.mark_agent_running(plan.run.id.clone()).await?;
        let snapshot: ContextSnapshot = serde_json::from_value(serde_json::json!({
            "version": 14, "contextId": plan.context_id, "scope": plan.scope, "baseCommit": null,
            "submittedMessages": plan.input, "conversationReferences": submitted,
            "nativeHistory": { "mode": "fresh" },
        }))?;
        let dispatch = store
            .prepare_dispatch(
                plan.run.id.clone(),
                snapshot,
                vec![b"{\"fixture\":true}".to_vec()],
            )
            .await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let recorded = store
            .agent_dispatch(result.conversation.id.clone(), plan.run.id.clone())
            .await?
            .context("The destination has no recorded dispatch")?;
        assert_eq!(recorded.snapshot.conversation_references, submitted);
        assert_eq!(recorded.snapshot.scope.checkout, changed.checkout);
        let lineage = store
            .dispatch_lineage(result.conversation.id.clone(), plan.run.id.clone())
            .await?
            .context("No saved dispatch lineage")?;
        assert!(lineage.sources.iter().any(|entry| matches!(entry,
            DispatchSourceRevision::ConversationReference {
                message_revision_id, project_id, source_conversation_id, artifact_revision_id,
            } if message_revision_id == new_revision && project_id == &reference.project_id
                && source_conversation_id == &source.id && artifact_revision_id == reference.artifact.id()
        )));
        assert!(
            store
                .dispatches_using_message(source.id.clone(), source_revision.clone())
                .await?
                .iter()
                .any(|entry| entry.run_id == plan.run.id
                    && entry.conversation_id == result.conversation.id)
        );
        let origin = store
            .conversation_reference_source(result.conversation.id, source.id, Some(source_context))
            .await?;
        assert!(
            origin.messages.iter().any(
                |message| message.revision_id == source_revision && message.text == source_text
            )
        );
    }
    Ok(())
}

#[tokio::test]
async fn rejects_changed_profile_and_blocks_a_queued_continuation_after_protected_history_changes()
-> Result<()> {
    let (_dir, store, original, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let attempt = prepare(
        &store,
        &original.id,
        scope.clone(),
        "Original protected seed",
    )
    .await?;
    confirm(&store, &attempt).await?;
    let current = store.conversation(original.id).await?;
    let mut changed = scope;
    changed.effort = None;
    let inspected = store
        .inspect_conversation_continuation(
            current.id.clone(),
            current.context_id.clone(),
            changed.clone(),
        )
        .await?;
    let ContinuationInspection::ReviewRequired {
        preview_id, seed, ..
    } = inspected
    else {
        bail!("A reasoning profile change needs reviewed history");
    };
    let prompt = "Next user message".to_owned();
    store
        .save_conversation_draft(current.id.clone(), prompt.clone())
        .await?;
    let mut other = changed.clone();
    other.model = Some("unreviewed-model".into());
    assert!(
        store
            .confirm_conversation_continuation(
                current.id.clone(),
                preview_id.clone(),
                ContinuationChoice::SameTab,
                other,
                prompt.clone(),
            )
            .await
            .is_err()
    );
    assert_eq!(store.conversation_draft(current.id.clone()).await?, prompt);

    let result = store
        .confirm_conversation_continuation(
            current.id.clone(),
            preview_id,
            ContinuationChoice::SameTab,
            changed.clone(),
            prompt,
        )
        .await?;
    let revision = seed[0].revision_id.clone();
    store
        .execute(move |db| {
            let changed = protection::protect(b"Changed protected seed")?;
            db.execute(
                "UPDATE message_revisions SET content = ?1 WHERE id = ?2",
                params![changed, revision],
            )?;
            Ok(())
        })
        .await?;
    assert!(store.claim_next_job(environment(&changed)).await?.is_none());
    let inspected = store
        .latest_conversation_job(current.id, environment(&changed))
        .await?
        .context("Queued job disappeared")?;
    assert_eq!(inspected.job.id, result.job.id);
    assert!(inspected.blockers.iter().any(|reason| matches!(reason, AdmissionBlocker::ContextChanged { context_id } if context_id == &current.context_id)));
    Ok(())
}
