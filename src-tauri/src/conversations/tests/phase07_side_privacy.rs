use std::ffi::OsStr;

use crate::{
    conversations::references::ConversationReferenceSelection,
    dispatch::SelectedMessageSpan,
    jobs::{AdmissionEnvironment, SidePurpose, SideSubmission},
    providers::capabilities::{CAPABILITY_SCHEMA_VERSION, CapabilityAssessment, CapabilityScope},
};

use super::*;

pub(super) const OLDER_TRANSCRIPT: &str = "Older transcript to cover with a compacted summary. ";
pub(super) const PRIMARY_SESSION: &str = "2d8a7889-61ba-415d-8f95-4985c67952b7";
const COMPACTED_SUMMARY: &str = "Compact evidence from the permitted main-thread transcript.";

pub(super) async fn queue_question(
    store: &Store,
    chat: &str,
    context: &str,
    scope: &SessionScope,
    question: &str,
) -> Result<String> {
    let SideSubmission::Queued { job_id } = store
        .enqueue_side_request(
            chat.to_owned(),
            Some(context.to_owned()),
            None,
            scope.clone(),
            SidePurpose::Question,
            question.into(),
        )
        .await?
    else {
        anyhow::bail!("The qualified isolated route did not admit the side question");
    };
    Ok(job_id)
}

fn primary_location(attempt: &AttemptPlan) -> ProviderLocation {
    ProviderLocation {
        session_id: Some(PRIMARY_SESSION.into()),
        turn_id: Some(attempt.run.turn_id.clone()),
        ..Default::default()
    }
}

pub(super) async fn stream_main(store: &Store, attempt: &AttemptPlan, text: &str) -> Result<()> {
    store
        .record_agent_observation(
            attempt.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: primary_location(attempt),
                data: ProviderEventData::AssistantDelta { text: text.into() },
            },
        )
        .await?;
    Ok(())
}

pub(super) async fn finish_main(store: &Store, attempt: &AttemptPlan, answer: &str) -> Result<()> {
    for data in [
        ProviderEventData::SessionReady {
            model: attempt.scope.model.clone(),
            effort: attempt.scope.effort.clone(),
        },
        ProviderEventData::TurnFinished {
            outcome: TurnOutcome::Completed,
        },
    ] {
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: primary_location(attempt),
                    data,
                },
            )
            .await?;
    }
    store
        .finish_agent_attempt(
            attempt.run.id.clone(),
            RunStatus::Succeeded,
            Some(answer.into()),
        )
        .await
}

