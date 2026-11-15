use std::{collections::HashMap, ffi::OsString, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    diagnostics,
    process::TerminalOutcome,
    process_io::{OutputFrame, Stream},
};

use super::{
    AdapterUpdate, ApprovalDecision, AuthProbe, Authentication, Capability, CliAdapter, CliSession,
    Interrupt, ProbeKind, Provider, ProviderEvent, ProviderEventData, ProviderLocation,
    ReportedModel, RequestId, SessionIntent, SessionMode, SessionRequest, Transport, TurnInput,
    TurnOutcome, WireMessage,
};

const INITIALIZE: &str = "omniagent-initialize";

pub struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn transport(&self) -> Transport {
        Transport::ClaudeStreamJson
    }

    fn authentication_probe(&self) -> AuthProbe {
        AuthProbe {
            kind: ProbeKind::Command,
            arguments: args(["auth", "status", "--json"]),
        }
    }

    fn decode_authentication(&self, status: TerminalOutcome, output: &str) -> Authentication {
        let logged_in = serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|value| value["loggedIn"].as_bool());
        match (status, logged_in) {
            (TerminalOutcome::Exited(0), Some(true)) => Authentication::Authenticated,
            (TerminalOutcome::Exited(_), Some(false)) => Authentication::Required,
            _ => Authentication::Unknown,
        }
    }

    fn create_session(&self, request: SessionRequest) -> Result<Box<dyn CliSession>> {
        ensure!(
            request.checkout.is_absolute(),
            "Claude checkout must be absolute"
        );
        if let SessionIntent::Resume {
            provider_session_id,
        } = &request.intent
        {
            ensure!(
                Uuid::parse_str(provider_session_id).is_ok(),
                "Claude session ID must be a UUID"
            );
        }
        Ok(Box::new(ClaudeSession {
            request,
            initializing: false,
            ready: false,
            session_id: None,
            turn: None,
            pending: HashMap::new(),
            completed_turn: false,
        }))
    }
}

struct ClaudeSession {
    request: SessionRequest,
    initializing: bool,
    ready: bool,
    session_id: Option<String>,
    turn: Option<Turn>,
    pending: HashMap<RequestId, PendingInput>,
    completed_turn: bool,
}

#[derive(Default)]
struct Turn {
    interrupted: bool,
    saw_final_message: bool,
    message_ids: HashMap<Option<String>, String>,
    read_paths: HashMap<(Option<String>, String), String>,
}

enum PendingInput {
    Tool { input: Value },
    Question { input: Value },
}

impl ClaudeSession {
    fn event(&self, frame: &Value, data: ProviderEventData) -> ProviderEvent {
        ProviderEvent {
            location: ProviderLocation {
                session_id: self.session_id.clone(),
                item_id: string(&frame["message"], "id").or_else(|| string(frame, "uuid")),
                parent_item_id: string(frame, "parent_tool_use_id"),
                ..Default::default()
            },
            data,
        }
    }

    fn diagnostic(&self, message: &str) -> AdapterUpdate {
        events([self.event(
            &Value::Null,
            ProviderEventData::Diagnostic {
                message: diagnostics::preview(message),
            },
        )])
    }

    fn session_init(&mut self, frame: &Value) -> Result<AdapterUpdate> {
        let id = field(frame, "session_id")?;
        if let SessionIntent::Resume {
            provider_session_id,
        } = &self.request.intent
        {
            ensure!(
                provider_session_id == id,
                "Claude resumed an unexpected session"
            );
        }
        if let Some(cwd) = frame["cwd"].as_str() {
            ensure!(
                dunce::canonicalize(Path::new(cwd))?
                    == dunce::canonicalize(&self.request.checkout)?,
                "Claude reported an unexpected checkout"
            );
        }
        if let Some(mode) = frame["permissionMode"].as_str() {
            ensure!(
                mode == self.permission_mode(),
                "Claude reported an unexpected permission mode"
            );
        }
        self.session_id = Some(id.to_owned());
        Ok(events([self.event(
            frame,
            ProviderEventData::SessionReady {
                model: string(frame, "model"),
                effort: string(frame, "effort"),
            },
        )]))
    }

