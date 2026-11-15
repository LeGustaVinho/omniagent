use std::path::Path;

use anyhow::{Context, Result, bail};
use omniagent::{
    process_io::{OutputFrame, Stream},
    providers::{
        AdapterUpdate, ApprovalDecision, Capability, CliSession, Provider, ProviderEvent,
        ProviderEventData as Event, RequestId, SessionIntent, SessionMode, SessionRequest,
        TurnInput, TurnOutcome,
        capabilities::{
            Availability, CapabilityDescriptor, CapabilityStatus, CapabilitySubject,
            DeclaredSource, EvidenceBoundary, EvidenceOrigin, ObservedResult, ResolvedCapability,
            Verification,
        },
    },
};
use serde_json::{Value, json};

const APPROVAL: &str = "fixture-approval";
const SESSION: &str = "fixture-session";
const ANSWER: &str = "Answer ☕";

fn handshake(provider: Provider, request: &Value, checkout: &Path) -> Result<Option<Value>> {
    if provider == Provider::Claude {
        assert_eq!(request["request"]["subtype"], "initialize");
        return Ok(Some(json!({
            "type":"control_response", "response":{"subtype":"success",
                "request_id":request["request_id"],"response":{"current_permission_mode":"plan"}}
        })));
    }
    let result = match (provider, request["method"].as_str()) {
        (Provider::Codex, Some("initialize")) => json!({"userAgent":"fixture"}),
        (Provider::Codex, Some("initialized")) => return Ok(None),
        (Provider::Codex, Some("thread/start")) => json!({
            "thread":{"id":SESSION},"cwd":checkout,"model":"fixture-model",
            "sandbox":{"type":"readOnly"}
        }),
        (Provider::Codex, Some("model/list" | "skills/list")) => json!({"data":[]}),
        (Provider::Codex, Some("config/read")) => json!({"config":{}}),
        (Provider::Antigravity, Some("initialize")) => {
            assert_eq!(request["params"]["protocolVersion"], 1);
            json!({"protocolVersion":1,"agentCapabilities":{}})
        }
        (Provider::Antigravity, Some("session/new")) => {
            json!({"sessionId":SESSION,"modes":{"currentModeId":"plan"}})
        }
        _ => bail!("Unexpected {provider:?} fixture request: {request}"),
    };
    Ok(Some(if provider == Provider::Antigravity {
        json!({"jsonrpc":"2.0","id":request["id"],"result":result})
    } else {
        json!({"id":request["id"],"result":result})
    }))
}

fn transcript(provider: Provider, request: &Value, turn: &str) -> impl Iterator<Item = String> {
    let template = match provider {
        Provider::Codex => include_str!("fixtures/providers/codex-app-server.jsonl"),
        Provider::Claude => include_str!("fixtures/providers/claude-stream-json.jsonl"),
        Provider::Antigravity => include_str!("fixtures/providers/antigravity-acp-v1.jsonl"),
    };
    let id = request["id"].to_string();
    template.lines().map(move |line| {
        line.replace("\"$prompt_id\"", &id)
            .replace("fixture-turn", turn)
            .replace("fixture-result", turn)
    })
}

fn receive(session: &mut dyn CliSession, data: String) -> Result<AdapterUpdate> {
    session.receive(OutputFrame {
        stream: Stream::Stdout,
        data,
    })
}

