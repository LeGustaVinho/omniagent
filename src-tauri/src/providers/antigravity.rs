use std::{collections::HashMap, ffi::OsString, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::{
    diagnostics,
    process::TerminalOutcome,
    process_io::{OutputFrame, Stream},
};

use super::{
    AdapterUpdate, ApprovalDecision, AuthProbe, Authentication, Capability, CliAdapter, CliSession,
    Interrupt, ProbeKind, Provider, ProviderEvent, ProviderEventData, ProviderLocation,
    ReportedModel, RequestId, SessionChoice, SessionIntent, SessionMode, SessionOption,
    SessionRequest, Transport, TurnInput, TurnOutcome, WireMessage,
    composer_customizations::ComposerCommand,
    rpc::{self, Rpc},
};

const ACP_VERSION: u64 = 1;
const AUTH_REQUIRED: i64 = -32000;

pub struct AntigravityAdapter;

impl CliAdapter for AntigravityAdapter {
    fn provider(&self) -> Provider {
        Provider::Antigravity
    }
    fn transport(&self) -> Transport {
        Transport::AgentClientProtocol
    }

    fn authentication_probe(&self) -> AuthProbe {
        AuthProbe {
            kind: ProbeKind::SessionHandshake,
            arguments: arguments(),
        }
    }

    fn decode_authentication(&self, _status: TerminalOutcome, output: &str) -> Authentication {
        if output
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|frame| frame["error"]["code"].as_i64() == Some(AUTH_REQUIRED))
        {
            Authentication::Required
        } else {
            // A handshake can advertise a login route without proving credentials.
            Authentication::Unknown
        }
    }

    fn create_session(&self, request: SessionRequest) -> Result<Box<dyn CliSession>> {
        ensure!(
            !matches!(request.intent, SessionIntent::Fork { .. }),
            "Antigravity ACP has no qualified exact-turn fork route"
        );
        ensure!(
            request.checkout.is_absolute(),
            "ACP checkout must be absolute"
        );
        Ok(Box::new(AcpSession {
            request,
            rpc: Rpc::new(true),
            handshake_started: false,
            ready: false,
            session_id: None,
            configuration: Configuration::default(),
            settings: [Setting::Model, Setting::Effort, Setting::Mode].into_iter(),
            turn: None,
            permissions: HashMap::new(),
            questions: HashMap::new(),
            question_route: Default::default(),
            available_commands: None,
            composer_commands: None,
            plan_command_available: false,
            goal_command: None,
            bridge: None,
            prompt_capabilities: Value::Null,
            output_schema: None,
        }))
    }
}

fn arguments() -> Vec<OsString> {
    [
        "acp",
        "--stdio",
        "--include-default-customizations=false",
        "--sandbox",
    ]
    .map(OsString::from)
    .to_vec()
}

enum Request {
    Initialize,
    Open,
    Configure {
        setting: Setting,
        value: String,
        modern: bool,
    },
    Prompt,
}

#[derive(Clone, Copy)]
enum Setting {
    Model,
    Effort,
    Mode,
}

impl Setting {
    fn category(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "thought_level",
            Self::Mode => "mode",
        }
    }
}

#[derive(Default)]
struct Configuration {
    options: Vec<Value>,
    models: Value,
    modes: Value,
}

impl Configuration {
    fn option(&self, setting: Setting) -> Option<&Value> {
        self.options.iter().find(|option| {
            option["type"] == "select"
                && (option["category"] == setting.category() || option["id"] == setting.category())
        })
    }

    fn current(&self, setting: Setting) -> Option<&str> {
        self.option(setting)
            .and_then(|item| item["currentValue"].as_str())
            .or_else(|| match setting {
                Setting::Model => self.models["currentModelId"].as_str(),
                Setting::Mode => self.modes["currentModeId"].as_str(),
                Setting::Effort => None,
            })
    }

    fn update(&mut self, result: &Value) {
        if let Some(options) = result["configOptions"].as_array() {
            self.options = options.clone();
        }
        if result["models"].is_object() {
            self.models = result["models"].clone();
        }
        if result["modes"].is_object() {
            self.modes = result["modes"].clone();
        }
    }

    fn set_legacy(&mut self, setting: Setting, value: &str) {
        match setting {
            Setting::Model => self.models["currentModelId"] = json!(value),
            Setting::Mode => self.modes["currentModeId"] = json!(value),
            Setting::Effort => {}
        }
    }

    fn reported_models(&self) -> Option<Vec<ReportedModel>> {
        let modern = self.option(Setting::Model);
        let (options, id_key) = if let Some(option) = modern {
            (&option["options"], "value")
        } else {
            (&self.models["availableModels"], "modelId")
        };
        Some(
            choices(options)
                .filter_map(|model| {
                    let id = string(model, id_key)?;
                    Some(ReportedModel {
                        name: string(model, "name").unwrap_or_else(|| id.clone()),
                        id,
                        canonical_id: None,
                        input_modalities: None,
                        output_modalities: None,
                        // Options for thinking may change after selecting another model.
                        effort_levels: None,
                        default_effort: None,
                        selectable: Some(true),
                    })
                })
                .collect(),
        )
        .filter(|models: &Vec<_>| !models.is_empty())
    }

    fn reported_options(&self) -> Vec<SessionOption> {
        self.options
            .iter()
            .filter(|option| option["type"] == "select")
            .filter_map(|option| {
                let id = string(option, "id")?;
                Some(SessionOption {
                    name: string(option, "name").unwrap_or_else(|| id.clone()),
                    id,
                    category: string(option, "category"),
                    current: string(option, "currentValue"),
                    choices: choices(&option["options"])
                        .filter_map(|choice| {
                            let value = string(choice, "value")?;
                            Some(SessionChoice {
                                name: string(choice, "name").unwrap_or_else(|| value.clone()),
                                value,
                            })
                        })
                        .collect(),
                })
            })
            .collect()
    }
}

struct Turn {
    text: String,
    response_item_id: Option<String>,
    mixed_response: bool,
    cancel_requested: bool,
    tools: HashMap<String, ReportedTool>,
    asked_for_plan: bool,
}

#[derive(Default)]
struct ReportedTool {
    kind: Option<String>,
    name: Option<String>,
    paths: Vec<String>,
}