    fn permission_mode(&self) -> &'static str {
        match self.request.mode {
            SessionMode::ApprovedWrite => "default",
            SessionMode::ReadOnly | SessionMode::Plan => "plan",
        }
    }

    fn start_handshake(&mut self, frame: &Value) -> Result<AdapterUpdate> {
        let response = &frame["response"];
        if response["request_id"] != INITIALIZE || self.ready {
            return Ok(AdapterUpdate::default());
        }
        ensure!(self.initializing, "unexpected Claude control response");
        if response["subtype"] != "success" {
            let message = diagnostics::preview(
                response["error"]
                    .as_str()
                    .unwrap_or("Claude initialization failed"),
            );
            return Ok(events([
                self.event(frame, ProviderEventData::Error { message })
            ]));
        }
        let status = &response["response"];
        ensure!(status.is_object(), "Claude initialize response is invalid");
        if let Some(mode) = status["current_permission_mode"].as_str() {
            ensure!(
                mode == self.permission_mode(),
                "Claude did not establish the requested permission mode"
            );
        }
        self.ready = true;
        let mut capabilities = Vec::new();
        let models = status["models"].as_array().map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let id = string(model, "value")?;
                    Some(ReportedModel {
                        name: string(model, "displayName").unwrap_or_else(|| id.clone()),
                        canonical_id: string(model, "resolvedModel"),
                        input_modalities: None,
                        output_modalities: None,
                        effort_levels: strings(model, "supportedEffortLevels"),
                        default_effort: string(model, "defaultEffortLevel"),
                        selectable: model["isAvailable"].as_bool().or_else(|| {
                            model["disabledReason"]
                                .as_str()
                                .map(|reason| reason.is_empty())
                        }),
                        id,
                    })
                })
                .collect()
        });
        if models.is_some() {
            capabilities.push(Capability::ModelInventory);
        }
        let mut announced = vec![self.event(
            frame,
            ProviderEventData::Handshake {
                protocol: "claude-stream-json".into(),
                capabilities,
            },
        )];
        if let Some(models) = models {
            announced.push(self.event(frame, ProviderEventData::ModelInventory { models }));
        }
        Ok(events(announced))
    }

    fn stream(&mut self, frame: &Value) -> Result<AdapterUpdate> {
        let Some(turn) = self.turn.as_mut() else {
            return Ok(AdapterUpdate::default());
        };
        let parent = string(frame, "parent_tool_use_id");
        let stream = &frame["event"];
        if stream["type"] == "message_start" {
            if let Some(id) = string(&stream["message"], "id") {
                turn.message_ids.insert(parent, id);
            }
            return Ok(AdapterUpdate::default());
        }
        let message_id = turn.message_ids.get(&parent).cloned();
        let data = match stream["delta"]["type"].as_str() {
            Some("text_delta") => ProviderEventData::AssistantDelta {
                text: field(&stream["delta"], "text")?.to_owned(),
            },
            Some("thinking_delta") => ProviderEventData::Reasoning {
                text: field(&stream["delta"], "thinking")?.to_owned(),
                summary: false,
            },
            _ => return Ok(AdapterUpdate::default()),
        };
        let mut event = self.event(frame, data);
        event.location.item_id = message_id.or(event.location.item_id);
        Ok(events([event]))
    }

    fn assistant(&mut self, frame: &Value) -> Result<AdapterUpdate> {
        let Some(turn) = self.turn.as_mut() else {
            return Ok(AdapterUpdate::default());
        };
        let parent = string(frame, "parent_tool_use_id");
        let content = frame["message"]["content"]
            .as_array()
            .context("Claude assistant message has no content")?;
        let text = content
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<String>();
        if !text.is_empty() && parent.is_none() {
            turn.saw_final_message = true;
        }
        for block in content {
            if block["type"] == "tool_use"
                && block["name"] == "Read"
                && let (Some(id), Some(path)) =
                    (string(block, "id"), string(&block["input"], "file_path"))
            {
                turn.read_paths.insert((parent.clone(), id), path);
            }
        }
        let mut result = Vec::new();
        if parent.is_none()
            && let Some(model) = string(&frame["message"], "model")
        {
            result.push(self.event(
                frame,
                ProviderEventData::ModelObserved {
                    model: Some(model),
                    effort: None,
                },
            ));
        }
        if !text.is_empty() {
            result.push(self.event(frame, ProviderEventData::AssistantMessage { text }));
        }
        for block in content {
            let data = match block["type"].as_str() {
                Some("thinking") => ProviderEventData::Reasoning {
                    text: field(block, "thinking")?.to_owned(),
                    summary: false,
                },
                Some("tool_use") => ProviderEventData::Tool {
                    name: field(block, "name")?.to_owned(),
                    phase: "started".into(),
                },
                _ => continue,
            };
            let mut event = self.event(frame, data);
            event.location.item_id = string(block, "id").or(event.location.item_id);
            result.push(event);
        }
        Ok(events(result))
    }

    fn tool_results(&mut self, frame: &Value) -> AdapterUpdate {
        let mut results = Vec::new();
        let blocks = frame["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "tool_result");
        for block in blocks {
            let Some(id) = string(block, "tool_use_id") else {
                continue;
            };
            let path = self.turn.as_mut().and_then(|turn| {
                turn.read_paths
                    .remove(&(string(frame, "parent_tool_use_id"), id.clone()))
            });
            let failed = block["is_error"] == true;
            let event = |data| {
                let mut event = self.event(frame, data);
                event.location.item_id = Some(id.clone());
                event
            };
            results.push(event(ProviderEventData::Tool {
                name: "tool_result".into(),
                phase: if failed { "failed" } else { "completed" }.into(),
            }));
            if let Some(path) = path.filter(|_| !failed) {
                results.push(event(ProviderEventData::ContextRead {
                    path,
                    tool: "Read".into(),
                }));
            }
        }
        events(results)
    }

    fn request_input(&mut self, frame: &Value) -> Result<AdapterUpdate> {
        let id = RequestId::Text(field(frame, "request_id")?.to_owned());
        let request = &frame["request"];
        if self.turn.is_none() || request["subtype"] != "can_use_tool" {
            return outbound(control_error(
                id,
                "Unsupported or inactive Claude control request",
            )?);
        }
        ensure!(
            !self.pending.contains_key(&id),
            "duplicate Claude permission request"
        );
        let input = request["input"].clone();
        let name = field(request, "tool_name")?;
        let data = if name == "AskUserQuestion" {
            let form = input["questions"].clone();
            ensure!(
                form.is_array(),
                "Claude AskUserQuestion request has no questions"
            );
            self.pending
                .insert(id.clone(), PendingInput::Question { input });
            ProviderEventData::Question {
                request_id: id,
                form,
            }
        } else {
            self.pending
                .insert(id.clone(), PendingInput::Tool { input });
            ProviderEventData::Approval {
                request_id: id,
                operation: name.to_owned(),
                description: string(request, "decision_reason")
                    .map(|text| diagnostics::preview(&text)),
            }
        };
        let mut event = self.event(frame, data);
        event.location.item_id = string(request, "tool_use_id").or(event.location.item_id);
        event.location.parent_item_id =
            string(request, "parent_tool_use_id").or(event.location.parent_item_id);
        Ok(events([event]))
    }

    fn finish(&mut self, frame: &Value, outcome: TurnOutcome) -> AdapterUpdate {
        let Some(turn) = self.turn.take() else {
            return AdapterUpdate::default();
        };
        self.pending.clear();
        self.completed_turn = true;
        let mut reported = Vec::new();
        if let Some(usage) = frame.get("usage").filter(|usage| usage.is_object()) {
            reported.push(self.event(
                frame,
                ProviderEventData::Usage {
                    input: usage["input_tokens"].as_u64(),
                    output: usage["output_tokens"].as_u64(),
                    cached_input: usage["cache_read_input_tokens"].as_u64(),
                },
            ));
        }
        if outcome == TurnOutcome::Completed
            && !turn.saw_final_message
            && let Some(text) = string(frame, "result").filter(|text| !text.is_empty())
        {
            reported.push(self.event(frame, ProviderEventData::AssistantMessage { text }));
        }
        reported.push(self.event(frame, ProviderEventData::TurnFinished { outcome }));
        events(reported)
    }
}

