use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{
    AdapterUpdate, ApprovalDecision, AuthProbe, Authentication, CliAdapter, CliSession, Interrupt,
    ProbeKind, Provider, ProviderEvent, ProviderEventData, ProviderLocation, ReportedModel,
    RequestId, SessionIntent, SessionMode, SessionRequest, Transport, TurnInput, TurnOutcome,
    WireMessage, capabilities,
    rpc::{self, Rpc},
    skills::{SkillCatalog, SkillCatalogIssue, SkillEntry, SkillSource},
};
use crate::{
    diagnostics,
    process::TerminalOutcome,
    process_io::{OutputFrame, Stream},
};

pub struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }
    fn transport(&self) -> Transport {
        Transport::CodexAppServer
    }

    fn authentication_probe(&self) -> AuthProbe {
        AuthProbe {
            kind: ProbeKind::Command,
            arguments: ["login", "status"].map(OsString::from).to_vec(),
        }
    }

    fn decode_authentication(&self, status: TerminalOutcome, output: &str) -> Authentication {
        let normalized = output.trim().to_ascii_lowercase();
        if status == TerminalOutcome::Exited(0) && normalized.starts_with("logged in") {
            Authentication::Authenticated
        } else if normalized.starts_with("not logged in") {
            Authentication::Required
        } else {
            Authentication::Unknown
        }
    }

    fn create_session(&self, request: SessionRequest) -> Result<Box<dyn CliSession>> {
        ensure!(
            request.checkout.is_absolute(),
            "Codex checkout must be absolute"
        );
        Ok(Box::new(CodexSession {
            checkout_root: request.checkout.clone(),
            bridge: None,
            request,
            rpc: Rpc::new(false),
            handshake_started: false,
            session: None,
            active: None,
            model_pages: Vec::new(),
            workspace_instructions: None,
            model_cursors: HashSet::new(),
            approvals: HashMap::new(),
            questions: HashMap::new(),
        }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    Initialize,
    OpenSession,
    ReadConfiguration,
    ListSkills,
    ModelPage,
    StartTurn,
    Interrupt,
}

struct Session {
    id: String,
    model: String,
    effort: Option<String>,
}

struct ActiveTurn {
    id: Option<String>,
    interrupt_requested: bool,
}

enum Approval {
    Decision,
    Permissions(Value),
}

struct CodexSession {
    request: SessionRequest,
    checkout_root: std::path::PathBuf,
    bridge: Option<crate::mcp_bridge::BridgeLaunch>,
    rpc: Rpc<Request>,
    handshake_started: bool,
    session: Option<Session>,
    active: Option<ActiveTurn>,
    model_pages: Vec<ReportedModel>,
    workspace_instructions: Option<String>,
    model_cursors: HashSet<String>,
    approvals: HashMap<RequestId, Approval>,
    questions: HashMap<RequestId, Vec<String>>,
}

impl CodexSession {
    fn sandbox(&self) -> (&'static str, &'static str) {
        match self.request.mode {
            SessionMode::ApprovedWrite => ("workspace-write", "workspaceWrite"),
            SessionMode::ReadOnly | SessionMode::Plan => ("read-only", "readOnly"),
        }
    }

    fn open_session(&mut self) -> Result<AdapterUpdate> {
        let approval = match self.request.mode {
            SessionMode::ApprovedWrite => "on-request",
            _ => "never",
        };
        let mut params = json!({
            "cwd": self.request.checkout,
            "sandbox": self.sandbox().0,
            "approvalPolicy": approval,
        });
        if let Some(model) = &self.request.model {
            params["model"] = json!(model);
        }
        if let Some(instructions) = &self.workspace_instructions {
            params["developerInstructions"] = json!(instructions);
        }
        let method = match &self.request.intent {
            SessionIntent::Start => "thread/start",
            SessionIntent::Resume {
                provider_session_id,
            } => {
                params["threadId"] = json!(provider_session_id);
                "thread/resume"
            }
        };
        Ok(AdapterUpdate {
            outbound: vec![
                self.rpc.notify("initialized", None)?,
                self.rpc.request(method, params, Request::OpenSession)?,
            ],
            events: vec![self.event(ProviderEventData::Handshake {
                protocol: "codex-app-server-v2".into(),
                capabilities: Vec::new(),
            })],
        })
    }

    fn event(&self, data: ProviderEventData) -> ProviderEvent {
        ProviderEvent {
            location: ProviderLocation {
                session_id: self.session.as_ref().map(|session| session.id.clone()),
                turn_id: self.active.as_ref().and_then(|turn| turn.id.clone()),
                ..Default::default()
            },
            data,
        }
    }

    fn diagnostic(&self, message: &str) -> AdapterUpdate {
        self.events(vec![self.event(ProviderEventData::Diagnostic {
            message: diagnostics::preview(message),
        })])
    }

    fn notification_diagnostic(&self, params: &Value, message: &str) -> AdapterUpdate {
        let mut event = self.event(ProviderEventData::Diagnostic {
            message: diagnostics::preview(message),
        });
        event.location.turn_id = string(params, "turnId").or(event.location.turn_id);
        event.location.item_id = string(params, "itemId").or_else(|| string(&params["item"], "id"));
        self.events(vec![event])
    }

    fn events(&self, events: Vec<ProviderEvent>) -> AdapterUpdate {
        AdapterUpdate {
            outbound: Vec::new(),
            events,
        }
    }

    fn response(&mut self, id: &RequestId, response: &Value) -> Result<AdapterUpdate> {
        let Some(request) = self.rpc.take_request(id) else {
            return Ok(self.diagnostic("Codex sent a response for an unknown request"));
        };
        if let Some(error) = response.get("error") {
            if request == Request::ReadConfiguration {
                return Ok(self.events(vec![self.event(
                    ProviderEventData::ConfigurationReported {
                        revision: None,
                        codex_instructions: None,
                        qualification: None,
                    },
                )]));
            }
            if request == Request::ListSkills {
                let message = error["message"]
                    .as_str()
                    .map(diagnostics::preview)
                    .unwrap_or_else(|| "Codex skills/list failed".into());
                return Ok(
                    self.events(vec![self.event(ProviderEventData::SkillsReported {
                        catalog: SkillCatalog::unavailable(&message),
                    })]),
                );
            }
            let message =
                diagnostics::preview(error["message"].as_str().unwrap_or("Codex request failed"));
            if request == Request::ModelPage {
                return Ok(
                    self.diagnostic(&format!("Codex model inventory unavailable: {message}"))
                );
            }
            let mut events = vec![self.event(ProviderEventData::Error { message })];
            if request == Request::StartTurn {
                events.extend(self.finish_turn(TurnOutcome::Failed));
            }
            return Ok(self.events(events));
        }
        let result = response
            .get("result")
            .context("Codex response has no result")?;
        match request {
            Request::Initialize => {
                ensure!(
                    result["userAgent"].is_string(),
                    "invalid Codex initialize response"
                );
                self.open_session()
            }
            Request::OpenSession => {
                let id = required_string(&result["thread"], "id")?;
                let model = required_string(result, "model")?;
                let cwd = required_string(result, "cwd")?;
                ensure!(
                    dunce::canonicalize(Path::new(cwd))?
                        == dunce::canonicalize(&self.request.checkout)?,
                    "Codex reported an unexpected checkout"
                );
                ensure!(
                    result.pointer("/sandbox/type").and_then(Value::as_str)
                        == Some(self.sandbox().1),
                    "Codex did not establish the requested sandbox"
                );
                if let SessionIntent::Resume {
                    provider_session_id,
                } = &self.request.intent
                {
                    ensure!(
                        provider_session_id == id,
                        "Codex resumed an unexpected session"
                    );
                }
                let effort = string(result, "reasoningEffort");
                self.session = Some(Session {
                    id: id.to_owned(),
                    model: model.to_owned(),
                    effort: effort.clone(),
                });
                let mut events = Vec::new();
                if matches!(self.request.intent, SessionIntent::Resume { .. })
                    && let Some(turns) = result["thread"]["turns"].as_array()
                {
                    events.push(self.event(ProviderEventData::HistoryReported {
                        turns: Some(turns.len() as u64),
                        item_kind: None,
                    }));
                }
                events.push(self.event(ProviderEventData::SessionReady {
                    model: Some(model.to_owned()),
                    effort,
                }));
                Ok(AdapterUpdate {
                    outbound: vec![self.rpc.request(
                        "model/list",
                        json!({"limit":100}),
                        Request::ModelPage,
                    )?],
                    events,
                })
            }
            Request::ReadConfiguration => {
                let mut codex_instructions = None;
                let mut qualification = None;
                let revision = if let Some(mut config) = result
                    .get("config")
                    .filter(|value| value.is_object())
                    .cloned()
                {
                    if config.get("project_doc_fallback_filenames").is_some()
                        || config.get("project_doc_max_bytes").is_some()
                    {
                        codex_instructions = serde_json::from_value(config.clone()).ok();
                    }
                    config.sort_all_objects();
                    if let Some(fields) = config.as_object() {
                        qualification = Some(crate::providers::configuration::codex_configuration(
                            fields.clone(),
                            &self.checkout_root,
                            self.bridge.as_ref(),
                        )?);
                    }
                    Some(capabilities::revision(&config)?)
                } else {
                    None
                };
                Ok(
                    self.events(vec![self.event(ProviderEventData::ConfigurationReported {
                        revision,
                        codex_instructions,
                        qualification,
                    })]),
                )
            }
            Request::ListSkills => {
                let catalog = self.decode_skills(result)?;
                Ok(self.events(vec![
                    self.event(ProviderEventData::SkillsReported { catalog }),
                ]))
            }
            Request::ModelPage => self.model_page(result),
            Request::StartTurn => {
                let id = required_string(&result["turn"], "id")?;
                let events = self.learn_turn(id)?;
                Ok(self.events(events))
            }
            Request::Interrupt => Ok(AdapterUpdate::default()),
        }
    }

    fn decode_skills(&self, result: &Value) -> Result<SkillCatalog> {
        let scope = result["data"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| {
                entry["cwd"].as_str().is_some_and(|cwd| {
                    dunce::simplified(Path::new(cwd)) == dunce::simplified(&self.request.checkout)
                })
            });
        let Some((scope, skills)) =
            scope.and_then(|entry| Some((entry, entry["skills"].as_array()?)))
        else {
            return Ok(SkillCatalog::unavailable(
                "Codex did not report skills for the active checkout",
            ));
        };
        let mut catalog = SkillCatalog {
            entries: Vec::new(),
            issues: Vec::new(),
            reported: true,
        };
        for skill in skills {
            let path = string(skill, "path")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute());
            let name = string(skill, "name").filter(|name| !name.trim().is_empty());
            let (Some(path), Some(name)) = (path, name) else {
                catalog.issues.push(SkillCatalogIssue {
                    path: None,
                    message: "Codex reported a skill without an absolute source path or name"
                        .into(),
                });
                continue;
            };
            catalog.entries.push(SkillEntry {
                id: SkillEntry::identity(Provider::Codex, &path)?,
                provider: Provider::Codex,
                source: SkillSource::Provider,
                name,
                path,
                scope: string(skill, "scope"),
                plugin_id: string(skill, "pluginId"),
                enabled: skill["enabled"].as_bool(),
                revision: None,
            });
        }
        catalog
            .issues
            .extend(
                scope["errors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|error| {
                        Some(SkillCatalogIssue {
                            path: string(error, "path").map(Into::into),
                            message: diagnostics::preview(error["message"].as_str()?),
                        })
                    }),
            );
        Ok(catalog)
    }

    fn model_page(&mut self, result: &Value) -> Result<AdapterUpdate> {
        let Some(data) = result["data"].as_array() else {
            return Ok(self.diagnostic("Codex model page has no data array"));
        };
        self.model_pages.extend(data.iter().filter_map(|model| {
            let id = string(model, "id")?;
            Some(ReportedModel {
                name: string(model, "displayName").unwrap_or_else(|| id.clone()),
                id,
                canonical_id: string(model, "model"),
                input_modalities: model["inputModalities"].as_array().map(|modalities| {
                    modalities
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                }),
                output_modalities: None,
                effort_levels: model["supportedReasoningEfforts"].as_array().map(|levels| {
                    levels
                        .iter()
                        .filter_map(|level| string(level, "reasoningEffort"))
                        .collect()
                }),
                default_effort: string(model, "defaultReasoningEffort"),
                selectable: model["hidden"].as_bool().map(|hidden| !hidden),
            })
        }));
        if let Some(cursor) = result["nextCursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
        {
            if self.model_cursors.len() >= 64 || !self.model_cursors.insert(cursor.to_owned()) {
                return Ok(self.diagnostic("Codex stopped advancing the model inventory cursor"));
            }
            return Ok(AdapterUpdate {
                outbound: vec![self.rpc.request(
                    "model/list",
                    json!({"limit":100,"cursor":cursor}),
                    Request::ModelPage,
                )?],
                events: Vec::new(),
            });
        }
        let models = std::mem::take(&mut self.model_pages);
        Ok(self.events(vec![
            self.event(ProviderEventData::ModelInventory { models }),
        ]))
    }

    fn learn_turn(&mut self, id: &str) -> Result<Vec<ProviderEvent>> {
        let Some(turn) = self.active.as_mut() else {
            return Ok(Vec::new());
        };
        if let Some(known) = &turn.id {
            ensure!(known == id, "Codex sent an unexpected turn ID");
            return Ok(Vec::new());
        }
        turn.id = Some(id.to_owned());
        Ok(vec![self.event(ProviderEventData::TurnStarted)])
    }

    fn finish_turn(&mut self, outcome: TurnOutcome) -> Vec<ProviderEvent> {
        if self.active.is_none() {
            return Vec::new();
        }
        let finished = self.event(ProviderEventData::TurnFinished { outcome });
        self.active = None;
        self.approvals.clear();
        self.questions.clear();
        vec![finished]
    }

    fn notification(&mut self, method: &str, params: &Value) -> Result<AdapterUpdate> {
        if self.active.is_none() && method != "error" {
            return Ok(AdapterUpdate::default());
        }
        let thread = params["threadId"].as_str();
        if let Some(thread) = thread
            && self
                .session
                .as_ref()
                .is_none_or(|session| session.id != thread)
        {
            return Ok(AdapterUpdate::default());
        }
        if let Some(turn) = params["turnId"].as_str()
            && self
                .active
                .as_ref()
                .and_then(|active| active.id.as_deref())
                .is_some_and(|id| id != turn)
        {
            return Ok(AdapterUpdate::default());
        }
        if matches!(method, "turn/started" | "turn/completed") {
            let id = required_string(&params["turn"], "id")?;
            if self
                .active
                .as_ref()
                .and_then(|turn| turn.id.as_deref())
                .is_some_and(|active| id != active)
            {
                return Ok(AdapterUpdate::default());
            }
            let mut events = self.learn_turn(id)?;
            if method == "turn/completed" {
                let outcome = match params["turn"]["status"].as_str() {
                    Some("completed") => TurnOutcome::Completed,
                    Some("interrupted") => TurnOutcome::Interrupted,
                    Some("failed") => TurnOutcome::Failed,
                    _ => {
                        return Ok(self.notification_diagnostic(
                            params,
                            "Codex sent an unknown terminal turn status",
                        ));
                    }
                };
                events.extend(self.finish_turn(outcome));
            }
            return Ok(self.events(events));
        }
        let delta = || required_string(params, "delta").map(str::to_owned);
        let data = match method {
            "thread/settings/updated" => {
                let settings = &params["threadSettings"];
                ProviderEventData::ModelObserved {
                    model: string(settings, "model"),
                    effort: string(settings, "effort"),
                }
            }
            "item/agentMessage/delta" => ProviderEventData::AssistantDelta { text: delta()? },
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                ProviderEventData::Reasoning {
                    text: delta()?,
                    summary: method.ends_with("/summaryTextDelta"),
                }
            }
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"]["last"];
                ProviderEventData::Usage {
                    input: usage["inputTokens"].as_u64(),
                    output: usage["outputTokens"].as_u64(),
                    cached_input: usage["cachedInputTokens"].as_u64(),
                }
            }
            "item/started" | "item/completed" => {
                return self.item(params, method.ends_with("/completed"));
            }
            "hook/started" | "hook/completed" => {
                let run = &params["run"];
                let mut event = self.event(ProviderEventData::HookObserved {
                    trigger: required_string(run, "eventName")?.into(),
                    name: None,
                    source_path: string(run, "sourcePath").map(Into::into),
                    status: required_string(run, "status")?.into(),
                });
                event.location.turn_id = string(params, "turnId").or(event.location.turn_id);
                event.location.item_id = string(run, "id");
                return Ok(self.events(vec![event]));
            }
            "error" => ProviderEventData::Error {
                message: diagnostics::preview(
                    params["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex reported an error"),
                ),
            },
            "thread/status/changed"
            | "turn/diff/updated"
            | "turn/plan/updated"
            | "item/reasoning/summaryPartAdded"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "account/rateLimits/updated" => {
                return Ok(AdapterUpdate::default());
            }
            _ => {
                return Ok(self.notification_diagnostic(
                    params,
                    &format!("Unsupported Codex notification {method}"),
                ));
            }
        };
        let mut event = self.event(data);
        event.location.turn_id = string(params, "turnId").or(event.location.turn_id);
        event.location.item_id = string(params, "itemId");
        Ok(self.events(vec![event]))
    }

    fn item(&self, params: &Value, completed: bool) -> Result<AdapterUpdate> {
        let item = &params["item"];
        let kind = required_string(item, "type")?;
        let event = |data| {
            let mut event = self.event(data);
            event.location.item_id = string(item, "id");
            event.location.turn_id = string(params, "turnId").or(event.location.turn_id);
            event
        };
        let data = match (kind, completed) {
            ("imageGeneration", _) => ProviderEventData::MediaGeneration {
                media_type: "image".into(),
                tool: "imageGeneration".into(),
                producing_model: string(item, "model"),
                status: string(item, "status"),
            },
            ("agentMessage", true) => ProviderEventData::AssistantMessage {
                text: required_string(item, "text")?.to_owned(),
            },
            ("contextCompaction", true) => ProviderEventData::Compacted,
            (
                "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" | "webSearch",
                _,
            ) => ProviderEventData::Tool {
                name: string(item, "tool").unwrap_or_else(|| kind.to_owned()),
                phase: string(item, "status")
                    .unwrap_or_else(|| if completed { "completed" } else { "started" }.to_owned()),
            },
            ("collabAgentToolCall", _) => {
                let children = item["receiverThreadIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                if !children.is_empty() {
                    return Ok(self.events(
                        children
                            .into_iter()
                            .map(|child| {
                                event(ProviderEventData::ChildActivity {
                                    child_id: Some(child.to_owned()),
                                    status: string(&item["agentsStates"][child], "status"),
                                })
                            })
                            .collect(),
                    ));
                }
                ProviderEventData::ChildActivity {
                    child_id: None,
                    status: string(item, "status"),
                }
            }
            _ => return Ok(AdapterUpdate::default()),
        };
        let mut events = vec![event(data)];
        if kind == "commandExecution"
            && completed
            && item["status"] == "completed"
            && item["exitCode"].as_i64() == Some(0)
        {
            events.extend(
                item["commandActions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|action| action["type"] == "read")
                    .filter_map(|action| string(action, "path"))
                    .map(|path| {
                        event(ProviderEventData::ContextRead {
                            path,
                            tool: "commandExecution".into(),
                        })
                    }),
            );
        }
        Ok(self.events(events))
    }

    fn server_request(
        &mut self,
        id: RequestId,
        method: &str,
        params: &Value,
    ) -> Result<AdapterUpdate> {
        let allowed = self.active.is_some()
            && self
                .session
                .as_ref()
                .is_some_and(|session| params["threadId"].as_str() == Some(session.id.as_str()))
            && self
                .active
                .as_ref()
                .and_then(|turn| turn.id.as_deref())
                .is_none_or(|turn| params["turnId"].as_str() == Some(turn));
        if !allowed {
            return Ok(AdapterUpdate {
                outbound: vec![self.rpc.error(
                    &id,
                    -32602,
                    "request does not belong to the active turn",
                )?],
                events: vec![],
            });
        }
        ensure!(
            !self.approvals.contains_key(&id) && !self.questions.contains_key(&id),
            "duplicate Codex server request"
        );
        let approval = match method {
            "item/tool/requestUserInput" => {
                let questions = params["questions"]
                    .as_array()
                    .context("Codex planning request has no questions")?;
                let ids = questions
                    .iter()
                    .map(|question| required_string(question, "id").map(str::to_owned))
                    .collect::<Result<Vec<_>>>()?;
                self.questions.insert(id.clone(), ids);
                return Ok(self.events(vec![self.event(ProviderEventData::Question {
                    request_id: id,
                    form: params["questions"].clone(),
                })]));
            }
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                Approval::Decision
            }
            "item/permissions/requestApproval" => {
                Approval::Permissions(params["permissions"].clone())
            }
            _ => {
                return Ok(AdapterUpdate {
                    outbound: vec![self.rpc.error(
                        &id,
                        -32601,
                        "method not supported by OmniAgent",
                    )?],
                    events: vec![],
                });
            }
        };
        let mut event = self.event(ProviderEventData::Approval {
            request_id: id.clone(),
            operation: method.to_owned(),
            description: string(params, "reason").map(|value| diagnostics::preview(&value)),
            prompt: None,
        });
        event.location.item_id = string(params, "itemId");
        self.approvals.insert(id, approval);
        Ok(self.events(vec![event]))
    }
}