struct AcpSession {
    request: SessionRequest,
    output_schema: Option<Value>,
    rpc: Rpc<Request>,
    handshake_started: bool,
    ready: bool,
    session_id: Option<String>,
    configuration: Configuration,
    settings: std::array::IntoIter<Setting, 3>,
    turn: Option<Turn>,
    permissions: HashMap<RequestId, Vec<PermissionOption>>,
    questions: HashMap<RequestId, super::questions::QuestionForm>,
    question_route: super::questions::QuestionRoute,
    available_commands: Option<(String, Vec<ComposerCommand>)>,
    composer_commands: Option<Vec<ComposerCommand>>,
    plan_command_available: bool,
    goal_command: Option<bool>,
    bridge: Option<crate::mcp_bridge::BridgeLaunch>,
    prompt_capabilities: Value,
}

struct PermissionOption {
    id: String,
    kind: String,
}

impl AcpSession {
    fn command_observations(&mut self, commands: Vec<ComposerCommand>) -> [ProviderEvent; 2] {
        let names: Vec<_> = commands
            .iter()
            .map(|command| command.name.clone())
            .collect();
        self.composer_commands = Some(commands);
        self.plan_command_available = names.iter().any(|name| name == "plan");
        let available = names.iter().any(|name| name == "goal");
        self.goal_command = Some(available);
        [
            self.event(ProviderEventData::AvailableCommands { names }),
            self.event(ProviderEventData::NativeGoalCommandReported { available }),
        ]
    }

    fn event(&self, data: ProviderEventData) -> ProviderEvent {
        ProviderEvent {
            location: ProviderLocation {
                session_id: self.session_id.clone(),
                ..Default::default()
            },
            data,
        }
    }

    fn diagnostic(&self, message: &str) -> AdapterUpdate {
        events([self.event(ProviderEventData::Diagnostic {
            message: diagnostics::preview(message),
        })])
    }

    fn update_diagnostic(&self, update: &Value, message: &str) -> AdapterUpdate {
        let mut event = self.event(ProviderEventData::Diagnostic {
            message: diagnostics::preview(message),
        });
        event.location.item_id =
            string(update, "toolCallId").or_else(|| string(update, "messageId"));
        events([event])
    }

    fn open_session(&mut self, result: &Value) -> Result<AdapterUpdate> {
        ensure!(
            result["protocolVersion"].as_u64() == Some(ACP_VERSION),
            "incompatible ACP protocol version"
        );
        let agent = &result["agentCapabilities"];
        let can_resume = agent
            .pointer("/sessionCapabilities/resume")
            .is_some_and(Value::is_object);
        let can_load = agent["loadSession"] == true;
        let mut params = json!({ "cwd": self.request.checkout, "mcpServers": [] });
        if let Some(bridge) = &self.bridge {
            params["mcpServers"] = bridge.acp_servers();
        }
        let method = match &self.request.intent {
            SessionIntent::Start => "session/new",
            SessionIntent::Fork { .. } => {
                bail!("Antigravity ACP has no qualified exact-turn fork route")
            }
            SessionIntent::Resume {
                provider_session_id,
            } => {
                ensure!(
                    can_resume || can_load,
                    "ACP did not advertise session resumption"
                );
                params["sessionId"] = json!(provider_session_id);
                if can_resume {
                    "session/resume"
                } else {
                    "session/load"
                }
            }
        };
        let mut observations = vec![self.event(ProviderEventData::Handshake {
            protocol: "acp-v1".into(),
            capabilities: if can_resume || can_load {
                vec![Capability::Resume]
            } else {
                Vec::new()
            },
        })];
        if let Some(prompt) = agent
            .get("promptCapabilities")
            .filter(|data| data.is_object())
        {
            self.prompt_capabilities = prompt.clone();
            observations.push(self.event(ProviderEventData::PromptCapabilities {
                image: prompt["image"].as_bool(),
                audio: prompt["audio"].as_bool(),
                embedded_context: prompt["embeddedContext"].as_bool(),
            }));
        }
        Ok(AdapterUpdate {
            outbound: vec![self.rpc.request(method, params, Request::Open)?],
            events: observations,
        })
    }

    fn opened(&mut self, result: &Value) -> Result<AdapterUpdate> {
        let reported = result["sessionId"].as_str();
        let id = match &self.request.intent {
            SessionIntent::Start => reported.context("ACP session/new did not return an ID")?,
            SessionIntent::Fork { .. } => bail!("unexpected Antigravity ACP fork response"),
            SessionIntent::Resume {
                provider_session_id,
            } => {
                ensure!(
                    reported.is_none_or(|id| id == provider_session_id),
                    "ACP resumed an unexpected session"
                );
                provider_session_id
            }
        };
        ensure!(!id.is_empty(), "ACP returned an empty session ID");
        self.session_id = Some(id.to_owned());
        self.configuration.update(result);
        self.configure()
    }