impl CliSession for ClaudeSession {
    fn arguments(&self) -> Vec<OsString> {
        let tools = if self.request.mode == SessionMode::ApprovedWrite {
            "Read,Glob,Grep,Edit,Write"
        } else {
            "Read,Glob,Grep"
        };
        let mut arguments = args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--forward-subagent-text",
            "--safe-mode",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--permission-prompts",
            "host",
            "--permission-mode",
            self.permission_mode(),
            "--tools",
            tools,
        ]);
        if let Some(model) = &self.request.model {
            arguments.extend(args(["--model", model]));
        }
        if let Some(effort) = &self.request.effort {
            arguments.extend(args(["--effort", effort]));
        }
        if let SessionIntent::Resume {
            provider_session_id,
        } = &self.request.intent
        {
            arguments.extend(args(["--resume", provider_session_id]));
        }
        arguments
    }

    fn start(&mut self) -> Result<AdapterUpdate> {
        ensure!(!self.initializing, "Claude handshake already started");
        self.initializing = true;
        outbound(WireMessage::json(&json!({
            "type": "control_request", "request_id": INITIALIZE, "request": { "subtype": "initialize", "hooks": {} }
        }))?)
    }

    fn send(&mut self, input: TurnInput) -> Result<AdapterUpdate> {
        ensure!(self.ready, "Claude handshake has not completed");
        ensure!(self.turn.is_none(), "Claude already has a turn in progress");
        let message = WireMessage::json(&json!({
            "type": "user", "message": {"role": "user", "content": input.text}, "parent_tool_use_id": null
        }))?;
        self.completed_turn = false;
        self.turn = Some(Turn::default());
        Ok(AdapterUpdate {
            outbound: vec![message],
            events: vec![self.event(&Value::Null, ProviderEventData::TurnStarted)],
        })
    }

    fn receive(&mut self, frame: OutputFrame) -> Result<AdapterUpdate> {
        if frame.stream == Stream::Stderr {
            return Ok(self.diagnostic(&frame.data));
        }
        let Ok(frame) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(self.diagnostic("Malformed Claude protocol frame"));
        };
        if let (Some(expected), Some(actual)) = (&self.session_id, frame["session_id"].as_str())
            && actual != expected
        {
            return Ok(AdapterUpdate::default());
        }

        match frame["type"].as_str() {
            Some("control_response") => self.start_handshake(&frame),
            Some("control_request") => self.request_input(&frame),
            Some("system") if frame["subtype"] == "init" => self.session_init(&frame),
            Some("system") if frame["subtype"] == "compact_boundary" => {
                Ok(events([self.event(&frame, ProviderEventData::Compacted)]))
            }
            Some("system")
                if matches!(
                    frame["subtype"].as_str(),
                    Some("task_started" | "task_notification" | "task_progress")
                ) =>
            {
                let mut event = self.event(
                    &frame,
                    ProviderEventData::ChildActivity {
                        child_id: string(&frame, "task_id"),
                        status: string(&frame, "status").or_else(|| string(&frame, "subtype")),
                    },
                );
                event.location.parent_item_id = event
                    .location
                    .parent_item_id
                    .or_else(|| string(&frame, "tool_use_id"));
                Ok(events([event]))
            }
            Some("stream_event") => self.stream(&frame),
            Some("assistant") => self.assistant(&frame),
            Some("user") if self.turn.is_some() => Ok(self.tool_results(&frame)),
            Some("prompt_suggestion") if self.completed_turn => Ok(frame["suggestion"]
                .as_str()
                .filter(|text| !text.trim().is_empty())
                .map(|text| {
                    events([self.event(
                        &frame,
                        ProviderEventData::Suggestion {
                            text: text.to_owned(),
                        },
                    )])
                })
                .unwrap_or_default()),
            Some("result") => {
                let outcome = if self.turn.as_ref().is_some_and(|turn| turn.interrupted) {
                    TurnOutcome::Interrupted
                } else if frame["is_error"] == false && frame["subtype"] == "success" {
                    TurnOutcome::Completed
                } else {
                    TurnOutcome::Failed
                };
                let failure = (outcome == TurnOutcome::Failed && self.turn.is_some()).then(|| {
                    self.event(
                        &frame,
                        ProviderEventData::Error {
                            message: diagnostics::preview(
                                frame["errors"][0]
                                    .as_str()
                                    .or(frame["result"].as_str())
                                    .unwrap_or("Claude turn failed"),
                            ),
                        },
                    )
                });
                let mut update = self.finish(&frame, outcome);
                if let Some(error) = failure {
                    update.events.insert(0, error);
                }
                Ok(update)
            }
            Some("system" | "user" | "keep_alive" | "rate_limit_event") => {
                Ok(AdapterUpdate::default())
            }
            _ => Ok(self.diagnostic("Unrecognized Claude protocol frame")),
        }
    }

    fn respond_approval(
        &mut self,
        id: &RequestId,
        decision: ApprovalDecision,
    ) -> Result<AdapterUpdate> {
        ensure!(
            decision != ApprovalDecision::AllowOnce
                || self.request.mode == SessionMode::ApprovedWrite,
            "read-only Claude sessions cannot grant write or execution approval"
        );
        let Some(PendingInput::Tool { input }) = self.pending.get(id) else {
            anyhow::bail!("Claude approval is stale, absent or a question");
        };
        let reply = match decision {
            ApprovalDecision::AllowOnce => json!({ "behavior": "allow", "updatedInput": input }),
            ApprovalDecision::Deny | ApprovalDecision::Cancel => {
                json!({ "behavior": "deny", "message": "The user declined this permission", "interrupt": decision == ApprovalDecision::Cancel })
            }
        };
        let message = control_success(id, reply)?;
        self.pending.remove(id);
        outbound(message)
    }

    fn respond_question(&mut self, id: &RequestId, answer: Value) -> Result<AdapterUpdate> {
        let Some(PendingInput::Question { input }) = self.pending.get(id) else {
            anyhow::bail!("Claude question is stale or absent");
        };
        let answers = answer["answers"]
            .as_object()
            .context("Claude question response requires answers")?;
        let forms = input["questions"]
            .as_array()
            .context("invalid saved Claude question")?;
        ensure!(
            answers.len() == forms.len()
                && forms.iter().all(|question| {
                    question["question"]
                        .as_str()
                        .and_then(|key| answers.get(key))
                        .is_some_and(Value::is_string)
                }),
            "Claude question response does not match the active form"
        );
        let mut updated = input.clone();
        updated["answers"] = answer["answers"].clone();
        let message = control_success(id, json!({ "behavior": "allow", "updatedInput": updated }))?;
        self.pending.remove(id);
        outbound(message)
    }

    fn interrupt(&mut self) -> Result<Interrupt> {
        let turn = self.turn.as_mut().context("Claude has no active turn")?;
        ensure!(!turn.interrupted, "Claude interruption already requested");
        turn.interrupted = true;
        self.pending.clear();
        Ok(Interrupt {
            outbound: vec![WireMessage::json(&json!({
                "type": "control_request", "request_id": format!("interrupt-{}", Uuid::new_v4()),
                "request": { "subtype": "interrupt" }
            }))?],
            grace: Duration::from_secs(3),
        })
    }

    fn process_finished(&mut self, outcome: TerminalOutcome) -> AdapterUpdate {
        let stopped = self.turn.as_ref().is_some_and(|turn| turn.interrupted)
            || matches!(outcome, TerminalOutcome::Cancelled(_));
        self.finish(
            &Value::Null,
            if stopped {
                TurnOutcome::Interrupted
            } else {
                TurnOutcome::Failed
            },
        )
    }
}

