use std::sync::Arc;

use serde_json::json;

use crate::{
    conversations::tests::{prepare, setup},
    dispatch::{Delivery, EventSource},
    jobs::JobScheduler,
    providers::{
        ProviderLocation, SessionIntent,
        capabilities::CAPABILITY_SCHEMA_VERSION,
        profiles::{DesktopProfile, desktop_profile},
        runtime::start_managed_turn,
    },
    runner::{ExecutionPlatform, Runner, WindowsVersion},
};

use super::*;

async fn establish_fixture_cursor(
    store: &Store,
    chat: &Conversation,
    scope: &SessionScope,
) -> Result<String> {
    let source = prepare(store, &chat.id, scope.clone(), "Prior canonical history").await?;
    let assessment = CapabilityScope::from_dispatch(
        scope,
        chat.checkout_id.clone(),
        Some("fixture-executable".into()),
        Some(ExecutionPlatform {
            host: WindowsVersion {
                major: 10,
                minor: 0,
                build: 19045,
                revision: 6466,
                native_architecture: 9,
                server: false,
            },
            guest: None,
        }),
        serde_json::from_value(json!({ "detected": [], "unresolved": [] }))?,
        scope.model.clone(),
        None,
    );
    store.mark_agent_running(source.run.id.clone()).await?;
    let snapshot = serde_json::from_value(json!({
        "version": 13, "contextId": source.context_id, "scope": source.scope,
        "submittedMessages": source.input, "nativeHistory": { "mode": "fresh" },
        "capabilities": { "version": CAPABILITY_SCHEMA_VERSION, "scope": assessment, "entries": [] },
        "scopedLaunch": {
            "provider": "codex", "strategy": "fixture", "approved": {}, "excluded": {},
            "instructionSources": [], "inputs": []
        }
    }))?;
    let dispatch = store
        .prepare_dispatch(
            source.run.id.clone(),
            snapshot,
            vec![b"{\"fixture\":true}".to_vec()],
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let location = ProviderLocation {
        session_id: Some("native-thread".into()),
        turn_id: Some("original-turn".into()),
        ..Default::default()
    };
    store
        .record_agent_observation(
            source.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location,
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            source.run.id.clone(),
            RunStatus::Succeeded,
            Some("Original response".into()),
        )
        .await?;
    Ok(source.run.id)
}

#[tokio::test]
async fn native_compaction_reserves_the_exact_cursor_and_only_verified_completion_restores_it()
-> Result<()> {
    let (_temp, store, chat, mut scope) = setup().await?;
    scope.runner = DirectWindowsRunner::IDENTITY.into();
    scope.permission_revision = DESKTOP_PERMISSION_REVISION.into();
    scope.cli_version = VERIFIED_CODEX_BUILD.into();
    scope.model = Some(VERIFIED_MODEL.into());
    scope.effort = None;
    let original_run = establish_fixture_cursor(&store, &chat, &scope).await?;
    let before = store.conversation_history(chat.id.clone()).await?;
    let mut unqualified = scope.clone();
    unqualified.account_binding = Some("other-account".into());
    assert!(
        store
            .reserve_native_compaction(chat.id.clone(), before.context_id.clone(), unqualified)
            .await
            .is_err()
    );
    assert!(
        store
            .reserve_native_compaction(chat.id.clone(), chat.context_id.clone(), scope.clone())
            .await
            .is_err()
    );
    let plan = store
        .reserve_native_compaction(chat.id.clone(), before.context_id.clone(), scope.clone())
        .await?;
    assert_eq!(plan.provider_session_id, "native-thread");
    assert!(
        store
            .reserve_native_compaction(chat.id.clone(), before.context_id.clone(), scope.clone())
            .await
            .is_err()
    );
    assert!(
        prepare(&store, &chat.id, scope.clone(), "disallowed while active")
            .await
            .is_err()
    );
    assert!(
        store
            .request_conversation_compaction(
                chat.id.clone(),
                before.context_id.clone(),
                scope.clone(),
                scope.clone(),
                String::new(),
                false
            )
            .await
            .is_err()
    );
    let run = plan.run.id;
    store.mark_agent_running(run.clone()).await?;
    store
        .start_native_compaction_submission(run.clone())
        .await?;
    let location = ProviderLocation {
        session_id: Some(plan.provider_session_id),
        turn_id: Some("compacted-turn".into()),
        ..Default::default()
    };
    for data in [
        ProviderEventData::Compacted,
        ProviderEventData::Usage {
            input: Some(60),
            output: Some(17),
            cached_input: None,
        },
        ProviderEventData::NativeCompactionFinished {
            outcome: TurnOutcome::Completed,
        },
    ] {
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: location.clone(),
                    data,
                },
            )
            .await?;
    }
    store
        .finish_native_compaction(run.clone(), RunStatus::Succeeded, None, true)
        .await?;
    let profile = super::super::super::compaction::CompactionProfile {
        model: crate::model_catalog::ModelSelection {
            provider: scope.provider,
            profile_id: scope.profile_id.clone(),
            model_id: VERIFIED_MODEL.into(),
        },
        effort: None,
        mode: scope.mode,
    };
    let status = serde_json::to_value(
        store
            .conversation_compaction(chat.id.clone(), Some(profile), String::new())
            .await?,
    )?;
    assert_eq!(status["native"]["request"]["runId"], run);
    assert_eq!(status["native"]["request"]["status"], "succeeded");
    assert_eq!(
        status["native"]["request"]["boundary"]["turnId"],
        "compacted-turn"
    );
    assert_eq!(status["native"]["request"]["usage"]["input"], 60);
    assert_eq!(status["contextId"], before.context_id);
    assert!(status["activeSummary"].is_null());
    assert_eq!(
        store.conversation_history(chat.id.clone()).await?.messages,
        before.messages
    );
    assert_eq!(
        store
            .latest_conversation_run(chat.id.clone())
            .await?
            .unwrap()
            .id,
        original_run
    );

    let interrupted = store
        .reserve_native_compaction(chat.id.clone(), before.context_id.clone(), scope.clone())
        .await?;
    store.mark_agent_running(interrupted.run.id.clone()).await?;
    store
        .start_native_compaction_submission(interrupted.run.id.clone())
        .await?;
    drop(store);
    let store = Store::open(scope.checkout.join("store.sqlite"))?;
    store.recover_agent_attempts_after_restart().await?;
    assert!(
        store
            .reserve_native_compaction(chat.id.clone(), before.context_id, scope.clone())
            .await
            .is_err()
    );
    let next = prepare(
        &store,
        &chat.id,
        scope,
        "After interrupted native compaction",
    )
    .await?;
    assert!(matches!(next.intent, SessionIntent::Start));
    assert!(
        next.input
            .iter()
            .any(|message| message.text == "Original response")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires authenticated Codex App Server 0.169.0-alpha.11.1 and gpt-6-luna"]
async fn codex_managed_conversation_compacts_its_authentic_session_without_rewriting_transcript()
-> Result<()> {
    let (_temp, store, chat, mut scope) = setup().await?;
    let configured = store.configured_executable(Provider::Codex).await?;
    let profile = tokio::task::spawn_blocking(move || {
        desktop_profile(configured, Some(VERIFIED_MODEL.into()))
    })
    .await??;
    let DesktopProfile {
        executable,
        version,
        account_binding,
        models,
        ..
    } = profile;
    let model = models
        .into_iter()
        .find(|model| model.option.selection.model_id == VERIFIED_MODEL && model.option.selectable)
        .context("The qualified model is unavailable")?
        .option
        .selection;
    scope.profile_id = model.profile_id.clone();
    scope.executable = executable;
    scope.cli_version = version;
    scope.account_binding = account_binding;
    scope.runner = DirectWindowsRunner.identity().into();
    scope.permission_revision = DESKTOP_PERMISSION_REVISION.into();
    scope.model = Some(VERIFIED_MODEL.into());
    scope.effort = None;
    let runner = Arc::new(DirectWindowsRunner);
    let seed = prepare(
        &store,
        &chat.id,
        scope.clone(),
        "Reply with exactly the word WILLOW and do not use tools.",
    )
    .await?;
    assert_eq!(
        start_managed_turn(store.clone(), runner.clone(), seed, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    let before = store.conversation_history(chat.id.clone()).await?;
    let scheduler = JobScheduler::start(store.clone(), runner, None);
    scheduler
        .run_native_compaction(
            store.clone(),
            chat.id.clone(),
            before.context_id.clone(),
            scope.clone(),
        )
        .await?;
    scheduler.shutdown().await?;
    let profile = super::super::super::compaction::CompactionProfile {
        model,
        effort: None,
        mode: scope.mode,
    };
    let status = serde_json::to_value(
        store
            .conversation_compaction(chat.id.clone(), Some(profile), String::new())
            .await?,
    )?;
    let result = &status["native"]["request"];
    assert_eq!(
        result["status"], "succeeded",
        "Native compaction result: {result}"
    );
    assert!(result["boundary"]["turnId"].is_string());
    assert!(status["activeSummary"].is_null());
    let after = store.conversation_history(chat.id.clone()).await?;
    assert_eq!(after.context_id, before.context_id);
    assert_eq!(after.messages, before.messages);
    let next = prepare(
        &store,
        &chat.id,
        scope,
        "Reply with exactly the word MAPLE and do not use tools.",
    )
    .await?;
    assert!(matches!(next.intent, SessionIntent::Resume { .. }));
    assert_eq!(next.input.len(), 1);
    Ok(())
}