fn start_session(provider: Provider, checkout: &Path) -> Result<Box<dyn CliSession>> {
    let mut session = provider.adapter().create_session(SessionRequest {
        checkout: checkout.into(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    })?;
    let mut pending = std::collections::VecDeque::from(session.start()?.outbound);
    while let Some(frame) = pending.pop_front() {
        let request = serde_json::from_slice(frame.as_bytes())?;
        if let Some(response) = handshake(provider, &request, checkout)? {
            pending.extend(receive(&mut *session, response.to_string())?.outbound);
        }
    }
    Ok(session)
}

fn verify_stream(provider: Provider, events: &[ProviderEvent]) {
    let find = |predicate: fn(&Event) -> bool| {
        let matches = events
            .iter()
            .enumerate()
            .filter(|(_, event)| predicate(&event.data))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{provider:?} {events:#?}");
        matches[0].0
    };
    let reasoning = find(|event| {
        matches!(event,
        Event::Reasoning { text, .. } if text == "Inspect the fixture")
    });
    let approval = find(|event| {
        matches!(event,
        Event::Approval { request_id: RequestId::Text(id), .. } if id == APPROVAL)
    });
    let delta = find(|event| matches!(event, Event::AssistantDelta { text } if text == ANSWER));
    let message = find(|event| matches!(event, Event::AssistantMessage { text } if text == ANSWER));
    let terminal = find(|event| matches!(event, Event::TurnFinished { .. }));
    assert_eq!(
        events[terminal].data,
        Event::TurnFinished {
            outcome: TurnOutcome::Completed
        }
    );
    assert!(reasoning < approval && approval < delta && delta < message && message < terminal);
    assert_eq!(
        events[terminal].location.session_id.as_deref(),
        Some(SESSION)
    );
    match provider {
        Provider::Codex | Provider::Claude => {
            let child = find(|event| matches!(event, Event::ChildActivity { child_id: None, .. }));
            assert!(reasoning < child && child < approval);
            let usage = find(|event| {
                matches!(
                    event,
                    Event::Usage {
                        input: Some(12),
                        output: Some(4),
                        cached_input: Some(2)
                    }
                )
            });
            assert!(delta < usage && usage < terminal);
        }
        Provider::Antigravity => {
            assert!(!events.iter().any(|event| matches!(
                event.data,
                Event::ChildActivity { .. } | Event::Usage { .. }
            )));
            find(|event| matches!(event, Event::Tool { name, .. } if name == "Delegate review"));
        }
    }
}

#[test]
fn successive_native_turns_keep_reasoning_approval_and_completion_separate() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    for provider in Provider::ALL {
        let mut session = start_session(provider, checkout.path())?;
        for turn in ["turn-1", "turn-2"] {
            let input = session.send(TurnInput {
                message_id: turn.into(),
                text: "Read the fixture".into(),
                attachments: Vec::new(),
            })?;
            let request = serde_json::from_slice(input.outbound[0].as_bytes())?;
            let mut events = input.events;
            let unknown = match provider {
                Provider::Codex => json!({"method":"future/event","params":{"threadId":SESSION}}),
                Provider::Claude => json!({"type":"future_event","session_id":SESSION}),
                Provider::Antigravity => json!({"jsonrpc":"2.0","method":"session/update",
                    "params":{"sessionId":SESSION,"update":{"sessionUpdate":"future_event"}}}),
            };
            let unknown = receive(&mut *session, unknown.to_string())?;
            assert!(matches!(
                unknown.events.as_slice(),
                [ProviderEvent {
                    data: Event::Diagnostic { .. },
                    ..
                }]
            ));
            for line in transcript(provider, &request, turn) {
                let update = receive(&mut *session, line)?;
                for event in &update.events {
                    if let Event::Approval { request_id, .. } = &event.data {
                        let response =
                            session.respond_approval(request_id, ApprovalDecision::Deny)?;
                        assert_eq!(response.outbound.len(), 1);
                    }
                }
                events.extend(update.events);
            }
            verify_stream(provider, &events);
            let location = &events.last().context("missing terminal event")?.location;
            match provider {
                Provider::Codex => assert_eq!(location.turn_id.as_deref(), Some(turn)),
                Provider::Claude => assert_eq!(location.item_id.as_deref(), Some(turn)),
                Provider::Antigravity => assert_eq!(location.turn_id, None),
            }
            assert!(
                session
                    .respond_approval(&RequestId::Text(APPROVAL.into()), ApprovalDecision::Deny)
                    .is_err()
            );
        }
    }
    Ok(())
}

#[test]
fn capability_sources_and_successful_fixture_evidence_have_distinct_statuses() {
    let mut capability = ResolvedCapability {
        descriptor: CapabilityDescriptor {
            subject: CapabilitySubject::Protocol {
                capability: Capability::Reasoning,
            },
            source: DeclaredSource::Adapter,
            availability: Availability::Available,
            observed: None,
            blocker: None,
            boundary: EvidenceBoundary::EquivalentConfiguration,
        },
        evidence: Some(Verification {
            id: "fixture".into(),
            checked_at: "2026-11-15".into(),
            origin: EvidenceOrigin::ProtocolFixture,
            result: ObservedResult::Succeeded,
            artifact_id: "provider-stream".into(),
            attachment_limits: None,
        }),
    };
    assert_eq!(capability.status(), CapabilityStatus::Unverified);
    capability.descriptor.source = DeclaredSource::Provider;
    assert_eq!(capability.status(), CapabilityStatus::Reported);
    capability.descriptor.observed = Some(ObservedResult::Succeeded);
    assert_eq!(capability.status(), CapabilityStatus::Observed);
    capability.descriptor.availability = Availability::Unavailable;
    assert_eq!(capability.status(), CapabilityStatus::Unavailable);
}
