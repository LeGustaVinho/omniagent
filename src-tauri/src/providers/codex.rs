use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{
    AdapterUpdate, ApprovalDecision, AuthProbe, Authentication, CliAdapter, CliSession,
    DelegationControls, ForkBoundary, Interrupt, ProbeKind, Provider, ProviderEvent,
    ProviderEventData, ProviderLocation, ReportedModel, RequestId, SessionIntent, SessionMode,
    SessionRequest, Transport, TurnInput, TurnOutcome, WireMessage, capabilities,
    goal::{NativeGoalObservation, NativeGoalState, NativeGoalSupport},
    rpc::{self, Rpc},
    skills::{SkillCatalog, SkillCatalogIssue, SkillEntry, SkillSource},
    steering::{SteerOutcome, SteerTarget},
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
        if let SessionIntent::Fork {
            provider_session_id,
            boundary,
        } = &request.intent
        {
            let ForkBoundary::CompletedTurn { turn_id } = boundary else {
                bail!("Codex native forks require a completed provider turn boundary");
            };
            ensure!(
                !provider_session_id.trim().is_empty() && !turn_id.trim().is_empty(),
                "Codex native forks require a session and turn ID"
            );
        }
        Ok(Box::new(CodexSession {
            checkout_root: request.checkout.clone(),
            bridge: None,
            request,
            rpc: Rpc::new(false),
            handshake_started: false,
            session: None,
            pending_fork: None,
            goal_command: None,
            active: None,
            pending_steer: None,
            model_pages: Vec::new(),
            workspace_instructions: None,
            model_cursors: HashSet::new(),
            approvals: HashMap::new(),
            questions: HashMap::new(),
            question_route: Default::default(),
        }))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    Initialize,
    OpenSession,
    VerifyFork,
    ReadConfiguration,
    ListSkills,
    ModelPage,
    StartTurn,
    Steer,
    StartCompaction,
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
    compaction: Option<Compaction>,
}

struct PendingSteer {
    request_id: RequestId,
    message_id: String,
    target: SteerTarget,
}