impl CliSession for CodexSession {
    fn set_checkout_root(&mut self, root: &Path) {
        self.checkout_root = root.to_owned();
    }

    fn configure_mcp_bridge(&mut self, launch: crate::mcp_bridge::BridgeLaunch) {
        self.bridge = Some(launch);
    }
    fn set_workspace_instructions(&mut self, instructions: String) -> Result<()> {
        ensure!(
            !self.handshake_started,
            "Codex instruction projection must precede startup"
        );
        self.workspace_instructions = Some(instructions);
        Ok(())
    }

    fn arguments(&self) -> Vec<OsString> {
        ["app-server", "--listen", "stdio://"]
            .map(OsString::from)
            .to_vec()
    }

    fn start(&mut self) -> Result<AdapterUpdate> {
        ensure!(!self.handshake_started, "Codex handshake already started");
        self.handshake_started = true;
        Ok(AdapterUpdate {
            outbound: vec![self.rpc.request("initialize", json!({
                "clientInfo": { "name": "omniagent", "title": "OmniAgent", "version": env!("CARGO_PKG_VERSION") },
                "capabilities": { "experimentalApi": true, "explicitGatewayOauth": true },
            }), Request::Initialize)?],
            events: vec![],
        })
    }

    fn query_effective_configuration(&mut self) -> Result<Option<WireMessage>> {
        Ok(Some(self.rpc.request(
            "config/read",
            json!({"cwd": self.request.checkout, "includeLayers": false}),
            Request::ReadConfiguration,
        )?))
    }

