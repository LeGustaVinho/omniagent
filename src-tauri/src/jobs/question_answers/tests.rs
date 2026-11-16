use std::{fs, process::Command};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery, EventSource},
    process_io::{OutputFrame, Stream},
    providers::{
        AdapterUpdate, CliAdapter, CliSession, Provider, ProviderEvent, ProviderEventData,
        RequestId, SessionIntent, SessionMode, SessionRequest, TurnInput,
        antigravity::AntigravityAdapter, questions::QuestionAction,
    },
    repository,
    runner::DirectWindowsRunner,
    store::Store,
};

fn receive(session: &mut dyn CliSession, mut frame: Value) -> Result<AdapterUpdate> {
    frame["jsonrpc"] = json!("2.0");
    session.receive(OutputFrame {
        stream: Stream::Stdout,
        data: frame.to_string(),
    })
}

#[tokio::test]
async fn native_question_timeline_keeps_typed_actions_and_recovers_an_open_form() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("questions.sqlite");
    let root = temporary.path().join("repo");
    fs::create_dir(&root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let store = Store::open(database.clone())?;
    let checkout = repository::register(&store, root.clone()).await?;
    let chat = store
        .create_conversation(checkout.checkout_id, "Plan fixture".into())
        .await?;
    let attempt = store
        .prepare_agent_attempt(
            chat.id.clone(),
            chat.context_id,
            SessionScope {
                provider: Provider::Antigravity,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(root)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner::IDENTITY.into(),
                permission_revision: "fixture".into(),
                mode: SessionMode::Plan,
                model: None,
                effort: None,
            },
            "Ask before changing the API".into(),
        )
        .await?;
    let run = attempt.run.id.clone();
    store.mark_agent_running(run.clone()).await?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 1, "contextId": attempt.context_id, "scope": attempt.scope,
        "baseCommit": null, "submittedMessages": attempt.input, "nativeHistory": { "mode": "fresh" },
    }))?;

    let mut session = AntigravityAdapter.create_session(SessionRequest {
        checkout: attempt.scope.checkout,
        mode: SessionMode::Plan,
        intent: SessionIntent::Start,
        model: None,
        effort: None,
    })?;
    session.start()?;
    receive(
        &mut *session,
        json!({"id":1,"result":{"protocolVersion":1}}),
    )?;
    receive(
        &mut *session,
        json!({"id":2,"result":{"sessionId":"acp-session","configOptions":[
            {"id":"mode","category":"mode","type":"select","currentValue":"plan",
             "options":[{"value":"default"},{"value":"plan"}]}
        ]}}),
    )?;
    let turn = session.send(TurnInput {
        attachments: vec![],
        message_id: "plan".into(),
        text: "Configure delivery".into(),
    })?;
    let dispatch = store
        .prepare_dispatch(
            run.clone(),
            snapshot,
            turn.outbound
                .iter()
                .map(|frame| frame.as_bytes().to_vec())
                .collect(),
        )
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    let native_forms = json!({"sessionId":"acp-session","mode":"form","message":"Choose delivery options",
        "requestedSchema":{"type":"object","required":["plan","tests","deploy","fallback"],"properties":{
            "plan":{"type":"string","oneOf":[{"const":"01","title":"Small"},{"const":"02","title":"Complete"}]},
            "tests":{"type":"array","items":{"type":"string","enum":["backend","frontend"]},"minItems":2},
            "deploy":{"type":"boolean"}, "fallback":{"type":"string"}
        }}
    });
    let values = json!({"plan":"02","tests":["frontend","backend"],"deploy":false,"fallback":"Private workspace"});
    for (id, action, answer, expected) in [
        (
            134,
            QuestionAction::Accept,
            json!({"action":"accept","values":values}),
            json!({"action":"accept","content":values}),
        ),
        (
            135,
            QuestionAction::Decline,
            json!({"action":"decline"}),
            json!({"action":"decline"}),
        ),
        (
            136,
            QuestionAction::Cancel,
            json!({"action":"cancel"}),
            json!({"action":"cancel"}),
        ),
    ] {
        let mut question = receive(
            &mut *session,
            json!({"id":id,"method":"elicitation/create","params":native_forms}),
        )?;
        let event = question
            .events
            .pop()
            .context("validated ACP question missing")?;
        store
            .record_agent_observation(run.clone(), EventSource::Provider, event)
            .await?;
        let request_id = RequestId::Number(id);
        let mut reply = session.respond_question(&request_id, answer.clone())?;
        assert_eq!(
            serde_json::from_slice::<Value>(reply.outbound[0].as_bytes())?,
            json!({"jsonrpc":"2.0","id":id,"result":expected})
        );
        let prepared = reply
            .events
            .pop()
            .context("native prepared evidence missing")?;
        assert!(session.respond_question(&request_id, answer).is_err());
        store
            .record_agent_observation(run.clone(), EventSource::Application, prepared.clone())
            .await?;
        assert!(
            store
                .record_agent_observation(run.clone(), EventSource::Application, prepared)
                .await
                .is_err()
        );
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Application,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::QuestionReplySettled {
                        request_id,
                        action,
                        delivery: Delivery::Submitted,
                    },
                },
            )
            .await?;
    }
    let pending = receive(
        &mut *session,
        json!({"id":137,"method":"elicitation/create","params":native_forms}),
    )?;
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            pending.events[0].clone(),
        )
        .await?;
    store
        .record_agent_observation(
            run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::Approval {
                    request_id: RequestId::Number(137),
                    operation: "write_file".into(),
                    description: None,
                    prompt: None,
                },
            },
        )
        .await?;
    assert!(
        store
            .conversation_question_timeline("other-conversation".into(), run.clone())
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let saved = store
        .saved_conversation_questions(chat.id.clone(), run.clone())
        .await?;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].reason, "runtime_restart");
    let timeline = serde_json::to_value(
        store
            .conversation_question_timeline(chat.id, run.clone())
            .await?,
    )?;
    assert_eq!(timeline.as_array().unwrap().len(), 4);
    assert_eq!(timeline[0]["provider"], "antigravity");
    assert_eq!(timeline[0]["origin"]["runId"], run);
    assert_eq!(timeline[0]["reply"]["action"], "accept");
    let fields = timeline[0]["reply"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| {
            (
                field["id"].as_str().unwrap().to_string(),
                field["value"].clone(),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    assert_eq!(Value::Object(fields), values);
    assert_eq!(timeline[1]["reply"]["action"], "decline");
    assert_eq!(timeline[2]["reply"]["action"], "cancel");
    assert!((0..3).all(|index| timeline[index]["state"] == "submitted"));
    assert_eq!(timeline[3]["state"], "saved");
    assert_eq!(timeline[3]["reason"], "runtime_restart");
    assert!(timeline[3]["reply"].is_null());
    Ok(())
}