pub(super) async fn verify_after_restart(
    store: &Store,
    chat: &Conversation,
    scope: &SessionScope,
    profile: &CapabilityScope,
    assert_without_side: &impl Fn(&str),
) -> Result<()> {
    let next = prepare(store, &chat.id, scope.clone(), "Next main prompt").await?;
    let next_run_id = next.run.id.clone();
    let runner = Arc::new(RecordedMainRunner {
        scope: scope.clone(),
        frames: Arc::default(),
    });
    let outcome = start_managed_turn(store.clone(), runner.clone(), next, None)
        .wait()
        .await?;
    if outcome != RunStatus::Succeeded {
        let events = store
            .agent_observations(chat.id.clone(), next_run_id.clone(), 0)
            .await?;
        let latest = events
            .iter()
            .filter_map(|event| match &event.event.data {
                ProviderEventData::Error { message }
                | ProviderEventData::Diagnostic { message } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        anyhow::bail!("The primary stream completed with {outcome:?}: {latest:?}");
    }
    let dispatch = store
        .agent_dispatch(chat.id.clone(), next_run_id)
        .await?
        .context("missing next main dispatch")?;
    assert_eq!(dispatch.delivery, Delivery::Submitted);
    let outbound: Vec<_> = runner
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|bytes| {
            serde_json::from_slice::<Value>(bytes).is_ok_and(|frame| frame["type"] == "user")
        })
        .cloned()
        .collect();
    assert_eq!(dispatch.frames, outbound);
    assert!(
        matches!(dispatch.snapshot.native_history, NativeHistory::ProviderCursor { ref session_id, .. } if session_id == PRIMARY_SESSION)
    );
    let wire = String::from_utf8(outbound.concat())?;
    assert!(wire.contains("Next main prompt"));
    assert_without_side(&wire);
    assert_without_side(&serde_json::to_string(
        &dispatch.snapshot.submitted_messages,
    )?);
    let provider_sessions = store.conversation_sessions(chat.id.clone()).await?;
    assert!(
        provider_sessions
            .iter()
            .any(|session| session.provider_session_id.as_deref() == Some(PRIMARY_SESSION))
    );
    assert!(
        provider_sessions
            .iter()
            .all(|session| session.provider_session_id.as_deref() != Some("side-only"))
    );

    // Use the pinned qualified profile for the next side launch. The full
    // streaming runtime above independently inspected the main launch scope.
    let recent = prepare(
        store,
        &chat.id,
        scope.clone(),
        "Retain this approved recent instruction.",
    )
    .await?;
    store.mark_agent_running(recent.run.id.clone()).await?;
    let id = prepare_fixture_dispatch_with_capabilities(
        store,
        &recent,
        Some(CapabilityAssessment {
            version: CAPABILITY_SCHEMA_VERSION,
            scope: profile.clone(),
            entries: vec![],
        }),
    )
    .await?;
    store.settle_dispatch(id, Delivery::Submitted).await?;
    finish_main(store, &recent, "A recent main answer.").await?;

    let history = store.conversation_history(chat.id.clone()).await?;
    let compaction = store
        .request_conversation_compaction(
            chat.id.clone(),
            history.context_id,
            scope.clone(),
            scope.clone(),
            String::new(),
            false,
        )
        .await?;
    let request = serde_json::to_value(compaction)?;
    let artifact_id = request["request"]["artifactId"]
        .as_str()
        .context("compaction request was not saved")?;
    let summary = store
        .claim_next_job(AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        })
        .await?
        .context("compaction job was not admitted")?;
    let summary_input = &summary.input[0].text;
    assert!(summary_input.contains(OLDER_TRANSCRIPT));
    assert_without_side(summary_input);
    store.mark_agent_running(summary.run.id.clone()).await?;
    let id = prepare_fixture_dispatch(store, &summary).await?;
    store.settle_dispatch(id, Delivery::Submitted).await?;
    store
        .record_agent_observation(
            summary.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            summary.run.id,
            RunStatus::Succeeded,
            Some(COMPACTED_SUMMARY.into()),
        )
        .await?;
    let compacted = store
        .conversation_compaction(chat.id.clone(), None, String::new())
        .await?;
    let compacted = serde_json::to_value(compacted)?;
    let compacted_context_id = compacted["contextId"]
        .as_str()
        .context("compaction context is missing")?;
    let artifact = &compacted["activeSummary"];
    assert_eq!(artifact["id"], artifact_id, "summary was not activated");
    assert_without_side(&serde_json::to_string(artifact)?);

    let side_job = queue_question(
        store,
        &chat.id,
        compacted_context_id,
        scope,
        "Inspect the compacted context.",
    )
    .await?;
    let frozen: Value = store
        .execute({
            let side_job = side_job.clone();
            move |db| {
                let payload: Vec<u8> = db.query_row(
                    "SELECT payload FROM job_side_requests WHERE job_id = ?1",
                    [&side_job],
                    |row| row.get(0),
                )?;
                Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?)
            }
        })
        .await?;
    assert_eq!(frozen["pin"]["contextId"], compacted_context_id);
    assert_eq!(frozen["compaction_artifact_id"], artifact_id);
    let saved = store.saved_side_requests(chat.id.clone()).await?;
    assert!(saved.iter().any(|record| record.job.id == side_job
        && record.compaction_artifact_id.as_deref() == Some(artifact_id)));
    let side_context = frozen["context"]
        .as_str()
        .context("missing frozen side context")?;
    assert!(side_context.contains(COMPACTED_SUMMARY));
    assert!(side_context.contains("Retain this approved recent instruction."));
    assert!(!side_context.contains(OLDER_TRANSCRIPT));
    assert_without_side(side_context);
    store
        .delete_conversation_side_question(chat.id.clone(), side_job)
        .await?;

    let destination = store
        .create_conversation(chat.checkout_id.clone(), "Reference recipient".into())
        .await?;
    let source = store
        .conversation_reference_source(destination.id.clone(), chat.id.clone(), None)
        .await?;
    assert!(
        source
            .messages
            .iter()
            .any(|message| message.text.contains(OLDER_TRANSCRIPT))
    );
    assert_without_side(&serde_json::to_string(&source)?);
    let reference = store
        .preview_conversation_reference(
            destination.id,
            chat.id.clone(),
            ConversationReferenceSelection::Messages {
                context_id: source.context_id,
                spans: source
                    .messages
                    .iter()
                    .map(|message| SelectedMessageSpan {
                        revision_id: message.revision_id.clone(),
                        byte_range: None,
                    })
                    .collect(),
            },
        )
        .await?;
    assert_without_side(&serde_json::to_string(&reference)?);
    Ok(())
}

#[derive(Clone)]
struct RecordedMainRunner {
    scope: SessionScope,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct MainWire(Arc<Mutex<Vec<Vec<u8>>>>);

impl io::Write for MainWire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().push(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Runner for RecordedMainRunner {
    fn identity(&self) -> &str {
        &self.scope.runner
    }

    fn executable_revision(&self, _: &std::path::Path) -> Option<String> {
        Some("side-fixture-build".into())
    }

    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        DirectWindowsRunner.execution_platform()
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments == ["plugin", "list", "--json"].map(OsStr::new) {
            let (send, output) = mpsc::channel();
            send.send(Ok(OutputFrame {
                stream: Stream::Stdout,
                data: "[]".into(),
            }))
            .map_err(io::Error::other)?;
            return Ok(CapturedProcess {
                input: ProcessInput::new(io::sink()),
                output,
                control: Arc::new(CompletedProcess),
            });
        }
        assert!(
            request
                .arguments
                .windows(2)
                .any(|args| args == ["--resume", PRIMARY_SESSION].map(OsStr::new))
        );
        assert!(
            !request
                .arguments
                .contains(&OsStr::new("--no-session-persistence"))
        );
        let (send, output) = mpsc::channel();
        for frame in [
            json!({"type":"control_response","response":{"request_id":"omniagent-initialize","subtype":"success","response":{"current_permission_mode":"plan"}}}),
            json!({"type":"system","subtype":"init","session_id":PRIMARY_SESSION,"model":self.scope.model,"permissionMode":"plan","tools":[],"cwd":request.checkout}),
            json!({"type":"assistant","session_id":PRIMARY_SESSION,"message":{"id":"next-message","model":self.scope.model,"content":[{"type":"text","text":"Next main answer"}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"session_id":PRIMARY_SESSION,"result":"Next main answer"}),
        ] {
            send.send(Ok(OutputFrame {
                stream: Stream::Stdout,
                data: frame.to_string(),
            }))
            .map_err(io::Error::other)?;
        }
        Ok(CapturedProcess {
            input: ProcessInput::new(MainWire(self.frames.clone())),
            output,
            control: Arc::new(CompletedProcess),
        })
    }
}
