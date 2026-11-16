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
    ReportedModel, RequestId, SessionIntent, SessionMode, SessionPlugin, SessionRequest, Transport,
    TurnInput, TurnOutcome, WireMessage,
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
        ensure!(
            !matches!(request.intent, SessionIntent::Fork { .. }),
            "Claude streaming adapter has no qualified exact-turn fork route"
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
            permission_mode_reported: false,
            session_id: None,
            turn: None,
            pending: HashMap::new(),
            suggestion_enabled: false,
            suggestion_turn_started: false,
            suggestion: None,
            goal_command: None,
            question_route: Default::default(),
        }))
    }
}

struct ClaudeSession {
    request: SessionRequest,
    initializing: bool,
    ready: bool,
    permission_mode_reported: bool,
    session_id: Option<String>,
    turn: Option<Turn>,
    pending: HashMap<RequestId, PendingInput>,
    suggestion_enabled: bool,
    suggestion_turn_started: bool,
    suggestion: Option<super::suggestions::SuggestionSource>,
    goal_command: Option<bool>,
    question_route: super::questions::QuestionRoute,
}

#[derive(Default)]
struct Turn {
    message_id: String,
    interrupted: bool,
    saw_final_message: bool,
    last_parent_text: Option<String>,
    last_parent_item_id: Option<String>,
    plan_review: Option<ProviderLocation>,
    message_ids: HashMap<Option<String>, String>,
    tool_evidence: HashMap<(Option<String>, String), ProviderEventData>,
}

