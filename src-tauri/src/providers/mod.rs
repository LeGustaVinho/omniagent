use std::{
    env,
    ffi::OsString,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{process::TerminalOutcome, process_io::OutputFrame};

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum Provider {
    Codex,
    Claude,
    Antigravity,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::Antigravity];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Antigravity => "antigravity",
        }
    }

    pub fn adapter(self) -> &'static dyn CliAdapter {
        match self {
            Self::Codex => &codex::CodexAdapter,
            Self::Claude => &claude::ClaudeAdapter,
            Self::Antigravity => &antigravity::AntigravityAdapter,
        }
    }

    pub fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex.exe",
            Self::Claude => "claude.exe",
            Self::Antigravity => "agy.exe",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    CodexAppServer,
    ClaudeStreamJson,
    AgentClientProtocol,
}

pub struct AuthProbe {
    pub kind: ProbeKind,
    pub arguments: Vec<OsString>,
}

pub enum ProbeKind {
    Command,
    SessionHandshake,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum Authentication {
    Authenticated,
    Required,
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ModelInventory,
    Resume,
    Reasoning,
    ToolApprovals,
    PlanningQuestions,
    CancelTurn,
    SubagentEvents,
    NativeCompaction,
    NativeSuggestions,
    SideRequest,
    WorkspaceWrite,
}

impl Capability {
    pub const ALL: [Self; 11] = [
        Self::ModelInventory,
        Self::Resume,
        Self::Reasoning,
        Self::ToolApprovals,
        Self::PlanningQuestions,
        Self::CancelTurn,
        Self::SubagentEvents,
        Self::NativeCompaction,
        Self::NativeSuggestions,
        Self::SideRequest,
        Self::WorkspaceWrite,
    ];
}

#[derive(Clone)]
pub enum SessionIntent {
    Start,
    Resume { provider_session_id: String },
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    ReadOnly,
    Plan,
    ApprovedWrite,
}

pub struct SessionRequest {
    pub checkout: PathBuf,
    pub intent: SessionIntent,
    pub mode: SessionMode,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub struct TurnInput {
    pub message_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    Text(String),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    Deny,
    Cancel,
}

/// Wire bytes are kept out of debug output. Dispatch persistence can record the exact frame.
pub struct WireMessage(Vec<u8>);

impl WireMessage {
    pub fn json(value: &Value) -> serde_json::Result<Self> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Default)]
pub struct AdapterUpdate {
    pub outbound: Vec<WireMessage>,
    pub events: Vec<ProviderEvent>,
}

pub struct Interrupt {
    pub outbound: Vec<WireMessage>,
    /// The runtime can terminate the supervised process when this deadline elapses.
    pub grace: Duration,
}

#[derive(Debug, Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderLocation {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    pub parent_item_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEvent {
    pub location: ProviderLocation,
    pub data: ProviderEventData,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedModel {
    pub id: String,
    pub canonical_id: Option<String>,
    pub name: String,
    pub input_modalities: Option<Vec<String>>,
    pub output_modalities: Option<Vec<String>>,
    pub effort_levels: Option<Vec<String>>,
    pub default_effort: Option<String>,
    pub selectable: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub current: Option<String>,
    pub choices: Vec<SessionChoice>,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionChoice {
    pub value: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEventData {
    DispatchSubmitted {
        dispatch_id: String,
    },
    ConfigurationReported {
        revision: Option<String>,
        #[serde(default)]
        codex_instructions: Option<configuration::CodexInstructionSettings>,
    },
    SkillsReported {
        catalog: skills::SkillCatalog,
    },
    CapabilityAssessmentUpdated {
        assessment: Box<capabilities::CapabilityAssessment>,
    },
    Handshake {
        protocol: String,
        capabilities: Vec<Capability>,
    },
    Authentication {
        authenticated: Option<bool>,
    },
    SessionReady {
        model: Option<String>,
        effort: Option<String>,
    },
    ModelObserved {
        model: Option<String>,
        effort: Option<String>,
    },
    ModelInventory {
        models: Vec<ReportedModel>,
    },
    SessionOptions {
        options: Vec<SessionOption>,
    },
    PromptCapabilities {
        image: Option<bool>,
        audio: Option<bool>,
        embedded_context: Option<bool>,
    },
    AvailableCommands {
        names: Vec<String>,
    },
    TurnStarted,
    AssistantDelta {
        text: String,
    },
    AssistantMessage {
        text: String,
    },
    Reasoning {
        text: String,
        summary: bool,
    },
    Usage {
        input: Option<u64>,
        output: Option<u64>,
        cached_input: Option<u64>,
    },
    ContextWindowUsage {
        used: u64,
        size: u64,
    },
    HistoryReported {
        turns: Option<u64>,
        item_kind: Option<String>,
    },
    ContextRead {
        path: String,
        tool: String,
    },
    Tool {
        name: String,
        phase: String,
    },
    MediaGeneration {
        media_type: String,
        tool: String,
        producing_model: Option<String>,
        status: Option<String>,
    },
    ChildActivity {
        child_id: Option<String>,
        status: Option<String>,
    },
    Approval {
        request_id: RequestId,
        operation: String,
        description: Option<String>,
    },
    Question {
        request_id: RequestId,
        form: Value,
    },
    Compacted,
    GoalObserved {
        observation: goal::NativeGoalObservation,
    },
    Suggestion {
        text: String,
    },
    Diagnostic {
        message: String,
    },
    Error {
        message: String,
    },
    TurnFinished {
        outcome: TurnOutcome,
    },
}

/// A provider describes its executable and probes. Each launch owns its protocol state.
pub trait CliAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn transport(&self) -> Transport;

    fn discover(&self, configured: Option<&Path>) -> io::Result<PathBuf> {
        discover_executable(self.provider(), configured, env::var_os("PATH"))
    }

    fn version_arguments(&self) -> Vec<OsString> {
        vec!["--version".into()]
    }

    fn authentication_probe(&self) -> AuthProbe;
    fn decode_authentication(&self, status: TerminalOutcome, output: &str) -> Authentication;
    fn create_session(&self, request: SessionRequest) -> anyhow::Result<Box<dyn CliSession>>;
}

/// Methods advance one provider protocol per process. The runtime owns I/O and persistence.
pub trait CliSession: Send {
    fn arguments(&self) -> Vec<OsString>;
    fn start(&mut self) -> anyhow::Result<AdapterUpdate>;
    fn query_effective_configuration(&mut self) -> anyhow::Result<Option<WireMessage>> {
        Ok(None)
    }
    fn query_skill_inventory(&mut self) -> anyhow::Result<Option<WireMessage>> {
        Ok(None)
    }
    fn send(&mut self, input: TurnInput) -> anyhow::Result<AdapterUpdate>;
    fn receive(&mut self, frame: OutputFrame) -> anyhow::Result<AdapterUpdate>;
    fn respond_approval(
        &mut self,
        id: &RequestId,
        decision: ApprovalDecision,
    ) -> anyhow::Result<AdapterUpdate>;
    fn respond_question(&mut self, id: &RequestId, answer: Value) -> anyhow::Result<AdapterUpdate>;
    fn interrupt(&mut self) -> anyhow::Result<Interrupt>;
    fn process_finished(&mut self, outcome: TerminalOutcome) -> AdapterUpdate;

    fn native_goal_support(&self) -> goal::NativeGoalSupport {
        goal::NativeGoalSupport::Managed {
            reason: goal::ManagedGoalReason::Unqualified,
        }
    }

    fn control_native_goal(
        &mut self,
        _command: goal::NativeGoalCommand,
    ) -> anyhow::Result<AdapterUpdate> {
        anyhow::bail!("the native goal lifecycle is unqualified for this session")
    }
}

fn discover_executable(
    provider: Provider,
    configured: Option<&Path>,
    path: Option<OsString>,
) -> io::Result<PathBuf> {
    if let Some(file) = configured {
        if !file.is_absolute() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "configured CLI path must be absolute",
            ));
        }
        return validate_executable(file);
    }
    let path = path.unwrap_or_default();
    env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(provider.executable()))
        .find_map(|candidate| validate_executable(&candidate).ok())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::NotFound,
                format!("{} is not installed on PATH", provider.executable()),
            )
        })
}