fn args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.map(OsString::from).to_vec()
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Claude protocol field {key} is missing or invalid"))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

fn strings(value: &Value, key: &str) -> Option<Vec<String>> {
    value[key].as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn control_success(id: &RequestId, reply: Value) -> Result<WireMessage> {
    Ok(WireMessage::json(&json!({
        "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": reply }
    }))?)
}

fn control_error(id: RequestId, message: &str) -> Result<WireMessage> {
    Ok(WireMessage::json(&json!({
        "type": "control_response", "response": { "subtype": "error", "request_id": id, "error": message }
    }))?)
}

fn outbound(message: WireMessage) -> Result<AdapterUpdate> {
    Ok(AdapterUpdate {
        outbound: vec![message],
        events: Vec::new(),
    })
}

fn events(values: impl IntoIterator<Item = ProviderEvent>) -> AdapterUpdate {
    AdapterUpdate {
        outbound: Vec::new(),
        events: values.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(session: &mut dyn CliSession, frame: Value) -> Result<AdapterUpdate> {
        session.receive(OutputFrame {
            stream: Stream::Stdout,
            data: frame.to_string(),
        })
    }

    fn session(checkout: &Path) -> Result<Box<dyn CliSession>> {
        let mut adapter = ClaudeAdapter.create_session(SessionRequest {
            checkout: checkout.into(),
            intent: SessionIntent::Start,
            mode: SessionMode::Plan,
            model: Some("claude-haiku-5-5".into()),
            effort: None,
        })?;
        adapter.start()?;
        receive(
            &mut *adapter,
            json!({
                "type":"control_response", "response":{
                    "request_id":INITIALIZE, "subtype":"success", "response":{
                        "current_permission_mode":"plan",
                        "models":[{"value":"claude-haiku-5-5","displayName":"Haiku"}]
                    }
                }
            }),
        )?;
        adapter.send(TurnInput {
            message_id: "turn".into(),
            text: "test".into(),
        })?;
        receive(
            &mut *adapter,
            json!({
                "type":"system", "subtype":"init", "session_id":"session", "permissionMode":"plan"
            }),
        )?;
        Ok(adapter)
    }

    #[test]
    fn correlates_subagent_streams_and_preserves_missing_child_identity() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut adapter = session(temp.path())?;
        receive(
            &mut *adapter,
            json!({
                "type":"stream_event", "session_id":"session", "parent_tool_use_id":"task-tool",
                "event":{"type":"message_start","message":{"id":"child-message"}}
            }),
        )?;
        let delta = receive(
            &mut *adapter,
            json!({
                "type":"stream_event", "session_id":"session", "parent_tool_use_id":"task-tool",
                "event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"reasoning"}}
            }),
        )?;
        assert_eq!(
            delta.events[0].location.parent_item_id.as_deref(),
            Some("task-tool")
        );
        assert_eq!(
            delta.events[0].location.item_id.as_deref(),
            Some("child-message")
        );
        assert!(matches!(
            delta.events[0].data,
            ProviderEventData::Reasoning { .. }
        ));
        let child = receive(
            &mut *adapter,
            json!({
                "type":"system","subtype":"task_started","session_id":"session","tool_use_id":"task-tool"
            }),
        )?;
        assert_eq!(
            child.events[0].location.parent_item_id.as_deref(),
            Some("task-tool")
        );
        assert!(matches!(
            child.events[0].data,
            ProviderEventData::ChildActivity { child_id: None, .. }
        ));
        let interrupt = adapter.interrupt()?;
        let interrupt: Value = serde_json::from_slice(interrupt.outbound[0].as_bytes())?;
        assert_eq!(interrupt["request"]["subtype"], "interrupt");
        let cancelled = adapter.process_finished(TerminalOutcome::Cancelled(1));
        assert_eq!(
            cancelled.events[0].data,
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Interrupted
            }
        );
        assert!(
            adapter
                .process_finished(TerminalOutcome::Exited(0))
                .events
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn reports_successful_reads_in_the_original_parent_scope() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut adapter = session(temp.path())?;
        let started = receive(
            &mut *adapter,
            json!({"type":"assistant","session_id":"session",
            "parent_tool_use_id":"child","message":{"content":[
                {"type":"tool_use","id":"okay","name":"Read","input":{"file_path":"first.txt"}},
                {"type":"tool_use","id":"failed","name":"Read","input":{"file_path":"second.txt"}}
            ]}}),
        )?;
        assert_eq!(started.events.len(), 2);
        let done = receive(
            &mut *adapter,
            json!({"type":"user","session_id":"session","parent_tool_use_id":"child",
            "message":{"content":[{"type":"tool_result","tool_use_id":"okay","content":"ok"},
            {"type":"tool_result","tool_use_id":"failed","is_error":true}]}}),
        )?;
        let reads = done
            .events
            .iter()
            .filter(|event| matches!(event.data, ProviderEventData::ContextRead { .. }))
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].location.parent_item_id.as_deref(), Some("child"));
        assert_eq!(
            reads[0].data,
            ProviderEventData::ContextRead {
                path: "first.txt".into(),
                tool: "Read".into()
            }
        );
        Ok(())
    }

    #[test]
    fn returns_each_question_answer_to_the_originating_request_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut adapter = session(temp.path())?;
        let form = json!([{"question":"Which checks?", "multiSelect":true,
            "options":[{"label":"Unit"},{"label":"Desktop"}]}]);
        let received = receive(
            &mut *adapter,
            json!({
                "type":"control_request", "request_id":"question-1", "session_id":"session",
                "request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"form",
                    "input":{"questions":form}}
            }),
        )?;
        assert!(matches!(
            received.events[0].data,
            ProviderEventData::Question { .. }
        ));
        let id = RequestId::Text("question-1".into());
        assert!(
            adapter
                .respond_approval(&id, ApprovalDecision::Deny)
                .is_err()
        );
        let answers = json!({"answers":{"Which checks?":"Unit, Desktop"}});
        let reply = adapter.respond_question(&id, answers.clone())?;
        let reply: Value = serde_json::from_slice(reply.outbound[0].as_bytes())?;
        assert_eq!(reply["response"]["request_id"], "question-1");
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["answers"],
            answers["answers"]
        );
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["questions"],
            form
        );
        assert!(adapter.respond_question(&id, answers).is_err());
        Ok(())
    }
}