    fn required_mode(&self) -> &'static str {
        match self.request.mode {
            SessionMode::ApprovedWrite => "default",
            SessionMode::ReadOnly | SessionMode::Plan => "plan",
        }
    }

    fn configure(&mut self) -> Result<AdapterUpdate> {
        let id = self
            .session_id
            .as_deref()
            .context("ACP session has not opened")?;
        for setting in self.settings.by_ref() {
            let requested = match setting {
                Setting::Model => self.request.model.as_deref(),
                Setting::Effort => self.request.effort.as_deref(),
                Setting::Mode => Some(match self.request.mode {
                    SessionMode::ApprovedWrite => "default",
                    _ => "plan",
                }),
            };
            let Some(value) = requested else {
                continue;
            };
            if self.configuration.current(setting) == Some(value) {
                continue;
            }
            let option = self.configuration.option(setting);
            if let Some(option) = option
                && choices(&option["options"]).any(|choice| choice["value"].as_str() == Some(value))
            {
                let config_id = field(option, "id")?;
                return outbound(self.rpc.request(
                    "session/set_config_option",
                    json!({
                        "sessionId": id, "configId": config_id, "value": value,
                    }),
                    Request::Configure {
                        setting,
                        value: value.to_owned(),
                        modern: true,
                    },
                )?);
            }
            let (candidates, key, method) = match setting {
                Setting::Model => (
                    &self.configuration.models["availableModels"],
                    "modelId",
                    "session/set_model",
                ),
                Setting::Mode => (
                    &self.configuration.modes["availableModes"],
                    "id",
                    "session/set_mode",
                ),
                Setting::Effort => bail!("ACP did not offer the requested thinking level"),
            };
            ensure!(
                choices(candidates).any(|candidate| candidate[key].as_str() == Some(value)),
                "ACP did not offer the requested {}",
                setting.category()
            );
            let field = if matches!(setting, Setting::Mode) {
                "modeId"
            } else {
                "modelId"
            };
            let mut params = json!({ "sessionId": id });
            params[field] = json!(value);
            return outbound(self.rpc.request(
                method,
                params,
                Request::Configure {
                    setting,
                    value: value.to_owned(),
                    modern: false,
                },
            )?);
        }
        self.ready = true;
        let mut announced = Vec::new();
        if let Some(models) = self.configuration.reported_models() {
            announced.push(self.event(ProviderEventData::ModelInventory { models }));
        }
        let opts = self.configuration.reported_options();
        if !opts.is_empty() {
            announced.push(self.event(ProviderEventData::SessionOptions { options: opts }));
        }
        if let Some((session, names)) = self.available_commands.take()
            && session == id
        {
            announced.extend(self.command_observations(names));
        }
        if self.request.mode == SessionMode::Plan
            && self.configuration.current(Setting::Mode) == Some("plan")
        {
            announced.push(self.event(ProviderEventData::PlanningModeConfirmed {
                mode: super::planning::PlanningMode::AcpSessionMode,
            }));
        }
        announced.push(
            self.event(ProviderEventData::SessionReady {
                model: self
                    .configuration
                    .current(Setting::Model)
                    .map(str::to_owned),
                effort: self
                    .configuration
                    .current(Setting::Effort)
                    .map(str::to_owned),
            }),
        );
        if matches!(self.request.intent, SessionIntent::Start) {
            announced.push(self.event(ProviderEventData::FreshSessionOpened));
        }
        Ok(events(announced))
    }

    fn response(&mut self, id: &RequestId, frame: &Value) -> Result<AdapterUpdate> {
        let Some(request) = self.rpc.take_request(id) else {
            return Ok(self.diagnostic("ACP sent a response for an unknown request"));
        };
        if let Some(error) = frame.get("error") {
            let mut data = Vec::new();
            if error["code"].as_i64() == Some(AUTH_REQUIRED) {
                data.push(self.event(ProviderEventData::Authentication {
                    authenticated: Some(false),
                }));
            }
            data.push(self.event(ProviderEventData::Error {
                message: diagnostics::preview(
                    error["message"].as_str().unwrap_or("ACP request failed"),
                ),
            }));
            if matches!(request, Request::Prompt) {
                data.extend(self.finish(TurnOutcome::Failed).events);
            }
            return Ok(events(data));
        }
        let result = frame.get("result").context("ACP response has no result")?;
        match request {
            Request::Initialize => self.open_session(result),
            Request::Open => self.opened(result),
            Request::Configure {
                setting,
                value,
                modern,
            } => {
                self.configuration.update(result);
                if modern {
                    ensure!(
                        self.configuration.current(setting) == Some(&value),
                        "ACP did not confirm the requested {}",
                        setting.category()
                    );
                } else {
                    self.configuration.set_legacy(setting, &value);
                    // An ACP implementation with both option representations may
                    // acknowledge only the legacy change. Discard that stale selection.
                    self.configuration.options.retain(|option| {
                        option["category"] != setting.category()
                            && option["id"] != setting.category()
                    });
                }
                self.configure()
            }
            Request::Prompt => {
                let reason = result["stopReason"].as_str();
                let outcome = match reason {
                    Some("end_turn") => TurnOutcome::Completed,
                    Some("cancelled") => TurnOutcome::Interrupted,
                    _ => TurnOutcome::Failed,
                };
                let mut finished = self.finish(outcome);
                if outcome == TurnOutcome::Failed {
                    let reason = reason.unwrap_or("missing stop reason");
                    finished.events.insert(
                        0,
                        self.event(ProviderEventData::Error {
                            message: diagnostics::preview(&format!("ACP prompt stopped: {reason}")),
                        }),
                    );
                }
                Ok(finished)
            }
        }
    }

    fn finish(&mut self, outcome: TurnOutcome) -> AdapterUpdate {
        let Some(turn) = self.turn.take() else {
            return AdapterUpdate::default();
        };
        self.permissions.clear();
        self.questions.clear();
        let mut result = Vec::new();
        if !turn.text.is_empty() {
            if outcome == TurnOutcome::Completed
                && turn.asked_for_plan
                && self.plan_command_available
                && let Some(text) = super::planning::marked_proposal(&turn.text)
            {
                result.push(self.event(ProviderEventData::PlanProposed {
                    text,
                    source: super::planning::PlanSource::AntigravityPlanCommand,
                }));
            }
            let final_response =
                (outcome == TurnOutcome::Completed && !turn.mixed_response).then(|| {
                    let mut event = self.event(ProviderEventData::AssistantFinal {
                        text: turn.text.clone(),
                    });
                    event.location.item_id = turn.response_item_id.clone();
                    event
                });
            let mut message = self.event(ProviderEventData::AssistantMessage { text: turn.text });
            if !turn.mixed_response {
                message.location.item_id = turn.response_item_id;
            }
            result.push(message);
            result.extend(final_response);
        }
        result.push(self.event(ProviderEventData::TurnFinished { outcome }));
        events(result)
    }

    fn update(&mut self, params: &Value) -> Result<AdapterUpdate> {
        let update = &params["update"];
        let Some(session_id) = params["sessionId"].as_str() else {
            return Ok(self.diagnostic("Malformed ACP session update"));
        };
        if !self.ready
            && let SessionIntent::Resume {
                provider_session_id,
            } = &self.request.intent
            && session_id == provider_session_id
            && let Some(
                kind @ ("user_message_chunk"
                | "agent_message_chunk"
                | "tool_call"
                | "tool_call_update"),
            ) = update["sessionUpdate"].as_str()
        {
            return Ok(events([self.event(ProviderEventData::HistoryReported {
                turns: None,
                item_kind: Some(kind.into()),
            })]));
        }
        if update["sessionUpdate"] == "available_commands_update" {
            let Some(commands) = update["availableCommands"]
                .as_array()
                .filter(|commands| commands.iter().all(|command| command["name"].is_string()))
            else {
                return Ok(self.update_diagnostic(update, "Malformed ACP available commands list"));
            };
            let commands: Vec<_> = commands
                .iter()
                .filter_map(ComposerCommand::from_advertisement)
                .collect();
            if !self.ready {
                self.available_commands = Some((session_id.to_owned(), commands));
                return Ok(AdapterUpdate::default());
            }
            if self.session_id.as_deref() == Some(session_id) {
                return Ok(events(self.command_observations(commands)));
            }
        }
        if !self.ready || self.session_id.as_deref() != Some(session_id) {
            return Ok(AdapterUpdate::default());
        }
        if update["sessionUpdate"] == "current_mode_update" {
            ensure!(
                update["currentModeId"] == self.required_mode(),
                "ACP changed the workspace permission mode"
            );
            return Ok(AdapterUpdate::default());
        }
        if update["sessionUpdate"] == "config_option_update" {
            self.configuration.update(update);
            ensure!(
                self.configuration.current(Setting::Mode) == Some(self.required_mode()),
                "ACP changed the workspace permission mode"
            );
            return Ok(events([self.event(ProviderEventData::ModelObserved {
                model: self
                    .configuration
                    .current(Setting::Model)
                    .map(str::to_owned),
                effort: self
                    .configuration
                    .current(Setting::Effort)
                    .map(str::to_owned),
            })]));
        }
        if update["sessionUpdate"] == "usage_update" {
            return Ok(match (update["used"].as_u64(), update["size"].as_u64()) {
                (Some(used), Some(size)) => {
                    events([self.event(ProviderEventData::ContextWindowUsage { used, size })])
                }
                _ => AdapterUpdate::default(),
            });
        }
        let Some(turn) = self.turn.as_mut() else {
            return Ok(AdapterUpdate::default());
        };
        if matches!(
            update["sessionUpdate"].as_str(),
            Some("tool_call" | "tool_call_update")
        ) {
            let Some(id) = string(update, "toolCallId") else {
                return Ok(
                    self.update_diagnostic(update, "ACP tool update omitted its tool call ID")
                );
            };
            turn.mixed_response |= !turn.text.is_empty();
            let tool = turn.tools.entry(id.clone()).or_default();
            if let Some(kind) = string(update, "kind") {
                tool.kind = Some(kind);
            }
            if let Some(name) = string(update, "name") {
                tool.name = Some(name);
            }
            if let Some(locations) = update["locations"].as_array() {
                tool.paths = locations
                    .iter()
                    .filter_map(|location| string(location, "path"))
                    .collect();
            }
            let name = tool
                .name
                .clone()
                .or_else(|| string(update, "title"))
                .or_else(|| tool.kind.clone())
                .unwrap_or_else(|| "tool".into());
            let phase = string(update, "status").unwrap_or_else(|| "updated".into());
            let terminal = matches!(phase.as_str(), "completed" | "failed");
            let paths = if phase == "completed" && tool.kind.as_deref() == Some("read") {
                std::mem::take(&mut tool.paths)
            } else {
                Vec::new()
            };
            if terminal {
                turn.tools.remove(&id);
            }
            let event = |data| {
                let mut event = self.event(data);
                event.location.item_id = Some(id.clone());
                event
            };
            let mut observations = vec![event(ProviderEventData::Tool {
                name: name.clone(),
                phase,
            })];
            observations.extend(paths.into_iter().map(|path| {
                event(ProviderEventData::ContextRead {
                    path,
                    tool: name.clone(),
                })
            }));
            return Ok(events(observations));
        }
        let data = match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let Some(text) = string(&update["content"], "text") else {
                    return Ok(
                        self.update_diagnostic(update, "ACP message update omitted its text")
                    );
                };
                let item_id = string(update, "messageId");
                if turn.text.is_empty() {
                    turn.response_item_id = item_id;
                } else if turn.response_item_id != item_id {
                    turn.mixed_response = true;
                }
                turn.text.push_str(&text);
                ProviderEventData::AssistantDelta { text }
            }
            Some("agent_thought_chunk") => {
                let Some(text) = string(&update["content"], "text") else {
                    return Ok(
                        self.update_diagnostic(update, "ACP thought update omitted its text")
                    );
                };
                ProviderEventData::Reasoning {
                    text,
                    summary: false,
                }
            }
            Some("plan") => {
                let Ok(steps) = serde_json::from_value(update["entries"].clone()) else {
                    return Ok(self.update_diagnostic(
                        update,
                        "ACP plan update did not contain supported steps",
                    ));
                };
                ProviderEventData::PlanProgress { steps }
            }
            Some("user_message_chunk" | "session_info_update") => {
                return Ok(AdapterUpdate::default());
            }
            _ => {
                return Ok(
                    self.update_diagnostic(update, "Malformed or unsupported ACP session update")
                );
            }
        };
        let mut event = self.event(data);
        event.location.item_id =
            string(update, "toolCallId").or_else(|| string(update, "messageId"));
        Ok(events([event]))
    }

    fn permission(&mut self, id: RequestId, params: &Value) -> Result<AdapterUpdate> {
        if self.turn.as_ref().is_none_or(|turn| turn.cancel_requested)
            || self.session_id.as_deref() != params["sessionId"].as_str()
        {
            return outbound(self.rpc.result(&id, cancelled_permission())?);
        }
        ensure!(
            !self.permissions.contains_key(&id) && !self.questions.contains_key(&id),
            "duplicate ACP permission request"
        );
        let options = choices(&params["options"])
            .filter_map(|option| {
                Some(PermissionOption {
                    id: string(option, "optionId")?,
                    kind: string(option, "kind")?,
                })
            })
            .collect::<Vec<_>>();
        ensure!(!options.is_empty(), "ACP permission request has no options");
        let tool = &params["toolCall"];
        let operation = string(tool, "kind").unwrap_or_else(|| "tool".into());
        let description = string(tool, "title").map(|value| diagnostics::preview(&value));
        let mut event = self.event(ProviderEventData::Approval {
            request_id: id.clone(),
            operation,
            description,
            prompt: None,
        });
        event.location.item_id = string(tool, "toolCallId");
        self.permissions.insert(id, options);
        Ok(events([event]))
    }

    fn question(&mut self, id: RequestId, params: &Value) -> Result<AdapterUpdate> {
        if !self.question_route.is_native() {
            return outbound(self.rpc.error(
                &id,
                -32601,
                "this attempt collects questions at turn completion",
            )?);
        }
        if self.turn.as_ref().is_none_or(|turn| turn.cancel_requested)
            || params["sessionId"].as_str().is_none()
            || self.session_id.as_deref() != params["sessionId"].as_str()
            || !params["requestId"].is_null()
        {
            return outbound(self.rpc.error(
                &id,
                -32602,
                "ACP form does not belong to the active prompt",
            )?);
        }
        ensure!(
            !self.questions.contains_key(&id) && !self.permissions.contains_key(&id),
            "duplicate ACP request"
        );
        let normalized = match super::questions::QuestionForm::acp(params) {
            Ok(form) => form,
            Err(error) => {
                return Ok(AdapterUpdate {
                    outbound: vec![
                        self.rpc
                            .error(&id, -32602, "unsupported ACP question form")?,
                    ],
                    events: vec![self.event(ProviderEventData::QuestionUnavailable {
                        request_id: Some(id),
                        reason: diagnostics::preview(&error.to_string()),
                    })],
                });
            }
        };
        self.questions.insert(id.clone(), normalized.clone());
        let mut event = self.event(ProviderEventData::Question {
            request_id: id,
            form: params.clone(),
            normalized: Some(normalized),
            route: self.question_route,
        });
        event.location.item_id = string(params, "toolCallId");
        Ok(events([event]))
    }
}