fn validate_executable(file: &Path) -> io::Result<PathBuf> {
    if !file
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "CLI must be a Windows .exe executable",
        ));
    }
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "CLI path must name a file",
        ));
    }
    dunce::canonicalize(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_uses_only_absolute_executables_and_honors_explicit_path() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let executable = directory.path().join(Provider::Claude.executable());
        std::fs::write(&executable, [])?;
        let path = env::join_paths([Path::new("relative"), directory.path()])?;
        assert_eq!(
            discover_executable(Provider::Claude, None, Some(path.clone()))?,
            dunce::canonicalize(executable)?
        );
        assert_eq!(
            discover_executable(Provider::Claude, Some(Path::new("claude.exe")), Some(path))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        assert!(discover_executable(Provider::Codex, None, None).is_err());
        Ok(())
    }

    #[test]
    fn json_messages_frame_multiline_input_without_changing_it() -> anyhow::Result<()> {
        let value = serde_json::json!({"text":"one\ntwo ☕"});
        let frame = WireMessage::json(&value)?;
        assert_eq!(
            frame
                .as_bytes()
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count(),
            1
        );
        assert_eq!(serde_json::from_slice::<Value>(frame.as_bytes())?, value);
        Ok(())
    }
}
pub mod antigravity;
pub mod capabilities;
pub mod claude;
pub mod codex;
pub mod configuration;
pub mod goal;
pub mod plugins;
pub mod profiles;
mod rpc;
pub mod runtime;
pub mod skills;
