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
            available_commands: None,
            bridge: None,
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
    cancel_requested: bool,
    tools: HashMap<String, ReportedTool>,
}

#[derive(Default)]
struct ReportedTool {
    kind: Option<String>,
    name: Option<String>,
    paths: Vec<String>,
}

struct AcpSession {
    request: SessionRequest,
    rpc: Rpc<Request>,
    handshake_started: bool,
    ready: bool,
    session_id: Option<String>,
    configuration: Configuration,
    settings: std::array::IntoIter<Setting, 3>,
    turn: Option<Turn>,
    permissions: HashMap<RequestId, Vec<PermissionOption>>,
    available_commands: Option<(String, Vec<String>)>,
    bridge: Option<crate::mcp_bridge::BridgeLaunch>,
}

struct PermissionOption {
    id: String,
    kind: String,
}

impl AcpSession {
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
            announced.push(self.event(ProviderEventData::AvailableCommands { names }));
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
        let mut result = Vec::new();
        if !turn.text.is_empty() {
            result.push(self.event(ProviderEventData::AssistantMessage { text: turn.text }));
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
            let names = choices(&update["availableCommands"])
                .filter_map(|command| string(command, "name"))
                .collect();
            if !self.ready {
                self.available_commands = Some((session_id.to_owned(), names));
                return Ok(AdapterUpdate::default());
            }
            if self.session_id.as_deref() == Some(session_id) {
                return Ok(events([
                    self.event(ProviderEventData::AvailableCommands { names })
                ]));
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
            Some("user_message_chunk" | "plan" | "session_info_update") => {
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
            !self.permissions.contains_key(&id),
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
}

impl CliSession for AcpSession {
    fn arguments(&self) -> Vec<OsString> {
        arguments()
    }

    fn configure_mcp_bridge(&mut self, launch: crate::mcp_bridge::BridgeLaunch) {
        self.bridge = Some(launch);
    }

    fn start(&mut self) -> Result<AdapterUpdate> {
        ensure!(!self.handshake_started, "ACP handshake already started");
        self.handshake_started = true;
        outbound(self.rpc.request(
            "initialize",
            json!({
                "protocolVersion": ACP_VERSION, "clientCapabilities": {},
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
        let frame = self.rpc.request(
            "session/prompt",
            json!({
                "sessionId": id, "prompt": [{"type":"text", "text": input.text}],
            }),
            Request::Prompt,
        )?;
        self.turn = Some(Turn {
            text: String::new(),
            cancel_requested: false,
            tools: HashMap::new(),
        });
        Ok(AdapterUpdate {
            outbound: vec![frame],
            events: vec![self.event(ProviderEventData::TurnStarted)],
        })
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

    fn respond_question(&mut self, _id: &RequestId, _answer: Value) -> Result<AdapterUpdate> {
        bail!("structured ACP elicitation has not been negotiated")
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
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Completed
            }
        );
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
                    message_id: "early".into(),
                    text: "blocked".into()
                })
                .is_err()
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