impl CliSession for AcpSession {
    fn configure_output_schema(&mut self, schema: &Value) -> Result<()> {
        // The CLI exposes --json-schema in print mode. ACP retains host approval of
        // tool requests and launches with default executable customizations disabled.
        self.output_schema = Some(schema.clone());
        Ok(())
    }

    fn reported_cli_permission(&self) -> Option<&'static str> {
        let mode = self.required_mode();
        (self.ready && self.configuration.current(Setting::Mode) == Some(mode)).then_some(mode)
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
        if format.is_image() && self.prompt_capabilities["image"] == true {
            Some(NativeAttachmentEncoding::AcpImage)
        } else if format.is_audio() && self.prompt_capabilities["audio"] == true {
            Some(NativeAttachmentEncoding::AcpAudio)
        } else if matches!(format, AttachmentFormat::Pdf | AttachmentFormat::Text)
            && self.prompt_capabilities["embeddedContext"] == true
        {
            Some(NativeAttachmentEncoding::AcpResource)
        } else {
            None
        }
    }
    fn arguments(&self) -> Vec<OsString> {
        arguments()
    }

    fn configure_mcp_bridge(&mut self, launch: crate::mcp_bridge::BridgeLaunch) {
        self.bridge = Some(launch);
    }

    fn start(&mut self) -> Result<AdapterUpdate> {
        ensure!(!self.handshake_started, "ACP handshake already started");
        self.handshake_started = true;
        let client_capabilities = if self.question_route.is_native() {
            json!({"elicitation": {"form": {}}})
        } else {
            json!({})
        };
        outbound(self.rpc.request(
            "initialize",
            json!({
                "protocolVersion": ACP_VERSION, "clientCapabilities": client_capabilities,
                "clientInfo": {"name":"omniagent", "version":env!("CARGO_PKG_VERSION")},
            }),
            Request::Initialize,
        )?)
    }

    fn send(&mut self, input: TurnInput) -> Result<AdapterUpdate> {
        ensure!(self.ready, "ACP session has not opened");
        ensure!(self.turn.is_none(), "ACP already has a prompt in progress");
        let id = self
            .session_id
            .as_deref()
            .context("missing ACP session ID")?;
        let command = input
            .text
            .split_whitespace()
            .next()
            .and_then(|word| word.strip_prefix('/'));
        let explicit_plan = command == Some("plan");
        let other_advertised_command = command.is_some_and(|name| {
            name != "plan"
                && self
                    .composer_commands
                    .as_ref()
                    .is_some_and(|commands| commands.iter().any(|command| command.name == name))
        });
        let asked_for_plan = self.request.mode == SessionMode::Plan
            && (explicit_plan || (self.plan_command_available && !other_advertised_command));
        let mut text = if asked_for_plan {
            let prefix = if explicit_plan { "" } else { "/plan " };
            format!(
                "{prefix}{}\n\n{}",
                input.text,
                super::planning::PROPOSAL_FORMAT
            )
        } else {
            input.text
        };
        if let Some(schema) = &self.output_schema {
            text.push_str(
                "\n\nReturn exactly one JSON object matching this output JSON Schema. Do not include Markdown fences or additional text.\n",
            );
            text.push_str(&schema.to_string());
        }
        let mut prompt = Vec::new();
        if !text.is_empty() || input.attachments.is_empty() {
            prompt.push(json!({"type":"text", "text":text}));
        }
        for attachment in &input.attachments {
            if let Some(native) = attachment.native() {
                ensure!(
                    self.attachment_encoding(native.format) == Some(native.encoding),
                    "ACP did not advertise this attachment format"
                );
            }
            prompt.push(attachment.block(Provider::Antigravity)?);
        }
        let frame = self.rpc.request(
            "session/prompt",
            json!({
                "sessionId": id, "prompt": prompt,
            }),
            Request::Prompt,
        )?;
        self.turn = Some(Turn {
            text: String::new(),
            response_item_id: None,
            mixed_response: false,
            cancel_requested: false,
            tools: HashMap::new(),
            asked_for_plan,
        });
        Ok(AdapterUpdate {
            outbound: vec![frame],
            events: vec![self.event(ProviderEventData::TurnStarted)],
        })
    }

    fn available_composer_commands(&self) -> Option<&[ComposerCommand]> {
        self.composer_commands.as_deref()
    }

    fn receive(&mut self, frame: OutputFrame) -> Result<AdapterUpdate> {
        if frame.stream == Stream::Stderr {
            return Ok(self.diagnostic(&frame.data));
        }
        let Ok(frame) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(self.diagnostic("Malformed ACP protocol frame"));
        };
        ensure!(
            frame["jsonrpc"] == "2.0",
            "ACP requires JSON-RPC 2.0 frames"
        );
        let params = &frame["params"];
        match (frame["method"].as_str(), rpc::id(&frame)) {
            (Some("session/request_permission"), Some(id)) => self.permission(id, params),
            (Some("elicitation/create"), Some(id)) => self.question(id, params),
            (Some("session/update"), None) => self.update(params),
            (Some(_), Some(id)) => outbound(self.rpc.error(
                &id,
                -32601,
                "ACP client method is unavailable",
            )?),
            (None, Some(id)) => self.response(&id, &frame),
            _ => Ok(self.diagnostic("Unrecognized ACP protocol frame")),
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
            "read-only ACP sessions cannot grant write or execution approval"
        );
        let options = self
            .permissions
            .get(id)
            .context("ACP approval is stale or unknown")?;
        let kind = match decision {
            ApprovalDecision::AllowOnce => Some("allow_once"),
            ApprovalDecision::Deny => Some("reject_once"),
            ApprovalDecision::Cancel => None,
        };
        let option = options
            .iter()
            .find(|candidate| Some(candidate.kind.as_str()) == kind);
        if decision == ApprovalDecision::AllowOnce {
            ensure!(option.is_some(), "ACP did not offer one-time approval");
        }
        let result = option.map_or_else(cancelled_permission, |option| {
            json!({
                "outcome": {"outcome": "selected", "optionId": option.id}
            })
        });
        let frame = self.rpc.result(id, result)?;
        self.permissions.remove(id);
        outbound(frame)
    }

    fn respond_question(&mut self, id: &RequestId, answer: Value) -> Result<AdapterUpdate> {
        use super::questions::{QuestionAction, QuestionReply};
        let form = self
            .questions
            .get(id)
            .context("ACP question is stale or unknown")?;
        let reply: QuestionReply = serde_json::from_value(answer)?;
        form.validate(&reply)?;
        let result = if reply.action == QuestionAction::Accept {
            json!({"action": reply.action, "content": reply.values})
        } else {
            json!({"action": reply.action})
        };
        let response = self.rpc.result(id, result)?;
        self.questions.remove(id);
        Ok(super::questions::encoded_reply(id, reply, response))
    }

    fn interrupt(&mut self) -> Result<Interrupt> {
        let turn = self.turn.as_mut().context("ACP has no active prompt")?;
        ensure!(!turn.cancel_requested, "ACP cancellation already requested");
        let id = self
            .session_id
            .as_deref()
            .context("missing ACP session ID")?;
        let mut outbound = vec![
            self.rpc
                .notify("session/cancel", Some(json!({"sessionId":id})))?,
        ];
        for id in self.permissions.keys() {
            outbound.push(self.rpc.result(id, cancelled_permission())?);
        }
        self.permissions.clear();
        for id in self.questions.keys() {
            outbound.push(self.rpc.result(id, json!({"action":"cancel"}))?);
        }
        self.questions.clear();
        turn.cancel_requested = true;
        Ok(Interrupt {
            outbound,
            grace: Duration::from_secs(5),
        })
    }

    fn process_finished(&mut self, outcome: TerminalOutcome) -> AdapterUpdate {
        self.finish(if matches!(outcome, TerminalOutcome::Cancelled(_)) {
            TurnOutcome::Interrupted
        } else {
            TurnOutcome::Failed
        })
    }
}

