use super::*;
use crate::{
    conversations::tests::{confirm, prepare, setup},
    jobs::{AdmissionBlocker, AdmissionEnvironment},
    providers::SessionIntent,
};

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
async fn verifies_native_cursor_and_distinguishes_the_three_reviewed_continuations() -> Result<()> {
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
        let prompt = "Next user message".to_owned();
        store
            .save_conversation_draft(current.id.clone(), prompt.clone())
            .await?;
        let inspected = store
            .submit_conversation_continuation(
                current.id.clone(),
                current.context_id.clone(),
                changed.clone(),
                prompt.clone(),
            )
            .await?;
        let ContinuationInspection::ReviewRequired {
            preview_id,
            context_id,
            reason,
            seed,
            settings,
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
            assert_eq!(
                store.conversation(current.id).await?.context_id,
                current.context_id
            );
        }
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