#[derive(Default)]
struct Compaction {
    acknowledged: bool,
    observed: bool,
    outcome: Option<TurnOutcome>,
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
    pending_fork: Option<Session>,
    goal_command: Option<bool>,
    active: Option<ActiveTurn>,
    pending_steer: Option<PendingSteer>,
    model_pages: Vec<ReportedModel>,
    workspace_instructions: Option<String>,
    model_cursors: HashSet<String>,
    approvals: HashMap<RequestId, Approval>,
    questions: HashMap<RequestId, super::questions::QuestionForm>,
    question_route: super::questions::QuestionRoute,
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
            SessionIntent::Fork {
                provider_session_id,
                boundary: ForkBoundary::CompletedTurn { turn_id },
            } => {
                params["threadId"] = json!(provider_session_id);
                params["lastTurnId"] = json!(turn_id);
                params["excludeTurns"] = json!(true);
                "thread/fork"
            }
            SessionIntent::Fork { .. } => bail!("unsupported Codex fork boundary"),
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
        if request == Request::Steer {
            if self
                .pending_steer
                .as_ref()
                .is_none_or(|pending| pending.request_id != *id)
            {
                return Ok(AdapterUpdate::default());
            }
            let events = self.finish_steering(response);
            return Ok(self.events(events));
        }
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
            if request == Request::StartCompaction {
                events.extend(self.finish_compaction(TurnOutcome::Failed));
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
                let session = Session {
                    id: id.to_owned(),
                    model: model.to_owned(),
                    effort: string(result, "reasoningEffort"),
                };
                if let SessionIntent::Fork {
                    provider_session_id,
                    ..
                } = &self.request.intent
                {
                    ensure!(
                        provider_session_id != id,
                        "Codex returned the source session for a fork"
                    );
                    let request = self.rpc.request(
                        "thread/turns/list",
                        json!({"threadId": id, "limit": 1, "sortDirection": "desc", "itemsView": "notLoaded"}),
                        Request::VerifyFork,
                    )?;
                    self.pending_fork = Some(session);
                    return Ok(AdapterUpdate {
                        outbound: vec![request],
                        events: Vec::new(),
                    });
                }
                self.session = Some(session);
                let mut events = Vec::new();
                if matches!(self.request.intent, SessionIntent::Resume { .. })
                    && let Some(turns) = result["thread"]["turns"].as_array()
                {
                    events.push(self.event(ProviderEventData::HistoryReported {
                        turns: Some(turns.len() as u64),
                        item_kind: None,
                    }));
                }
                self.opened(events)
            }
            Request::VerifyFork => {
                let SessionIntent::Fork {
                    provider_session_id,
                    boundary: ForkBoundary::CompletedTurn { turn_id },
                } = &self.request.intent
                else {
                    bail!("unexpected Codex fork verification");
                };
                let session = self
                    .pending_fork
                    .take()
                    .context("Codex did not return a fork session")?;
                let turns = result["data"]
                    .as_array()
                    .context("Codex did not return fork turns")?;
                ensure!(
                    matches!(turns.as_slice(), [last] if last["id"].as_str() == Some(turn_id) && last["status"] == "completed"),
                    "Codex fork did not end at the requested completed turn"
                );
                let verification = ProviderEventData::NativeForkVerified {
                    source_session_id: provider_session_id.clone(),
                    last_turn_id: turn_id.clone(),
                };
                self.session = Some(session);
                self.opened(vec![self.event(verification)])
            }
            Request::ReadConfiguration => {
                let mut codex_instructions = None;
                let mut qualification = None;
                let mut events = Vec::new();
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
                    if let Some(available) =
                        config.pointer("/features/goals").and_then(Value::as_bool)
                    {
                        self.goal_command = Some(available);
                        events.push(
                            self.event(ProviderEventData::NativeGoalCommandReported { available }),
                        );
                    }
                    let agents = &config["agents"];
                    let enabled = agents["enabled"].as_bool();
                    let concurrent_spawned_threads = agents["max_concurrent_threads_per_session"]
                        .as_u64()
                        .or_else(|| agents["max_threads"].as_u64());
                    if enabled.is_some() || concurrent_spawned_threads.is_some() {
                        events.push(self.event(ProviderEventData::DelegationControlsReported {
                            controls: DelegationControls::CodexSessionTree {
                                enabled,
                                concurrent_spawned_threads,
                            },
                        }));
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
                events.push(self.event(ProviderEventData::ConfigurationReported {
                    revision,
                    codex_instructions,
                    qualification,
                }));
                Ok(self.events(events))
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
                let mut events = self.learn_turn(id)?;
                if self.request.mode == SessionMode::Plan && self.active.is_some() {
                    events.push(self.event(ProviderEventData::PlanningModeConfirmed {
                        mode: super::planning::PlanningMode::CodexCollaboration,
                    }));
                }
                Ok(self.events(events))
            }
            Request::Steer => unreachable!("steering replies are handled before other responses"),
            Request::StartCompaction => {
                if let Some(progress) = self
                    .active
                    .as_mut()
                    .and_then(|turn| turn.compaction.as_mut())
                {
                    progress.acknowledged = true;
                }
                let events = self.settle_compaction();
                Ok(self.events(events))
            }
            Request::Interrupt => Ok(AdapterUpdate::default()),
        }
    }

    fn opened(&mut self, mut events: Vec<ProviderEvent>) -> Result<AdapterUpdate> {
        let session = self
            .session
            .as_ref()
            .context("Codex session has not started")?;
        events.push(self.event(ProviderEventData::SessionReady {
            model: Some(session.model.clone()),
            effort: session.effort.clone(),
        }));
        if matches!(self.request.intent, SessionIntent::Start) {
            events.push(self.event(ProviderEventData::FreshSessionOpened));
        }
        Ok(AdapterUpdate {
            outbound: vec![self.rpc.request(
                "model/list",
                json!({"limit": 100}),
                Request::ModelPage,
            )?],
            events,
        })
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
        Ok(if turn.compaction.is_some() {
            Vec::new()
        } else {
            vec![self.event(ProviderEventData::TurnStarted)]
        })
    }

    fn finish_turn(&mut self, outcome: TurnOutcome) -> Vec<ProviderEvent> {
        if self.active.is_none() {
            return Vec::new();
        }
        let finished = self.event(ProviderEventData::TurnFinished { outcome });
        self.active = None;
        self.approvals.clear();
        self.questions.clear();
        let mut events = self.finish_steering(&Value::Null);
        events.push(finished);
        events
    }

    fn finish_steering(&mut self, response: &Value) -> Vec<ProviderEvent> {
        let Some(pending) = self.pending_steer.take() else {
            return Vec::new();
        };
        self.rpc.take_request(&pending.request_id);
        let outcome = if let Some(error) = response.get("error") {
            match error["code"].as_i64() {
                Some(-32601) => SteerOutcome::Unavailable,
                Some(-32600 | -32602) => SteerOutcome::ProviderRejected,
                _ => SteerOutcome::Unconfirmed,
            }
        } else if response.pointer("/result/turnId").and_then(Value::as_str)
            == Some(&pending.target.turn_id)
        {
            SteerOutcome::Accepted
        } else {
            SteerOutcome::Unconfirmed
        };
        vec![pending.target.settlement(pending.message_id, outcome)]
    }

    fn finish_compaction(&mut self, outcome: TurnOutcome) -> Vec<ProviderEvent> {
        let finished = self.event(ProviderEventData::NativeCompactionFinished { outcome });
        self.active = None;
        self.approvals.clear();
        self.questions.clear();
        vec![finished]
    }

    fn settle_compaction(&mut self) -> Vec<ProviderEvent> {
        let Some(progress) = self
            .active
            .as_ref()
            .and_then(|turn| turn.compaction.as_ref())
        else {
            return Vec::new();
        };
        match (progress.acknowledged, progress.outcome, progress.observed) {
            (true, Some(TurnOutcome::Completed), false) => {
                self.finish_compaction(TurnOutcome::Failed)
            }
            (true, Some(outcome), _) => self.finish_compaction(outcome),
            _ => Vec::new(),
        }
    }

    fn notification(&mut self, method: &str, params: &Value) -> Result<AdapterUpdate> {
        if matches!(method, "thread/goal/updated" | "thread/goal/cleared") {
            return self.goal_notification(method, params);
        }
        if self.active.is_none() && method != "error" {
            return Ok(AdapterUpdate::default());
        }
        let thread = params["threadId"].as_str();
        let is_compaction = self
            .active
            .as_ref()
            .is_some_and(|turn| turn.compaction.is_some());
        if is_compaction && thread.is_none() {
            return Ok(AdapterUpdate::default());
        }
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
                if let Some(progress) = self
                    .active
                    .as_mut()
                    .and_then(|turn| turn.compaction.as_mut())
                {
                    progress.outcome = Some(outcome);
                    events.extend(self.settle_compaction());
                } else {
                    events.extend(self.finish_turn(outcome));
                }
            }
            return Ok(self.events(events));
        }
        let delta = || required_string(params, "delta").map(str::to_owned);
        let data = match method {
            "serverRequest/resolved" => {
                let id = serde_json::from_value::<RequestId>(params["requestId"].clone())?;
                if self.questions.remove(&id).is_none() {
                    return Ok(AdapterUpdate::default());
                }
                ProviderEventData::QuestionClosed { request_id: id }
            }
            "thread/settings/updated" => {
                let settings = &params["threadSettings"];
                ProviderEventData::ModelObserved {
                    model: string(settings, "model"),
                    effort: string(settings, "effort"),
                }
            }
            "item/agentMessage/delta" => ProviderEventData::AssistantDelta { text: delta()? },
            "item/plan/delta" => ProviderEventData::PlanDelta { text: delta()? },
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
                if is_compaction {
                    let item = &params["item"];
                    if item["type"] != "contextCompaction" || method != "item/completed" {
                        return Ok(AdapterUpdate::default());
                    }
                    let Some(turn_id) = params["turnId"].as_str() else {
                        return Ok(AdapterUpdate::default());
                    };
                    self.learn_turn(turn_id)?;
                    if let Some(progress) = self
                        .active
                        .as_mut()
                        .and_then(|turn| turn.compaction.as_mut())
                    {
                        progress.observed = true;
                    }
                }
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

    fn goal_notification(&self, method: &str, params: &Value) -> Result<AdapterUpdate> {
        let id = required_string(params, "threadId")?;
        if self.session.as_ref().is_none_or(|session| session.id != id) {
            return Ok(AdapterUpdate::default());
        }
        let cleared = method == "thread/goal/cleared";
        let goal = if cleared {
            &Value::Null
        } else {
            &params["goal"]
        };
        let (state, reported_state) = if cleared {
            (NativeGoalState::Cleared, None)
        } else {
            ensure!(
                goal["threadId"].as_str() == Some(id),
                "Codex goal belongs to another thread"
            );
            let status = required_string(goal, "status")?;
            let state = match status {
                "active" => NativeGoalState::Active,
                "paused" => NativeGoalState::Paused,
                "blocked" => NativeGoalState::Blocked,
                "budgetLimited" => NativeGoalState::BudgetLimited,
                "usageLimited" => NativeGoalState::UsageLimited,
                "complete" => NativeGoalState::Completed,
                _ => NativeGoalState::Unknown,
            };
            (state, Some(status.to_owned()))
        };
        let provider_turn_id = if cleared {
            None
        } else {
            string(params, "turnId").filter(|id| !id.is_empty())
        };
        let observation = NativeGoalObservation {
            provider_session_id: id.to_owned(),
            provider_goal_id: None,
            provider_turn_id: provider_turn_id.clone(),
            state,
            reported_state,
            tokens_used: goal["tokensUsed"].as_u64(),
            token_budget: goal["tokenBudget"].as_u64(),
            elapsed_seconds: goal["timeUsedSeconds"].as_u64(),
            evaluation: None,
        };
        let mut event = self.event(ProviderEventData::GoalObserved { observation });
        event.location.turn_id = provider_turn_id;
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
            ("plan", true) => ProviderEventData::PlanProposed {
                text: required_string(item, "text")?.to_owned(),
                source: super::planning::PlanSource::CodexPlanItem,
            },
            ("plan", false) => return Ok(AdapterUpdate::default()),
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
                if !self.question_route.is_native() {
                    return Ok(AdapterUpdate {
                        outbound: vec![self.rpc.error(
                            &id,
                            -32601,
                            "this attempt collects questions at turn completion",
                        )?],
                        events: Vec::new(),
                    });
                }
                let normalized = match super::questions::QuestionForm::codex(&params["questions"]) {
                    Ok(form) => form,
                    Err(error) => {
                        return Ok(AdapterUpdate {
                            outbound: vec![self.rpc.error(
                                &id,
                                -32602,
                                "unsupported Codex question form",
                            )?],
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
                    form: params["questions"].clone(),
                    normalized: Some(normalized),
                    route: self.question_route,
                });
                event.location.item_id = string(params, "itemId");
                event.location.turn_id = string(params, "turnId");
                return Ok(self.events(vec![event]));
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
    fn native_goal_support(&self) -> NativeGoalSupport {
        NativeGoalSupport::from_advertisement(self.goal_command)
    }

    fn set_question_route(&mut self, route: super::questions::QuestionRoute) {
        self.question_route = route;
    }

    fn attachment_encoding(
        &self,
        format: crate::attachments::AttachmentFormat,
    ) -> Option<crate::attachments::NativeAttachmentEncoding> {
        format
            .is_image()
            .then_some(crate::attachments::NativeAttachmentEncoding::CodexLocalImage)
    }
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
        let mut items = Vec::new();
        if !input.text.is_empty() || input.attachments.is_empty() {
            items.push(json!({"type": "text", "text": input.text}));
        }
        for attachment in &input.attachments {
            items.push(attachment.block(Provider::Codex)?);
        }
        let mut params = json!({ "threadId": session.id, "input": items });
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
            compaction: None,
        });
        Ok(AdapterUpdate {
            outbound: vec![message],
            events: vec![],
        })
    }

    fn compact_native(&mut self) -> Result<AdapterUpdate> {
        ensure!(
            self.active.is_none(),
            "Codex already has a turn in progress"
        );
        let session = self
            .session
            .as_ref()
            .context("Codex session has not started")?;
        let message = self.rpc.request(
            "thread/compact/start",
            json!({"threadId": session.id}),
            Request::StartCompaction,
        )?;
        self.active = Some(ActiveTurn {
            id: None,
            interrupt_requested: false,
            compaction: Some(Compaction::default()),
        });
        Ok(AdapterUpdate {
            outbound: vec![message],
            events: Vec::new(),
        })
    }

    fn steer(&mut self, target: SteerTarget, input: TurnInput) -> Result<AdapterUpdate> {
        let Some((session, active)) = self.session.as_ref().zip(self.active.as_ref()) else {
            return Ok(target.rejected(input.message_id, SteerOutcome::StaleTurn));
        };
        if target.session_id != session.id
            || active.id.as_deref() != Some(&target.turn_id)
            || active.interrupt_requested
            || active.compaction.is_some()
        {
            return Ok(target.rejected(input.message_id, SteerOutcome::StaleTurn));
        }
        if input.text.trim().is_empty()
            || !input.attachments.is_empty()
            || input.message_id.trim().is_empty()
        {
            return Ok(target.rejected(input.message_id, SteerOutcome::UnsupportedInput));
        }
        if self.pending_steer.is_some() {
            return Ok(target.rejected(input.message_id, SteerOutcome::Busy));
        }
        let (request_id, wire) = self.rpc.request_with_id(
            "turn/steer",
            json!({
                "threadId": target.session_id,
                "expectedTurnId": target.turn_id,
                "input": [{"type": "text", "text": input.text}],
            }),
            Request::Steer,
        )?;
        let event = ProviderEvent {
            location: target.location(),
            data: ProviderEventData::SteeringPrepared {
                message_id: input.message_id.clone(),
                target: target.clone(),
                frame: wire.as_bytes().to_vec(),
            },
        };
        self.pending_steer = Some(PendingSteer {
            request_id,
            message_id: input.message_id,
            target,
        });
        Ok(AdapterUpdate {
            outbound: vec![wire],
            events: vec![event],
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
        use super::questions::{QuestionAction, QuestionReply};
        let questions = self
            .questions
            .get(id)
            .context("Codex question is stale or unknown")?;
        let reply: QuestionReply = if answer.get("action").is_some() {
            serde_json::from_value(answer)?
        } else {
            let values = answer["answers"]
                .as_object()
                .context("Codex response requires answers")?
                .iter()
                .map(|(field, item)| {
                    let answers = item["answers"]
                        .as_array()
                        .context("Codex answer requires a selection")?;
                    ensure!(
                        answers.len() == 1,
                        "Codex questions accept one answer per field"
                    );
                    Ok((field.clone(), answers[0].clone()))
                })
                .collect::<Result<_>>()?;
            QuestionReply {
                action: QuestionAction::Accept,
                values,
            }
        };
        questions.validate(&reply)?;
        let answers: serde_json::Map<_, _> = reply
            .values
            .iter()
            .map(|(field, value)| (field.clone(), json!({"answers":[value]})))
            .collect();
        let result = self.rpc.result(id, json!({"answers": answers}))?;
        self.questions.remove(id);
        Ok(super::questions::encoded_reply(id, reply, result))
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
        let events = if self
            .active
            .as_ref()
            .is_some_and(|turn| turn.compaction.is_some())
        {
            self.finish_compaction(outcome)
        } else {
            self.finish_turn(outcome)
        };
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

    #[test]
    fn steering_uses_the_active_provider_turn_and_retains_ambiguous_delivery() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(SessionMode::ReadOnly, SessionIntent::Start, checkout.path())?;
        let target = SteerTarget {
            session_id: "session".into(),
            turn_id: "turn".into(),
        };
        let input = |id: &str| TurnInput {
            message_id: id.to_owned(),
            text: "Instructions for this turn.".into(),
            attachments: Vec::new(),
        };
        let outcome = |update: &AdapterUpdate| match update.events.as_slice() {
            [
                ProviderEvent {
                    data: ProviderEventData::SteeringSettled { outcome, .. },
                    ..
                },
            ] => *outcome,
            _ => panic!("expected a steering settlement"),
        };
        let stale = session.steer(target.clone(), input("stale"))?;
        assert_eq!(outcome(&stale), SteerOutcome::StaleTurn);
        assert!(stale.outbound.is_empty());

        session.send(input("original"))?;
        inbound(
            &mut *session,
            json!({
                "method":"turn/started", "params":{"threadId":"session","turn":{"id":"turn"}}
            }),
        )?;
        let prepared = session.steer(target.clone(), input("first"))?;
        let encoded = outbound(&prepared, 0)?;
        assert_eq!(encoded["method"], "turn/steer");
        assert_eq!(encoded["params"]["expectedTurnId"], "turn");
        assert!(matches!(prepared.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::SteeringPrepared { message_id, frame, .. }, ..
        }] if message_id == "first" && frame == prepared.outbound[0].as_bytes()));
        let accepted = inbound(
            &mut *session,
            json!({"id":encoded["id"],"result":{"turnId":"turn"}}),
        )?;
        assert_eq!(outcome(&accepted), SteerOutcome::Accepted);
        assert_eq!(accepted.events[0].location.turn_id.as_deref(), Some("turn"));

        let rejected = outbound(
            &session.steer(target.clone(), input("provider-rejected"))?,
            0,
        )?;
        let response = inbound(
            &mut *session,
            json!({
                "id": rejected["id"], "error":{"code":-32600,"message":"no active turn to steer"}
            }),
        )?;
        assert!(matches!(response.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::SteeringSettled { message_id, outcome: SteerOutcome::ProviderRejected, .. }, ..
        }] if message_id == "provider-rejected"));
        assert!(response.outbound.is_empty());

        let ambiguous = outbound(&session.steer(target.clone(), input("ambiguous"))?, 0)?;
        let response = inbound(
            &mut *session,
            json!({
                "id": ambiguous["id"], "result":{"turnId":"different-turn"}
            }),
        )?;
        assert_eq!(outcome(&response), SteerOutcome::Unconfirmed);
        assert!(response.outbound.is_empty());

        let next = outbound(&session.steer(target, input("late"))?, 0)?;
        let terminal = inbound(
            &mut *session,
            json!({
                "method":"turn/completed", "params":{"threadId":"session","turn":{"id":"turn","status":"completed"}}
            }),
        )?;
        assert!(matches!(&terminal.events[0].data,
            ProviderEventData::SteeringSettled { message_id, outcome: SteerOutcome::Unconfirmed, .. }
                if message_id == "late"
        ));
        let late = inbound(
            &mut *session,
            json!({"id":next["id"],"result":{"turnId":"turn"}}),
        )?;
        assert!(
            late.events
                .iter()
                .all(|event| !matches!(event.data, ProviderEventData::SteeringSettled { .. }))
        );
        Ok(())
    }

    #[test]
    fn restored_goal_state_and_clear_are_separate_from_turn_cancellation() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(
            SessionMode::ReadOnly,
            SessionIntent::Resume {
                provider_session_id: "session".into(),
            },
            checkout.path(),
        )?;
        let restored = inbound(
            &mut *session,
            json!({
                "method":"thread/goal/updated","params":{
                    "threadId":"session","goal":{"threadId":"session","status":"active","tokensUsed":5,"tokenBudget":256,"timeUsedSeconds":7}
                }
            }),
        )?;
        assert!(matches!(restored.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::GoalObserved { observation },
            location
        }] if observation.state == NativeGoalState::Active && observation.tokens_used == Some(5)
            && observation.elapsed_seconds == Some(7) && location.turn_id.is_none()
            && observation.provider_goal_id.is_none() && observation.evaluation.is_none()));

        session.send(TurnInput {
            message_id: "work".into(),
            text: "offline fixture".into(),
            attachments: Vec::new(),
        })?;
        inbound(
            &mut *session,
            json!({
                "method":"turn/started","params":{"threadId":"session","turn":{"id":"turn"}}
            }),
        )?;
        let unrelated = inbound(
            &mut *session,
            json!({
                "method":"thread/goal/cleared","params":{"threadId":"other"}
            }),
        )?;
        assert!(unrelated.events.is_empty());
        let budget = inbound(
            &mut *session,
            json!({
                "method":"thread/goal/updated","params":{
                    "threadId":"session","turnId":"turn",
                    "goal":{"threadId":"session","status":"budgetLimited","tokensUsed":4096,"tokenBudget":256}
                }
            }),
        )?;
        assert!(matches!(budget.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::GoalObserved { observation },
            location
        }] if observation.state == NativeGoalState::BudgetLimited && observation.token_budget == Some(256)
            && observation.tokens_used == Some(4096) && location.turn_id.as_deref() == Some("turn")
            && observation.provider_turn_id == location.turn_id));
        let cleared = inbound(
            &mut *session,
            json!({
                "method":"thread/goal/cleared","params":{"threadId":"session"}
            }),
        )?;
        assert!(matches!(cleared.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::GoalObserved { observation },
            location
        }] if observation.state == NativeGoalState::Cleared && location.turn_id.is_none()));
        let interrupted = inbound(
            &mut *session,
            json!({
                "method":"turn/completed","params":{"threadId":"session","turn":{"id":"turn","status":"interrupted"}}
            }),
        )?;
        assert!(matches!(interrupted.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::TurnFinished { outcome: TurnOutcome::Interrupted },
            location
        }] if location.turn_id.as_deref() == Some("turn")));
        Ok(())
    }

    #[test]
    fn rejects_message_items_and_a_fork_with_a_different_last_turn() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let request = |boundary| SessionRequest {
            checkout: checkout.path().into(),
            intent: SessionIntent::Fork {
                provider_session_id: "original".into(),
                boundary,
            },
            mode: SessionMode::ReadOnly,
            model: None,
            effort: None,
        };
        assert!(
            CodexAdapter
                .create_session(request(ForkBoundary::MessageItem {
                    item_id: "inside-turn".into(),
                }))
                .is_err()
        );
        let mut session = CodexAdapter.create_session(request(ForkBoundary::CompletedTurn {
            turn_id: "boundary".into(),
        }))?;
        session.start()?;
        let initialize = inbound(
            &mut *session,
            json!({"id":1,"result":{"userAgent":"fixture"}}),
        )?;
        let fork = outbound(&initialize, 1)?;
        assert_eq!(fork["method"], "thread/fork");
        assert_eq!(fork["params"]["lastTurnId"], "boundary");
        let opened = inbound(
            &mut *session,
            json!({
                "id": fork["id"], "result": {
                    "thread": {"id": "forked"},
                    "cwd": checkout.path(),
                    "sandbox": {"type":"readOnly"},
                    "model": "fixture-model"
                }
            }),
        )?;
        assert!(opened.events.is_empty());
        let query = outbound(&opened, 0)?;
        assert_eq!(query["method"], "thread/turns/list");
        assert_eq!(query["params"]["threadId"], "forked");
        assert!(
            inbound(
                &mut *session,
                json!({"id":query["id"],
                    "result":{"data":[{"id":"later-turn","status":"completed"}]}
                })
            )
            .is_err()
        );
        assert!(session.compact_native().is_err());
        assert!(
            session
                .send(TurnInput {
                    message_id: "message".into(),
                    text: "offline fixture".into(),
                    attachments: Vec::new(),
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn compaction_requires_acknowledgement_and_matching_provider_evidence() -> Result<()> {
        let checkout = tempfile::tempdir()?;
        let mut session = negotiate(SessionMode::ReadOnly, SessionIntent::Start, checkout.path())?;
        let start = session.compact_native()?;
        let request = outbound(&start, 0)?;
        assert_eq!(request["method"], "thread/compact/start");
        assert!(session.compact_native().is_err());
        let started = inbound(
            &mut *session,
            json!({"method":"turn/started","params":{
                "threadId":"session", "turn":{"id":"compact"}
            }}),
        )?;
        assert!(started.events.is_empty());
        let unrelated = inbound(
            &mut *session,
            json!({"method":"item/completed","params":{
                "threadId":"session", "turnId":"other", "item":{"id":"other-item","type":"contextCompaction"}
            }}),
        )?;
        assert!(unrelated.events.is_empty());
        let terminal = inbound(
            &mut *session,
            json!({"method":"turn/completed","params":{
                "threadId":"session", "turn":{"id":"compact","status":"completed"}
            }}),
        )?;
        assert!(terminal.events.is_empty());
        let ack = inbound(&mut *session, json!({"id":request["id"],"result":{}}))?;
        assert!(matches!(ack.events.as_slice(), [ProviderEvent {
            data: ProviderEventData::NativeCompactionFinished { outcome: TurnOutcome::Failed },
            location
        }] if location.turn_id.as_deref() == Some("compact")));
        Ok(())
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
            attachments: Vec::new(),
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
            attachments: Vec::new(),
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
                    attachments: Vec::new(),
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
        let accepted = inbound(
            &mut *session,
            json!({"id":4,"result":{"turn":{"id":"turn"}}}),
        )?;
        assert!(
            matches!(accepted.events.as_slice(), [event] if event.data == ProviderEventData::PlanningModeConfirmed {
                mode: crate::providers::planning::PlanningMode::CodexCollaboration
            })
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
            attachments: Vec::new(),
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
            "params":{"threadId":"session","turnId":"turn","itemId":"ask-item",
                "questions":[{"id":"q-1","question":"Which?","isOther":true,"options":[
                    {"label":"Plan A","description":"First approach"},{"label":"Plan B","description":"Second approach"}
                ]}]}}),
        )?;
        assert!(matches!(
            &prompts.events[0].data,
            ProviderEventData::Question {
                request_id: RequestId::Number(91),
                normalized: Some(form),
                ..
            } if form.fields[0].id == "q-1" && matches!(
                &form.fields[0].input,
                crate::providers::questions::QuestionInput::Choice { options, allow_custom: true, .. }
                    if options[0].id == "Plan A" && options[0].description.as_deref() == Some("First approach")
            )
        ));
        assert_eq!(
            prompts.events[0].location.item_id.as_deref(),
            Some("ask-item")
        );
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
        inbound(
            &mut *session,
            json!({
                "id":92, "method":"item/tool/requestUserInput",
                "params":{"threadId":"session","turnId":"turn","questions":[{"id":"q-2","question":"Another?"}]}
            }),
        )?;
        let closed = inbound(
            &mut *session,
            json!({
                "method":"serverRequest/resolved", "params":{"threadId":"session","requestId":92}
            }),
        )?;
        assert_eq!(
            closed.events[0].data,
            ProviderEventData::QuestionClosed {
                request_id: RequestId::Number(92)
            }
        );
        assert!(
            session
                .respond_question(
                    &RequestId::Number(92),
                    json!({"action":"accept","values":{"q-2":"yes"}})
                )
                .is_err()
        );
        Ok(())
    }
}