    fn query_skill_inventory(&mut self) -> Result<Option<WireMessage>> {
        Ok(Some(self.rpc.request(
            "skills/list",
            json!({"cwds":[self.request.checkout], "forceReload":true}),
            Request::ListSkills,
        )?))
    }

    fn send(&mut self, input: TurnInput) -> Result<AdapterUpdate> {
        ensure!(
            self.active.is_none(),
            "Codex already has a turn in progress"
        );
        let session = self
            .session
            .as_ref()
            .context("Codex session has not started")?;
        let mut params =
            json!({ "threadId": session.id, "input": [{ "type": "text", "text": input.text }] });
        if let Some(effort) = &self.request.effort {
            params["effort"] = json!(effort);
        }
        if self.request.mode == SessionMode::Plan {
            params["collaborationMode"] = json!({
                "mode": "plan",
                "settings": { "model": session.model, "reasoning_effort": self.request.effort.as_ref().or(session.effort.as_ref()) },
            });
        }
        let message = self.rpc.request("turn/start", params, Request::StartTurn)?;
        self.active = Some(ActiveTurn {
            id: None,
            interrupt_requested: false,
        });
        Ok(AdapterUpdate {
            outbound: vec![message],
            events: vec![],
        })
    }

    fn receive(&mut self, frame: OutputFrame) -> Result<AdapterUpdate> {
        if frame.stream == Stream::Stderr {
            return Ok(self.diagnostic(&frame.data));
        }
        let Ok(body) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(self.diagnostic("Malformed Codex protocol frame"));
        };
        let id = rpc::id(&body);
        let params = &body["params"];
        match (body["method"].as_str(), id) {
            (Some(method), Some(id)) => self.server_request(id, method, params),
            (Some(method), None) => Ok(self.notification(method, params).unwrap_or_else(|_| {
                self.notification_diagnostic(
                    params,
                    &format!("Malformed Codex notification {method}"),
                )
            })),
            (None, Some(id)) => self.response(&id, &body),
            _ => Ok(self.diagnostic("Unrecognized Codex protocol frame")),
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
            "read-only Codex sessions cannot grant write or execution approval"
        );
        let approval = self
            .approvals
            .remove(id)
            .context("Codex approval is stale or unknown")?;
        let value = match approval {
            Approval::Decision => json!({ "decision": match decision {
                ApprovalDecision::AllowOnce => "accept", ApprovalDecision::Deny => "decline", ApprovalDecision::Cancel => "cancel",
            }}),
            Approval::Permissions(requested) => json!({
                "permissions": if decision == ApprovalDecision::AllowOnce { requested } else { json!({}) },
                "scope": "turn",
            }),
        };
        Ok(AdapterUpdate {
            outbound: vec![self.rpc.result(id, value)?],
            events: vec![],
        })
    }

    fn respond_question(&mut self, id: &RequestId, answer: Value) -> Result<AdapterUpdate> {
        let questions = self
            .questions
            .get(id)
            .context("Codex question is stale or unknown")?;
        let answers = answer["answers"]
            .as_object()
            .context("Codex question response requires answers")?;
        ensure!(
            answers.len() == questions.len()
                && questions.iter().all(|question| {
                    answers
                        .get(question)
                        .and_then(|value| value["answers"].as_array())
                        .is_some_and(|choices| choices.iter().all(Value::is_string))
                }),
            "Codex question response does not match the active form"
        );
        let result = self.rpc.result(id, answer)?;
        self.questions.remove(id);
        Ok(AdapterUpdate {
            outbound: vec![result],
            events: vec![],
        })
    }

    fn interrupt(&mut self) -> Result<Interrupt> {
        let session = self
            .session
            .as_ref()
            .context("Codex session has not started")?;
        let turn = self.active.as_mut().context("Codex has no active turn")?;
        ensure!(
            !turn.interrupt_requested,
            "Codex turn interrupt already requested"
        );
        turn.interrupt_requested = true;
        let Some(turn_id) = &turn.id else {
            return Ok(Interrupt {
                outbound: vec![],
                grace: Duration::ZERO,
            });
        };
        Ok(Interrupt {
            outbound: vec![self.rpc.request(
                "turn/interrupt",
                json!({ "threadId": session.id, "turnId": turn_id }),
                Request::Interrupt,
            )?],
            grace: Duration::from_secs(5),
        })
    }

    fn process_finished(&mut self, outcome: TerminalOutcome) -> AdapterUpdate {
        let outcome = match outcome {
            TerminalOutcome::Cancelled(_) => TurnOutcome::Interrupted,
            TerminalOutcome::Exited(_) => TurnOutcome::Failed,
        };
        let events = self.finish_turn(outcome);
        self.events(events)
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let Some(result) = value[key]
        .as_str()
        .filter(|text| !text.is_empty() || key == "text" || key == "delta")
    else {
        bail!("Codex protocol field {key} is missing or invalid");
    };
    Ok(result)
}