enum PendingInput {
    Tool {
        input: Value,
    },
    Question {
        input: Value,
        form: super::questions::QuestionForm,
    },
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
        self.frame_diagnostic(&Value::Null, message)
    }

    fn frame_diagnostic(&self, frame: &Value, message: &str) -> AdapterUpdate {
        events([self.event(
            frame,
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
        let fresh =
            self.session_id.is_none() && matches!(self.request.intent, SessionIntent::Start);
        self.session_id = Some(id.to_owned());
        let mut observations = vec![self.event(
            frame,
            ProviderEventData::SessionReady {
                model: string(frame, "model"),
                effort: string(frame, "effort"),
            },
        )];
        if fresh {
            observations.push(self.event(frame, ProviderEventData::FreshSessionOpened));
        }
        if let Some(tools) = frame["tools"].as_array()
            && tools.iter().all(Value::is_string)
        {
            observations.push(
                self.event(
                    frame,
                    ProviderEventData::DelegationControlsReported {
                        controls: super::DelegationControls::ClaudeSessionToolList {
                            agent_tool_available: tools
                                .iter()
                                .any(|tool| matches!(tool.as_str(), Some("Agent" | "Task"))),
                        },
                    },
                ),
            );
        }
        let skills = strings(frame, "skills");
        let plugins = frame["plugins"].as_array().map(|plugins| {
            plugins
                .iter()
                .filter_map(|plugin| {
                    Some(SessionPlugin {
                        name: string(plugin, "name")?,
                        path: string(plugin, "path").map(Into::into),
                    })
                })
                .collect()
        });
        if skills.is_some() || plugins.is_some() {
            observations.push(self.event(
                frame,
                ProviderEventData::SessionCustomizationsReported { skills, plugins },
            ));
        }
        Ok(events(observations))
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
            self.permission_mode_reported = true;
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
        if self.request.mode == SessionMode::Plan && status["current_permission_mode"] == "plan" {
            announced.push(self.event(
                frame,
                ProviderEventData::PlanningModeConfirmed {
                    mode: super::planning::PlanningMode::ClaudePermissionMode,
                },
            ));
        }
        if let Some(models) = models {
            announced.push(self.event(frame, ProviderEventData::ModelInventory { models }));
        }
        if let Some(commands) = status["commands"].as_array()
            && commands.iter().all(|command| command["name"].is_string())
        {
            let available = commands
                .iter()
                .any(|command| command["name"] == "goal" && command["builtin"] == true);
            self.goal_command = Some(available);
            announced.push(self.event(
                frame,
                ProviderEventData::NativeGoalCommandReported { available },
            ));
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
        let delta = &stream["delta"];
        let data = match delta["type"].as_str() {
            Some("text_delta") => {
                string(delta, "text").map(|text| ProviderEventData::AssistantDelta { text })
            }
            Some("thinking_delta") => {
                string(delta, "thinking").map(|text| ProviderEventData::Reasoning {
                    text,
                    summary: false,
                })
            }
            Some("input_json_delta" | "signature_delta") => return Ok(AdapterUpdate::default()),
            _ if stream["type"] == "content_block_delta" => None,
            _ => return Ok(AdapterUpdate::default()),
        }
        .unwrap_or_else(|| ProviderEventData::Diagnostic {
            message: "Malformed or unsupported Claude content delta".into(),
        });
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
            turn.last_parent_text = Some(text.clone());
            turn.last_parent_item_id =
                string(&frame["message"], "id").or_else(|| string(frame, "uuid"));
        }
        for block in content.iter().filter(|block| block["type"] == "tool_use") {
            let evidence = match block["name"].as_str() {
                Some("Read") => string(&block["input"], "file_path").map(|path| {
                    ProviderEventData::ContextRead {
                        path,
                        tool: "Read".into(),
                    }
                }),
                Some("Skill") => string(&block["input"], "skill")
                    .map(|name| ProviderEventData::SkillUsed { name }),
                _ => None,
            };
            if let (Some(id), Some(evidence)) = (string(block, "id"), evidence) {
                turn.tool_evidence.insert((parent.clone(), id), evidence);
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
            let evidence = self.turn.as_mut().and_then(|turn| {
                turn.tool_evidence
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
            if let Some(evidence) = evidence.filter(|_| !failed) {
                results.push(event(evidence));
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
        if name == "ExitPlanMode" && self.request.mode == SessionMode::Plan {
            let mut event = self.event(
                frame,
                ProviderEventData::PlanReviewRequested {
                    request_id: id.clone(),
                },
            );
            event.location.item_id = string(request, "tool_use_id").or(event.location.item_id);
            event.location.parent_item_id =
                string(request, "parent_tool_use_id").or(event.location.parent_item_id);
            if event.location.parent_item_id.is_none() {
                self.turn.as_mut().expect("checked active turn").plan_review =
                    Some(event.location.clone());
            }
            let response = control_success(
                &id,
                json!({
                    "behavior":"deny",
                "message":format!(
                    "This turn stays in Plan Mode. Return the complete proposed plan in your response without editing files. {}",
                    super::planning::PROPOSAL_FORMAT
                ),
                }),
            )?;
            return Ok(AdapterUpdate {
                outbound: vec![response],
                events: vec![event],
            });
        }
        let data = if name == "AskUserQuestion" {
            if !self.question_route.is_native() {
                return outbound(control_error(
                    id,
                    "this attempt collects questions at turn completion",
                )?);
            }
            let form = input["questions"].clone();
            let normalized = match super::questions::QuestionForm::claude(&form) {
                Ok(normalized) => normalized,
                Err(error) => {
                    return Ok(AdapterUpdate {
                        outbound: vec![control_error(
                            id.clone(),
                            "unsupported Claude question form",
                        )?],
                        events: vec![self.event(
                            frame,
                            ProviderEventData::QuestionUnavailable {
                                request_id: Some(id),
                                reason: diagnostics::preview(&error.to_string()),
                            },
                        )],
                    });
                }
            };
            self.pending.insert(
                id.clone(),
                PendingInput::Question {
                    input,
                    form: normalized.clone(),
                },
            );
            ProviderEventData::Question {
                request_id: id,
                form,
                normalized: Some(normalized),
                route: self.question_route,
            }
        } else {
            self.pending
                .insert(id.clone(), PendingInput::Tool { input });
            ProviderEventData::Approval {
                request_id: id,
                operation: name.to_owned(),
                description: string(request, "decision_reason")
                    .map(|text| diagnostics::preview(&text)),
                prompt: None,
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
        if self.suggestion_enabled
            && outcome == TurnOutcome::Completed
            && frame["num_turns"].as_u64().is_some_and(|turns| turns > 0)
            && let (Some(session_id), Some(result_id)) = (&self.session_id, string(frame, "uuid"))
        {
            self.suggestion = Some(super::suggestions::SuggestionSource {
                provider_session_id: session_id.clone(),
                provider_turn_id: None,
                provider_result_id: Some(result_id),
                message_id: turn.message_id.clone(),
            });
        }
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
        if outcome == TurnOutcome::Completed {
            let final_text = string(frame, "result").filter(|text| !text.trim().is_empty());
            if !turn.saw_final_message
                && let Some(text) = &final_text
            {
                reported.push(self.event(
                    frame,
                    ProviderEventData::AssistantMessage { text: text.clone() },
                ));
            }
            if let Some(text) = &final_text {
                let mut event = self.event(
                    frame,
                    ProviderEventData::AssistantFinal { text: text.clone() },
                );
                if turn.last_parent_text.as_ref() == Some(text) {
                    event.location.item_id =
                        turn.last_parent_item_id.clone().or(event.location.item_id);
                }
                reported.push(event);
            }
            if let Some(location) = turn.plan_review
                && let Some(text) = final_text
                    .as_deref()
                    .and_then(super::planning::marked_proposal)
                    .or_else(|| {
                        turn.last_parent_text
                            .as_deref()
                            .and_then(super::planning::marked_proposal)
                    })
            {
                reported.push(ProviderEvent {
                    location,
                    data: ProviderEventData::PlanProposed {
                        text,
                        source: super::planning::PlanSource::ClaudeExitPlanMode,
                    },
                });
            }
        }
        reported.push(self.event(frame, ProviderEventData::TurnFinished { outcome }));
        events(reported)
    }
}

impl CliSession for ClaudeSession {
    fn reported_cli_permission(&self) -> Option<&'static str> {
        self.permission_mode_reported
            .then(|| self.permission_mode())
    }

    fn enable_native_suggestions(&mut self) {
        self.suggestion_enabled = !self.initializing && self.permission_mode() != "plan";
    }

    fn suggestion_pending(&self) -> bool {
        self.suggestion.is_some()
    }

    fn abandon_suggestion(
        &mut self,
        outcome: super::suggestions::SuggestionOutcome,
    ) -> Option<ProviderEvent> {
        self.suggestion
            .take()
            .map(|source| source.settlement(outcome))
    }

    fn native_goal_support(&self) -> super::goal::NativeGoalSupport {
        super::goal::NativeGoalSupport::from_advertisement(self.goal_command)
    }

    fn set_question_route(&mut self, route: super::questions::QuestionRoute) {
        self.question_route = route;
    }

    fn attachment_encoding(
        &self,
        format: crate::attachments::AttachmentFormat,
    ) -> Option<crate::attachments::NativeAttachmentEncoding> {
        use crate::attachments::{AttachmentFormat, NativeAttachmentEncoding};
        if format.is_image() {
            Some(NativeAttachmentEncoding::ClaudeImage)
        } else {
            (format == AttachmentFormat::Pdf).then_some(NativeAttachmentEncoding::ClaudeDocument)
        }
    }
    fn arguments(&self) -> Vec<OsString> {
        let tools = match self.request.mode {
            SessionMode::ApprovedWrite => "Read,Glob,Grep,Edit,Write",
            SessionMode::Plan => "Read,Glob,Grep,AskUserQuestion,ExitPlanMode",
            SessionMode::ReadOnly => "Read,Glob,Grep",
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
            "--include-hook-events",
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
        if self.request.mode == SessionMode::Plan {
            arguments.extend(args(["--permission-prompt-tool", "stdio"]));
        }
        if self.suggestion_enabled {
            arguments.push("--prompt-suggestions".into());
        }
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
        let content = if input.attachments.is_empty() {
            Value::String(input.text)
        } else {
            let mut blocks = Vec::new();
            if !input.text.trim().is_empty() {
                blocks.push(json!({"type":"text", "text": input.text}));
            }
            for attachment in &input.attachments {
                blocks.push(attachment.block(Provider::Claude)?);
            }
            json!(blocks)
        };
        let message = WireMessage::json(&json!({
            "type": "user", "message": {"role": "user", "content": content}, "parent_tool_use_id": null
        }))?;
        ensure!(
            !self.suggestion_enabled || !self.suggestion_turn_started,
            "Claude suggestion attribution requires one supervised turn per invocation"
        );
        self.suggestion_turn_started = true;
        self.turn = Some(Turn {
            message_id: input.message_id,
            ..Turn::default()
        });
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
            Some("control_cancel_request") => {
                let id = RequestId::Text(field(&frame, "request_id")?.to_owned());
                if matches!(self.pending.get(&id), Some(PendingInput::Question { .. })) {
                    self.pending.remove(&id);
                    Ok(events([self.event(
                        &frame,
                        ProviderEventData::QuestionClosed { request_id: id },
                    )]))
                } else {
                    Ok(AdapterUpdate::default())
                }
            }
            Some("system") if frame["subtype"] == "init" => self.session_init(&frame),
            Some("system")
                if matches!(
                    frame["subtype"].as_str(),
                    Some("hook_started" | "hook_progress" | "hook_response")
                ) =>
            {
                if let Some(session) = &self.session_id
                    && frame["session_id"].as_str() != Some(session)
                {
                    return Ok(AdapterUpdate::default());
                }
                let Some(trigger) = string(&frame, "hook_event") else {
                    return Ok(AdapterUpdate::default());
                };
                let status = match frame["subtype"].as_str() {
                    Some("hook_started") => "started".into(),
                    Some("hook_progress") => "in_progress".into(),
                    _ => string(&frame, "outcome").unwrap_or_else(|| "finished".into()),
                };
                let mut event = self.event(
                    &frame,
                    ProviderEventData::HookObserved {
                        trigger,
                        name: string(&frame, "hook_name"),
                        source_path: None,
                        status,
                    },
                );
                event.location.item_id = string(&frame, "hook_id");
                Ok(events([event]))
            }
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
                        details: None,
                    },
                );
                event.location.parent_item_id = event
                    .location
                    .parent_item_id
                    .or_else(|| string(&frame, "tool_use_id"));
                Ok(events([event]))
            }
            Some("stream_event") => self.stream(&frame),
            Some("assistant") => Ok(self.assistant(&frame).unwrap_or_else(|_| {
                self.frame_diagnostic(&frame, "Malformed Claude assistant message")
            })),
            Some("user") if self.turn.is_some() => Ok(self.tool_results(&frame)),
            Some("prompt_suggestion") => {
                if self.suggestion.as_ref().is_none_or(|source| {
                    frame["session_id"].as_str() != Some(&source.provider_session_id)
                }) {
                    return Ok(AdapterUpdate::default());
                }
                let outcome =
                    super::suggestions::SuggestionOutcome::from_text(&frame["suggestion"]);
                Ok(events(self.abandon_suggestion(outcome)))
            }
            Some("result") if string(&frame, "parent_tool_use_id").is_some() => {
                Ok(AdapterUpdate::default())
            }
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
            _ => Ok(self.frame_diagnostic(&frame, "Unrecognized Claude protocol frame")),
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
        use super::questions::{QuestionAction, QuestionReply};
        let Some(PendingInput::Question { input, form }) = self.pending.get(id) else {
            anyhow::bail!("Claude question is stale or absent");
        };
        let reply: QuestionReply = if answer.get("action").is_some() {
            serde_json::from_value(answer)?
        } else {
            QuestionReply {
                action: QuestionAction::Accept,
                values: answer["answers"]
                    .as_object()
                    .context("Claude response requires answers")?
                    .clone(),
            }
        };
        form.validate(&reply)?;
        let response = if reply.action == QuestionAction::Accept {
            let answers: serde_json::Map<_, _> = reply
                .values
                .iter()
                .map(|(field, value)| {
                    if let Value::Array(selected) = value {
                        let labels = selected
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>();
                        (field.clone(), Value::String(labels.join(", ")))
                    } else {
                        (field.clone(), value.clone())
                    }
                })
                .collect();
            let mut updated = input.clone();
            updated["answers"] = Value::Object(answers);
            json!({ "behavior": "allow", "updatedInput": updated })
        } else {
            json!({
                "behavior": "deny",
                "message": if reply.action == QuestionAction::Decline { "The user declined the question" } else { "The user cancelled the question" },
            })
        };
        let message = control_success(id, response)?;
        self.pending.remove(id);
        Ok(super::questions::encoded_reply(id, reply, message))
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
            attachments: Vec::new(),
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
    fn completed_result_identifies_the_observed_final_response() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut adapter = session(temp.path())?;
        for (id, text) in [("working", "Checking"), ("answer", "Done")] {
            let update = receive(
                &mut *adapter,
                json!({
                    "type":"assistant", "session_id":"session", "message":{
                        "id":id, "content":[{"type":"text","text":text}]
                    }
                }),
            )?;
            assert!(
                update
                    .events
                    .iter()
                    .all(|event| !matches!(event.data, ProviderEventData::AssistantFinal { .. }))
            );
        }
        let update = receive(
            &mut *adapter,
            json!({
                "type":"result", "subtype":"success", "is_error":false, "session_id":"session",
                "uuid":"result", "result":"Done"
            }),
        )?;
        assert!(update.events.iter().any(|event| event.location.item_id.as_deref() == Some("answer") &&
            matches!(&event.data, ProviderEventData::AssistantFinal { text } if text == "Done")));
        Ok(())
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
        let malformed = receive(
            &mut *adapter,
            json!({
                "type":"stream_event", "session_id":"session", "parent_tool_use_id":"task-tool",
                "event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":false}}
            }),
        )?;
        assert_eq!(
            malformed.events[0].location.item_id.as_deref(),
            Some("child-message")
        );
        assert!(matches!(
            malformed.events[0].data,
            ProviderEventData::Diagnostic { .. }
        ));
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
    fn reports_successful_reads_and_skill_calls_in_the_original_parent_scope() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut adapter = session(temp.path())?;
        let started = receive(
            &mut *adapter,
            json!({"type":"assistant","session_id":"session",
            "parent_tool_use_id":"child","message":{"content":[
                {"type":"tool_use","id":"okay","name":"Read","input":{"file_path":"first.txt"}},
                {"type":"tool_use","id":"failed","name":"Read","input":{"file_path":"second.txt"}},
                {"type":"tool_use","id":"skill","name":"Skill","input":{"skill":"example:format"}},
                {"type":"tool_use","id":"failed-skill","name":"Skill","input":{"skill":"unknown"}}
            ]}}),
        )?;
        assert_eq!(started.events.len(), 4);
        let done = receive(
            &mut *adapter,
            json!({"type":"user","session_id":"session","parent_tool_use_id":"child",
            "message":{"content":[{"type":"tool_result","tool_use_id":"okay","content":"ok"},
            {"type":"tool_result","tool_use_id":"failed","is_error":true},
            {"type":"tool_result","tool_use_id":"skill","content":"ok"},
            {"type":"tool_result","tool_use_id":"failed-skill","is_error":true}]}}),
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
        let used: Vec<_> = done
            .events
            .iter()
            .filter_map(|event| match &event.data {
                ProviderEventData::SkillUsed { name } => {
                    Some((name.as_str(), event.location.parent_item_id.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(used, [("example:format", Some("child"))]);
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
        let answer = json!({"action":"accept","values":{"Which checks?":["Unit","Desktop"]}});
        assert!(
            adapter
                .respond_question(
                    &id,
                    json!({"action":"accept","values":{"Which checks?":["Unit","Unit"]}})
                )
                .is_err()
        );
        let reply = adapter.respond_question(&id, answer.clone())?;
        let reply: Value = serde_json::from_slice(reply.outbound[0].as_bytes())?;
        assert_eq!(reply["response"]["request_id"], "question-1");
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["answers"],
            json!({"Which checks?":"Unit, Desktop"})
        );
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["questions"],
            form
        );
        assert!(adapter.respond_question(&id, answer).is_err());
        Ok(())
    }
}
