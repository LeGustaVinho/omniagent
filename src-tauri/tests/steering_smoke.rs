use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    dispatch::EventSource,
    providers::{
        Provider, ProviderEventData, SessionMode, TurnInput,
        profiles::{ConfiguredExecutable, probe_cli},
        runtime::start_managed_turn,
        steering::{SteerOutcome, SteerTarget},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
};
use serde_json::Value;

#[tokio::test]
#[ignore = "requires an installed authenticated Codex App Server with gpt-6-luna and turn/steer"]
async fn codex_confirms_steering_on_its_original_turn_without_rewriting_dispatch() -> Result<()> {
    let root = tempfile::tempdir()?;
    let checkout = root.path().join("checkout");
    std::fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&checkout)
            .status()?
            .success(),
        "could not initialize the disposable checkout"
    );
    let store = Store::open(root.path().join("steering.sqlite"))?;
    let project = repository::register(&store, checkout.clone()).await?;
    let conversation = store
        .create_conversation(project.checkout_id, "Steering qualification".into())
        .await?;
    let provider = Provider::Codex;
    let cli = probe_cli(ConfiguredExecutable {
        provider,
        executable: None,
    })?;
    let scope = SessionScope {
        provider,
        profile_id: "steering-fixture".into(),
        account_binding: Some("authenticated-vendor-fixture".into()),
        checkout,
        executable: PathBuf::from(cli.executable.context("missing Codex executable")?),
        cli_version: cli.version.context("missing Codex version")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "fixture-read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("gpt-6-luna".into()),
        effort: None,
    };
    let original =
        "Write a numbered list of 300 trees. Do not use tools. Start with FIRST_TURN_MARKER.";
    let follow_up = "Stop listing trees. Reply with STEERED_FIXTURE exactly and no punctuation.";
    let attempt = store
        .prepare_agent_attempt(
            conversation.id.clone(),
            conversation.context_id,
            scope,
            original.into(),
        )
        .await?;
    let run = attempt.run.id.clone();
    let worker = start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), attempt, None);
    let input = |id: &str, text: &str| TurnInput {
        message_id: id.into(),
        text: text.into(),
        attachments: Vec::new(),
    };
    let mut after = 0;
    let mut steering_target = None;
    let mut preparation = None;
    let mut settlements = Vec::new();
    let mut echoed = false;
    tokio::time::timeout(Duration::from_secs(70), async {
        loop {
            for row in store
                .agent_observations(conversation.id.clone(), run.clone(), after)
                .await?
            {
                after = row.seq;
                let event = row.event;
                match event.data {
                    ProviderEventData::AssistantDelta { .. } if steering_target.is_none() => {
                        let target = SteerTarget {
                            session_id: event.location.session_id.context("missing session ID")?,
                            turn_id: event.location.turn_id.context("missing turn ID")?,
                        };
                        let stale = SteerTarget {
                            turn_id: "stale-turn".into(),
                            ..target.clone()
                        };
                        worker
                            .steer(stale, input("stale-revision", "STALE_FIXTURE"))
                            .await?;
                        worker
                            .steer(target.clone(), input("empty-revision", "\t"))
                            .await?;
                        worker
                            .steer(target.clone(), input("steering-revision", follow_up))
                            .await?;
                        steering_target = Some(target);
                    }
                    ProviderEventData::AssistantMessage { text }
                        if text.trim() == "STEERED_FIXTURE" =>
                    {
                        echoed = true;
                        assert_eq!(
                            event.location.turn_id.as_ref(),
                            steering_target.as_ref().map(|target| &target.turn_id)
                        );
                    }
                    ProviderEventData::SteeringPrepared {
                        message_id,
                        target,
                        frame,
                    } => {
                        assert_eq!(row.source, EventSource::Application);
                        assert_eq!(message_id, "steering-revision");
                        assert_eq!(Some(&target), steering_target.as_ref());
                        let body: Value = serde_json::from_slice(&frame)?;
                        assert_eq!(body["method"], "turn/steer");
                        assert_eq!(body["params"]["threadId"], target.session_id);
                        assert_eq!(body["params"]["expectedTurnId"], target.turn_id);
                        assert_eq!(body["params"]["input"][0]["text"], follow_up);
                        assert!(preparation.replace(row.seq).is_none());
                    }
                    ProviderEventData::SteeringSettled {
                        message_id,
                        target,
                        outcome,
                    } => {
                        if outcome == SteerOutcome::Accepted {
                            assert_eq!(row.source, EventSource::Provider);
                            assert!(preparation.is_some_and(|prepared| prepared < row.seq));
                            assert_eq!(Some(&target), steering_target.as_ref());
                        }
                        settlements.push((message_id, outcome));
                    }
                    ProviderEventData::TurnFinished { .. } => return Ok::<_, anyhow::Error>(()),
                    _ => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .context("steering fixture timed out")??;

    assert_eq!(worker.wait().await?, RunStatus::Succeeded);
    assert!(
        echoed,
        "the intended turn did not follow the steering instruction"
    );
    assert_eq!(
        settlements,
        [
            ("stale-revision".into(), SteerOutcome::StaleTurn),
            ("empty-revision".into(), SteerOutcome::UnsupportedInput),
            ("steering-revision".into(), SteerOutcome::Accepted),
        ]
    );
    let dispatch = store
        .agent_dispatch(conversation.id, run)
        .await?
        .context("missing original dispatch")?;
    let [original_frame] = dispatch.frames.as_slice() else {
        anyhow::bail!("steering altered the number of original frames");
    };
    let body: Value = serde_json::from_slice(original_frame)?;
    assert_eq!(body["method"], "turn/start");
    assert_eq!(body["params"]["input"][0]["text"], original);
    Ok(())
}