fn string(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbound(session: &mut dyn CliSession, frame: Value) -> Result<AdapterUpdate> {
        session.receive(OutputFrame {
            stream: Stream::Stdout,
            data: frame.to_string(),
        })
    }

    fn outbound(update: &AdapterUpdate, index: usize) -> Result<Value> {
        Ok(serde_json::from_slice(update.outbound[index].as_bytes())?)
    }

    fn negotiate(
        mode: SessionMode,
        intent: SessionIntent,
        checkout: &Path,
    ) -> Result<Box<dyn CliSession>> {
        let resuming = matches!(&intent, SessionIntent::Resume { .. });
        let mut session = CodexAdapter.create_session(SessionRequest {
            checkout: checkout.to_path_buf(),
            intent,
            mode,
            model: Some("gpt-6.1-sol".into()),
            effort: Some("high".into()),
        })?;
        assert_eq!(outbound(&session.start()?, 0)?["method"], "initialize");
        let initialized = inbound(
            &mut *session,
            json!({"id":1,"result":{"userAgent":"fixture"}}),
        )?;
        assert_eq!(outbound(&initialized, 0)?["method"], "initialized");
        let request = outbound(&initialized, 1)?;
        assert_eq!(request["params"]["model"], "gpt-6.1-sol");
        if resuming {
            assert_eq!(request["method"], "thread/resume");
            assert_eq!(request["params"]["threadId"], "session");
        } else {
            assert_eq!(request["method"], "thread/start");
        }
        let opened = inbound(
            &mut *session,
            json!({"id":2,"result":{
                "thread":{"id":"session"}, "model":"gpt-6.1-sol", "cwd":checkout,
                "sandbox":{"type":if mode == SessionMode::ApprovedWrite { "workspaceWrite" } else { "readOnly" }}
            }}),
        )?;
        assert!(matches!(
            opened.events[0].data,
            ProviderEventData::SessionReady { .. }
        ));
        assert_eq!(outbound(&opened, 0)?["method"], "model/list");
        Ok(session)
    }

    #[test]
    fn separates_reported_generation_from_image_input() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(SessionMode::ReadOnly, SessionIntent::Start, checkout.path())?;
        session.send(TurnInput {
            message_id: "message".into(),
            text: "fixture".into(),
        })?;
        inbound(
            &mut *session,
            json!({"id":4,"result":{"turn":{"id":"turn"}}}),
        )?;
        let update = |kind: &str| {
            json!({"method":"item/completed","params":{
                "threadId":"session","turnId":"turn","item":{"id":"image","type":kind,"status":"completed"}
            }})
        };
        assert!(
            inbound(&mut *session, update("imageView"))?
                .events
                .is_empty()
        );
        let generated = inbound(&mut *session, update("imageGeneration"))?;
        assert_eq!(
            generated.events[0].data,
            ProviderEventData::MediaGeneration {
                media_type: "image".into(),
                tool: "imageGeneration".into(),
                producing_model: None,
                status: Some("completed".into()),
            }
        );
        Ok(())
    }

    #[test]
    fn gathers_model_pages_and_preserves_unknown_output_modalities() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(SessionMode::ReadOnly, SessionIntent::Start, checkout.path())?;
        let page = inbound(
            &mut *session,
            json!({"id":3,"result":{"nextCursor":"next","data":[{
                "id":"catalog-id","model":"gpt-6-luna","displayName":"Luna","hidden":false,
                "inputModalities":["text","image"],
                "supportedReasoningEfforts":[{"reasoningEffort":"low"}],"defaultReasoningEffort":"low"
            }]}}),
        )?;
        assert_eq!(outbound(&page, 0)?["params"]["cursor"], "next");
        assert!(page.events.is_empty());
        let last = inbound(
            &mut *session,
            json!({"id":4,"result":{"data":[{
                "id":"hidden-id","hidden":true
            }]}}),
        )?;
        let ProviderEventData::ModelInventory { models } = &last.events[0].data else {
            anyhow::bail!("model inventory missing")
        };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].canonical_id.as_deref(), Some("gpt-6-luna"));
        assert_eq!(
            models[0].input_modalities.as_deref(),
            Some(&["text".into(), "image".into()][..])
        );
        assert_eq!(models[0].output_modalities, None);
        assert_eq!(models[0].default_effort.as_deref(), Some("low"));
        assert_eq!(models[1].selectable, Some(false));
        assert_eq!(models[1].input_modalities, None);
        Ok(())
    }

    #[test]
    fn streams_a_resumed_turn_with_correlation_and_interrupt() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(
            SessionMode::Plan,
            SessionIntent::Resume {
                provider_session_id: "session".into(),
            },
            checkout.path(),
        )?;
        let text = "first line\nsecond line ☕";
        let request = session.send(TurnInput {
            message_id: "message".into(),
            text: text.into(),
        })?;
        assert_eq!(outbound(&request, 0)?["params"]["input"][0]["text"], text);
        assert_eq!(
            outbound(&request, 0)?["params"]["collaborationMode"]["mode"],
            "plan"
        );
        assert!(
            session
                .send(TurnInput {
                    message_id: "another".into(),
                    text: "blocked".into()
                })
                .is_err()
        );
        let start = inbound(
            &mut *session,
            json!({"method":"turn/started","params":{
                "threadId":"session","turn":{"id":"turn","status":"inProgress"}
            }}),
        )?;
        assert_eq!(start.events[0].location.turn_id.as_deref(), Some("turn"));
        assert!(
            inbound(
                &mut *session,
                json!({"id":4,"result":{"turn":{"id":"turn"}}})
            )?
            .events
            .is_empty()
        );
        let unrelated = inbound(
            &mut *session,
            json!({"method":"item/agentMessage/delta","params":{
                "threadId":"session","turnId":"previous","itemId":"other","delta":"stale"
            }}),
        )?;
        assert!(unrelated.events.is_empty());
        let delta = inbound(
            &mut *session,
            json!({"method":"item/agentMessage/delta","params":{
                "threadId":"session","turnId":"turn","itemId":"response","delta":"partial"
            }}),
        )?;
        assert_eq!(
            delta.events[0].data,
            ProviderEventData::AssistantDelta {
                text: "partial".into()
            }
        );
        let reasoning = inbound(
            &mut *session,
            json!({"method":"item/reasoning/summaryTextDelta","params":{
                "threadId":"session","turnId":"turn","itemId":"thought","delta":"summary"
            }}),
        )?;
        assert!(matches!(
            reasoning.events[0].data,
            ProviderEventData::Reasoning { summary: true, .. }
        ));
        let children = inbound(
            &mut *session,
            json!({"method":"item/completed","params":{
                "threadId":"session", "turnId":"turn", "item":{"id":"call","type":"collabAgentToolCall",
                    "receiverThreadIds":["child"],"agentsStates":{"child":{"status":"completed"}}}
            }}),
        )?;
        assert_eq!(
            children.events[0].data,
            ProviderEventData::ChildActivity {
                child_id: Some("child".into()),
                status: Some("completed".into())
            }
        );
        let interrupt = session.interrupt()?;
        let wire: Value = serde_json::from_slice(interrupt.outbound[0].as_bytes())?;
        assert_eq!(wire["method"], "turn/interrupt");
        assert_eq!(wire["params"]["turnId"], "turn");
        let finished = inbound(
            &mut *session,
            json!({"method":"turn/completed","params":{
                "threadId":"session","turn":{"id":"turn","status":"interrupted"}
            }}),
        )?;
        assert_eq!(
            finished.events[0].data,
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

    #[test]
    fn routes_approvals_once_and_preserves_question_ids() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(
            SessionMode::ApprovedWrite,
            SessionIntent::Start,
            checkout.path(),
        )?;
        session.send(TurnInput {
            message_id: "message".into(),
            text: "run".into(),
        })?;
        inbound(
            &mut *session,
            json!({"id":4,"result":{"turn":{"id":"turn"}}}),
        )?;
        let request = inbound(
            &mut *session,
            json!({"id":"approval","method":"item/fileChange/requestApproval",
            "params":{"threadId":"session","turnId":"turn","itemId":"patch","reason":"save generated file"}}),
        )?;
        assert_eq!(request.events[0].location.item_id.as_deref(), Some("patch"));
        let approval_id = RequestId::Text("approval".into());
        let decision = session.respond_approval(&approval_id, ApprovalDecision::AllowOnce)?;
        assert_eq!(
            outbound(&decision, 0)?,
            json!({"id":"approval","result":{"decision":"accept"}})
        );
        assert!(
            session
                .respond_approval(&approval_id, ApprovalDecision::Deny)
                .is_err()
        );

        let prompts = inbound(
            &mut *session,
            json!({"id":91,"method":"item/tool/requestUserInput",
            "params":{"threadId":"session","turnId":"turn","questions":[{"id":"q-1","question":"Which?"}]}}),
        )?;
        assert!(matches!(
            &prompts.events[0].data,
            ProviderEventData::Question {
                request_id: RequestId::Number(91),
                ..
            }
        ));
        assert!(
            session
                .respond_question(
                    &RequestId::Number(91),
                    json!({"answers":{"wrong":{"answers":["a"]}}})
                )
                .is_err()
        );
        let answer = json!({"answers":{"q-1":{"answers":["free text"]}}});
        let response = session.respond_question(&RequestId::Number(91), answer.clone())?;
        assert_eq!(outbound(&response, 0)?["result"], answer);
        Ok(())
    }
}