fn choices(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten().flat_map(|entry| {
        entry["options"]
            .as_array()
            .map_or_else(|| std::slice::from_ref(entry).iter(), |group| group.iter())
    })
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("ACP field {key} is missing or invalid"))
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

fn cancelled_permission() -> Value {
    json!({"outcome":{"outcome":"cancelled"}})
}

fn outbound(frame: WireMessage) -> Result<AdapterUpdate> {
    Ok(AdapterUpdate {
        outbound: vec![frame],
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

    fn receive(session: &mut dyn CliSession, mut frame: Value) -> Result<AdapterUpdate> {
        frame["jsonrpc"] = json!("2.0");
        session.receive(OutputFrame {
            stream: Stream::Stdout,
            data: frame.to_string(),
        })
    }

    fn message(update: &AdapterUpdate) -> Result<Value> {
        Ok(serde_json::from_slice(update.outbound[0].as_bytes())?)
    }

    fn results(model: &str, thought: &str, mode: &str) -> Value {
        json!({"sessionId":"acp-session","configOptions":[
            {"id":"model","category":"model","type":"select","currentValue":model,
                "options":[{"value":"argon","name":"Argon"},{"value":"flash","name":"Flash"}]},
            {"id":"thought_level","type":"select","currentValue":thought,
                "options":[{"value":"medium"},{"value":"low"}]},
            {"id":"mode","type":"select","currentValue":mode,
                "options":[{"value":"default"},{"value":"plan"}]}
        ]})
    }

    #[test]
    fn structured_turn_sends_the_schema_in_the_scoped_acp_prompt() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            mode: SessionMode::ReadOnly,
            intent: SessionIntent::Start,
            model: None,
            effort: None,
        })?;
        let schema =
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false});
        session.configure_output_schema(&schema)?;
        assert!(
            session
                .arguments()
                .contains(&"--include-default-customizations=false".into())
        );
        session.start()?;
        receive(
            &mut *session,
            json!({"id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}),
        )?;
        receive(
            &mut *session,
            json!({"id":2,"result":results("argon","medium","plan")}),
        )?;
        let request = session.send(TurnInput {
            attachments: Vec::new(),
            message_id: "typed-message".into(),
            text: "Summarize".into(),
        })?;
        let frame = message(&request)?;
        assert_eq!(frame["method"], "session/prompt");
        let text = frame["params"]["prompt"][0]["text"]
            .as_str()
            .context("ACP structured prompt did not contain text")?;
        assert!(text.starts_with("Summarize\n\nReturn exactly one JSON object"));
        assert!(text.ends_with(&schema.to_string()));
        Ok(())
    }

    #[test]
    fn composer_command_uses_only_the_current_acp_session_inventory() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            mode: SessionMode::Plan,
            intent: SessionIntent::Start,
            model: None,
            effort: None,
        })?;
        session.start()?;
        receive(
            &mut *session,
            json!({"id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}),
        )?;
        let commands = |id: &str, names: Value| {
            json!({
                "method":"session/update",
                "params":{"sessionId":id,"update":{
                    "sessionUpdate":"available_commands_update",
                    "availableCommands":names
                }}
            })
        };
        receive(
            &mut *session,
            commands(
                "acp-session",
                json!([{"name":"plan"},{"name":"review","description":"Review"}]),
            ),
        )?;
        receive(
            &mut *session,
            json!({"id":2,"result":results("argon","medium","plan")}),
        )?;
        receive(
            &mut *session,
            commands("different-session", json!([{"name":"unexpected"}])),
        )?;
        let advertised = session
            .available_composer_commands()
            .context("missing commands")?;
        assert_eq!(
            advertised
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["plan", "review"]
        );
        let request = session.send(TurnInput {
            attachments: Vec::new(),
            message_id: "message".into(),
            text: "/review src".into(),
        })?;
        let frame = message(&request)?;
        assert_eq!(frame["method"], "session/prompt");
        assert_eq!(frame["params"]["prompt"][0]["text"], "/review src");
        receive(&mut *session, commands("acp-session", json!([])))?;
        assert_eq!(session.available_composer_commands(), Some([].as_slice()));
        Ok(())
    }

    #[test]
    fn resumes_configures_and_streams_one_prompt() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            mode: SessionMode::Plan,
            intent: SessionIntent::Resume {
                provider_session_id: "acp-session".into(),
            },
            model: Some("flash".into()),
            effort: Some("low".into()),
        })?;
        assert_eq!(message(&session.start()?)?["params"]["protocolVersion"], 1);
        let opening = receive(
            &mut *session,
            json!({"id":1,"result":{
                "protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"resume":{}}}
            }}),
        )?;
        assert_eq!(message(&opening)?["method"], "session/resume");
        assert_eq!(message(&opening)?["params"]["sessionId"], "acp-session");
        let replay = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk",
                    "content":{"type":"text","text":"previous answer"}}
            }}),
        )?;
        assert_eq!(
            replay.events[0].data,
            ProviderEventData::HistoryReported {
                turns: None,
                item_kind: Some("agent_message_chunk".into()),
            }
        );
        let option = receive(
            &mut *session,
            json!({"id":2,"result":results("argon","medium","default")}),
        )?;
        assert_eq!(message(&option)?["params"]["configId"], "model");
        let effort = receive(
            &mut *session,
            json!({"id":3,"result":results("flash","medium","default")}),
        )?;
        assert_eq!(message(&effort)?["params"]["configId"], "thought_level");
        let mode = receive(
            &mut *session,
            json!({"id":4,"result":results("flash","low","default")}),
        )?;
        assert_eq!(message(&mode)?["params"]["value"], "plan");
        assert!(
            session
                .send(TurnInput {
                    attachments: Vec::new(),
                    message_id: "early".into(),
                    text: "blocked".into()
                })
                .is_err()
        );
        let ready = receive(
            &mut *session,
            json!({"id":5,"result":results("flash","low","plan")}),
        )?;
        assert!(ready.events.iter().any(|event| event.data
            == ProviderEventData::SessionReady {
                model: Some("flash".into()),
                effort: Some("low".into())
            }));
        let request = session.send(TurnInput {
            attachments: Vec::new(),
            message_id: "message".into(),
            text: "Olá ☕\nline".into(),
        })?;
        assert_eq!(
            message(&request)?["params"]["prompt"][0]["text"],
            "Olá ☕\nline"
        );
        let read = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"tool_call","toolCallId":"read",
                    "name":"read_file","kind":"read","status":"in_progress","locations":[{"path":"checked.txt"}]}
            }}),
        )?;
        assert_eq!(read.events.len(), 1);
        let read = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"tool_call_update",
                    "toolCallId":"read","status":"completed"}
            }}),
        )?;
        assert_eq!(
            read.events[1].data,
            ProviderEventData::ContextRead {
                path: "checked.txt".into(),
                tool: "read_file".into(),
            }
        );
        let usage = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"usage_update","used":12,"size":256}
            }}),
        )?;
        assert_eq!(
            usage.events[0].data,
            ProviderEventData::ContextWindowUsage {
                used: 12,
                size: 256
            }
        );

        let permission = receive(
            &mut *session,
            json!({
                "method":"session/request_permission","id":"decision",
                "params":{"sessionId":"acp-session","toolCall":{"toolCallId":"tool","title":"examine","kind":"read"},
                    "options":[{"optionId":"yes","kind":"allow_once"},{"optionId":"no","kind":"reject_once"}]}
            }),
        )?;
        assert_eq!(
            permission.events[0].location.item_id.as_deref(),
            Some("tool")
        );
        let decision = RequestId::Text("decision".into());
        assert!(
            session
                .respond_approval(&decision, ApprovalDecision::AllowOnce)
                .is_err()
        );
        let denied = session.respond_approval(&decision, ApprovalDecision::Deny)?;
        assert_eq!(message(&denied)?["result"]["outcome"]["optionId"], "no");
        assert!(
            session
                .respond_approval(&decision, ApprovalDecision::Deny)
                .is_err()
        );

        let malformed = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk","messageId":"bad"}
            }}),
        )?;
        assert_eq!(malformed.events[0].location.item_id.as_deref(), Some("bad"));
        assert!(matches!(
            malformed.events[0].data,
            ProviderEventData::Diagnostic { .. }
        ));
        let update = receive(
            &mut *session,
            json!({"method":"session/update","params":{
                "sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"success"}}
            }}),
        )?;
        assert_eq!(
            update.events[0].data,
            ProviderEventData::AssistantDelta {
                text: "success".into()
            }
        );
        let done = receive(
            &mut *session,
            json!({"id":6,"result":{"stopReason":"end_turn"}}),
        )?;
        assert_eq!(
            done.events[0].data,
            ProviderEventData::AssistantMessage {
                text: "success".into()
            }
        );
        assert_eq!(
            done.events[1].data,
            ProviderEventData::AssistantFinal {
                text: "success".into()
            }
        );
        assert_eq!(
            done.events[2].data,
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Completed
            }
        );
        let prompt = session.send(TurnInput {
            text: "Mixed stream".into(),
            message_id: "mixed".into(),
            attachments: Vec::new(),
        })?;
        let prompt: Value = serde_json::from_slice(prompt.outbound[0].as_bytes())?;
        for (id, text) in [("progress", "Checking "), ("answer", "ready")] {
            receive(
                &mut *session,
                json!({"method":"session/update","params":{
                    "sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk",
                        "messageId":id,"content":{"type":"text","text":text}}
                }}),
            )?;
        }
        let mixed = receive(
            &mut *session,
            json!({"id":prompt["id"],"result":{"stopReason":"end_turn"}}),
        )?;
        assert!(matches!(mixed.events.as_slice(), [message, terminal]
            if matches!(&message.data, ProviderEventData::AssistantMessage { text } if text == "Checking ready")
                && matches!(terminal.data, ProviderEventData::TurnFinished { outcome: TurnOutcome::Completed })));
        Ok(())
    }

    #[test]
    fn rejects_a_protocol_version_or_permission_mode_the_agent_does_not_confirm() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            intent: SessionIntent::Start,
            mode: SessionMode::ReadOnly,
            model: None,
            effort: None,
        })?;
        session.start()?;
        assert!(
            receive(
                &mut *session,
                json!({"id":1,"result":{"protocolVersion":2}})
            )
            .is_err()
        );

        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            intent: SessionIntent::Start,
            mode: SessionMode::ReadOnly,
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
            json!({"id":2,"result":results("argon","medium","default")}),
        )?;
        assert!(
            receive(
                &mut *session,
                json!({"id":3,"result":results("argon","medium","default")})
            )
            .is_err()
        );
        assert!(
            session
                .send(TurnInput {
                    attachments: Vec::new(),
                    message_id: "early".into(),
                    text: "blocked".into()
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn answers_opaque_acp_form_values_and_rejects_an_invalid_or_stale_reply() -> Result<()> {
        assert_acp_form_reply(SessionMode::Plan, "plan")
    }

    #[test]
    fn answers_native_forms_during_approved_execution() -> Result<()> {
        assert_acp_form_reply(SessionMode::ApprovedWrite, "default")
    }

    fn assert_acp_form_reply(mode: SessionMode, acp_mode: &str) -> Result<()> {
        use crate::providers::questions::QuestionInput;

        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            intent: SessionIntent::Start,
            mode,
            model: None,
            effort: None,
        })?;
        let init = message(&session.start()?)?;
        assert_eq!(
            init["params"]["clientCapabilities"]["elicitation"],
            json!({"form":{}})
        );
        receive(
            &mut *session,
            json!({"id":1,"result":{"protocolVersion":1}}),
        )?;
        receive(
            &mut *session,
            json!({"id":2,"result":results("argon","medium",acp_mode)}),
        )?;
        session.send(TurnInput {
            attachments: Vec::new(),
            message_id: "form-turn".into(),
            text: "Ask for delivery preferences".into(),
        })?;
        let request = json!({
            "id":134, "method":"elicitation/create",
            "params":{"sessionId":"acp-session","toolCallId":"ask-1","mode":"form","message":"Choose inputs",
                "requestedSchema":{"type":"object","required":["plan_id","tests","include_note"],
                    "properties":{
                        "plan_id":{"type":"string","oneOf":[{"const":"1","title":"Small"},{"const":"2","title":"Expanded"}],"default":"2"},
                        "tests":{"type":"array","items":{"anyOf":[{"const":"core","title":"Core"},{"const":"ui","title":"UI"}]},"minItems":1,"maxItems":2},
                        "include_note":{"type":"boolean","default":false},
                        "owner":{"type":"string","maxLength":6},
                        "depth":{"type":"integer","minimum":1,"maximum":3}
                    }
                }
            }
        });
        let mut wrong = request.clone();
        wrong["params"]["sessionId"] = json!("other-session");
        assert_eq!(
            message(&receive(&mut *session, wrong)?)?["error"]["code"],
            -32602
        );
        let mut unsupported = request.clone();
        unsupported["params"]["requestedSchema"]["properties"]["owner"] = json!({"type":"object"});
        assert_eq!(
            message(&receive(&mut *session, unsupported)?)?["error"]["code"],
            -32602
        );
        let delivered = receive(&mut *session, request.clone())?;
        let event = &delivered.events[0];
        let ProviderEventData::Question {
            request_id,
            form,
            normalized: Some(normalized),
            ..
        } = &event.data
        else {
            panic!("missing normalized ACP question");
        };
        assert_eq!(event.location.item_id.as_deref(), Some("ask-1"));
        assert_eq!(form, &request["params"]);
        let plan = normalized
            .fields
            .iter()
            .find(|field| field.id == "plan_id")
            .unwrap();
        assert_eq!(plan.default, Some(json!("2")));
        assert!(
            matches!(&plan.input, QuestionInput::Choice { options, .. } if options[1].id == "2" && options[1].label == "Expanded")
        );
        let answer = json!({"action":"accept","values":{"plan_id":"2","tests":["ui","core"],"include_note":false,"owner":"Ana","depth":2}});
        for (key, malformed) in [
            ("plan_id", json!("Expanded")),
            ("tests", json!(["core", "core"])),
            ("include_note", json!("false")),
            ("owner", json!("overlong")),
            ("depth", json!(2.5)),
        ] {
            let mut rejected = answer.clone();
            rejected["values"][key] = malformed;
            assert!(session.respond_question(request_id, rejected).is_err());
        }
        let reply = session.respond_question(request_id, answer.clone())?;
        assert_eq!(
            message(&reply)?,
            json!({"jsonrpc":"2.0","id":134,"result":{"action":"accept","content":answer["values"]}})
        );
        assert!(session.respond_question(request_id, answer).is_err());
        let mut second = request;
        second["id"] = json!("second-form");
        receive(&mut *session, second.clone())?;
        let id = RequestId::Text("second-form".into());
        let decline = session.respond_question(&id, json!({"action":"decline"}))?;
        assert_eq!(message(&decline)?["result"], json!({"action":"decline"}));
        assert!(
            session
                .respond_question(&id, json!({"action":"cancel"}))
                .is_err()
        );
        second["id"] = json!("pending-on-cancel");
        receive(&mut *session, second)?;
        let cancelled = session.interrupt()?;
        assert_eq!(
            serde_json::from_slice::<Value>(cancelled.outbound[1].as_bytes())?,
            json!({"jsonrpc":"2.0","id":"pending-on-cancel","result":{"action":"cancel"}})
        );
        Ok(())
    }

    #[test]
    fn cancellation_unblocks_permissions_and_waits_for_the_prompt_response() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = AntigravityAdapter.create_session(SessionRequest {
            checkout: checkout.path().into(),
            intent: SessionIntent::Start,
            mode: SessionMode::ReadOnly,
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
            json!({"id":2,"result":results("argon","medium","plan")}),
        )?;
        session.send(TurnInput {
            attachments: Vec::new(),
            message_id: "turn".into(),
            text: "Cancellation fixture".into(),
        })?;
        let permission = |id: usize| {
            json!({
                "id":id,"method":"session/request_permission","params":{
                    "sessionId":"acp-session","toolCall":{"toolCallId":"edit","kind":"edit"},
                    "options":[{"optionId":"allow","kind":"allow_once"}]
                }
            })
        };
        receive(&mut *session, permission(101))?;
        let interrupt = session.interrupt()?;
        assert_eq!(interrupt.outbound.len(), 2);
        let notify: Value = serde_json::from_slice(interrupt.outbound[0].as_bytes())?;
        assert_eq!(
            notify,
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"acp-session"}})
        );
        let reply: Value = serde_json::from_slice(interrupt.outbound[1].as_bytes())?;
        assert_eq!(reply["id"], 101);
        assert_eq!(reply["result"]["outcome"]["outcome"], "cancelled");
        assert!(
            session
                .respond_approval(&RequestId::Number(101), ApprovalDecision::Deny)
                .is_err()
        );
        assert!(session.interrupt().is_err());
        let late = receive(&mut *session, permission(102))?;
        assert_eq!(message(&late)?["result"]["outcome"]["outcome"], "cancelled");
        assert!(
            receive(
                &mut *session,
                json!({"id":800,"result":{"stopReason":"cancelled"}})
            )?
            .events
            .iter()
            .all(|event| !matches!(event.data, ProviderEventData::TurnFinished { .. }))
        );
        let terminal = receive(
            &mut *session,
            json!({"id":3,"result":{"stopReason":"cancelled"}}),
        )?;
        assert_eq!(
            terminal.events[0].data,
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Interrupted
            }
        );
        assert!(
            session
                .process_finished(TerminalOutcome::Exited(0))
                .events
                .is_empty()
        );
        Ok(())
    }
}
