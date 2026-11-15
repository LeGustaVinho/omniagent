use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    customizations::{CustomizationKind, LaunchReview, ReviewState},
    mcp_bridge::{
        BridgeLaunch, GET_JOB_STATUS, INSPECT_OMNIAGENT, INSPECT_SCOPE, REPORT_GOAL_PROGRESS,
        REQUEST_PERMISSION, SERVER_NAME, SUBMIT_JOB, WAIT_FOR_JOBS,
    },
    repository::CheckoutLayout,
    runner::Runner,
};

use super::{
    Provider,
    configuration::{self, InstructionSource},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedLaunchEvidence {
    pub provider: Provider,
    pub strategy: String,
    pub approved: BTreeMap<String, String>,
    pub excluded: BTreeMap<String, String>,
    pub instruction_sources: Vec<InstructionSource>,
    pub inputs: Vec<LaunchInput>,
    #[serde(default)]
    pub(crate) mcp_sources: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) non_mcp_approvals: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInput {
    pub name: String,
    pub contents: String,
}

pub(crate) struct ScopedLaunch {
    pub arguments: Vec<OsString>,
    pub evidence: ScopedLaunchEvidence,
    directory: Option<tempfile::TempDir>,
    pub protocol_instructions: Option<String>,
}

impl ScopedLaunch {
    pub(crate) fn enable_codex_native_goals(&mut self) -> Result<()> {
        ensure!(
            self.evidence.provider == Provider::Codex,
            "The CLI cannot supervise native goals"
        );
        let disabled = "features.goals=false";
        let enabled = "features.goals=true";
        let argument = self
            .arguments
            .iter_mut()
            .find(|argument| argument.as_os_str() == disabled)
            .context("Missing scoped Codex goal setting")?;
        let evidence = self
            .evidence
            .inputs
            .iter_mut()
            .find(|input| input.contents == disabled)
            .context("Missing scoped Codex goal launch evidence")?;
        *argument = OsString::from(enabled);
        evidence.contents = enabled.into();
        Ok(())
    }

    pub(crate) fn apply_codex_subagent_limit(&mut self, allowance: u32) -> Result<()> {
        ensure!(
            self.evidence.provider == Provider::Codex,
            "The CLI has no verified subagent control"
        );
        let enabled = allowance > 0;
        // V2 includes the primary thread and can be selected by the model.
        let v2_cap = allowance + 1;
        let settings = [
            format!("agents.enabled={enabled}"),
            format!("features.multi_agent_v2.max_concurrent_threads_per_session={v2_cap}"),
            if enabled {
                format!("agents.max_concurrent_threads_per_session={allowance}")
            } else {
                "features.multi_agent_v2.enabled=false".into()
            },
        ];
        for contents in settings {
            self.arguments
                .extend([OsString::from("-c"), OsString::from(&contents)]);
            self.evidence.inputs.push(LaunchInput {
                name: "--config".into(),
                contents,
            });
        }
        Ok(())
    }

    pub(crate) fn prepare(
        provider: Provider,
        version: &str,
        layout: &CheckoutLayout,
        review: &LaunchReview,
        arguments: Vec<OsString>,
        bridge: Option<&BridgeLaunch>,
    ) -> Result<Self> {
        let mut launch = Self {
            arguments,
            evidence: ScopedLaunchEvidence {
                provider,
                strategy: String::new(),
                approved: BTreeMap::new(),
                excluded: BTreeMap::new(),
                instruction_sources: Vec::new(),
                inputs: Vec::new(),
                mcp_sources: BTreeMap::new(),
                non_mcp_approvals: BTreeMap::new(),
            },
            directory: None,
            protocol_instructions: None,
        };
        for entry in &review.inventory.entries {
            let reason = match entry.review {
                ReviewState::Pending | ReviewState::Changed => {
                    "Current fingerprint requires approval"
                }
                ReviewState::Disabled => "Disabled in repository configuration",
                ReviewState::Unresolved => "Executable target is unresolved",
                ReviewState::Approved => continue,
            };
            launch
                .evidence
                .excluded
                .insert(entry.id.clone(), reason.into());
        }
        match provider {
            Provider::Claude => launch.claude(version, layout, review, bridge)?,
            Provider::Codex => launch.codex(layout, review, bridge)?,
            Provider::Antigravity => {
                launch.antigravity(review)?;
                if let Some(bridge) = bridge {
                    launch.evidence.inputs.push(LaunchInput {
                        name: "mcpServers".into(),
                        contents: bridge.acp_servers().to_string(),
                    });
                }
            }
        }
        Ok(launch)
    }

    pub(crate) fn identify_mcp_executables(
        &mut self,
        runner: &dyn Runner,
        review: &LaunchReview,
        bridge: Option<&BridgeLaunch>,
    ) -> Result<()> {
        for (entry, declaration) in &review.approved {
            if entry.kind != CustomizationKind::McpServer {
                continue;
            }
            let id = configuration::mcp_identity(&declaration.name);
            let Some(source) = self.evidence.mcp_sources.get_mut(&id) else {
                continue;
            };
            let overrides_search_path = declaration.body["env"]
                .as_object()
                .is_some_and(|env| env.keys().any(|key| key.eq_ignore_ascii_case("PATH")));
            let launcher = entry.target.as_deref().map(Path::new).filter(|path| {
                !overrides_search_path
                    && path.is_absolute()
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
            });
            if let Some(revision) = launcher.and_then(|path| runner.executable_revision(path)) {
                *source = super::capabilities::revision(&(&*source, revision))?;
            } else {
                self.evidence.mcp_sources.remove(&id);
            }
        }
        if let Some(bridge) = bridge
            && let Some(revision) = runner.executable_revision(Path::new(&bridge.command))
        {
            self.evidence
                .mcp_sources
                .insert(configuration::mcp_identity(SERVER_NAME), revision);
        }
        Ok(())
    }

    fn claude(
        &mut self,
        version: &str,
        layout: &CheckoutLayout,
        review: &LaunchReview,
        bridge: Option<&BridgeLaunch>,
    ) -> Result<()> {
        ensure!(
            at_least(version, (2, 1, 257)),
            "This Claude Code build cannot enforce the scoped restricted launch"
        );
        self.evidence.strategy = "claude_restricted".into();
        let mut hooks: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        let mut mcp = Map::new();
        if let Some(bridge) = bridge {
            mcp.insert(SERVER_NAME.into(), serde_json::to_value(bridge)?);
            self.record_argument(
                "--allowedTools",
                [
                    INSPECT_SCOPE,
                    INSPECT_OMNIAGENT,
                    SUBMIT_JOB,
                    GET_JOB_STATUS,
                    WAIT_FOR_JOBS,
                    REPORT_GOAL_PROGRESS,
                ]
                .map(|tool| format!("mcp__{SERVER_NAME}__{tool}"))
                .join(","),
            );
            if bridge.permission_prompt {
                self.record_argument(
                    "--permission-prompt-tool",
                    format!("mcp__{SERVER_NAME}__{REQUEST_PERMISSION}"),
                );
            }
        }
        for (entry, declaration) in &review.approved {
            match entry.kind {
                CustomizationKind::Hook => {
                    let event = declaration
                        .event
                        .as_deref()
                        .context("approved Claude hook has no trigger")?;
                    hooks.entry(event).or_default().push(json!({
                        "matcher": declaration.body["matcher"], "hooks": [declaration.body["handler"]],
                    }));
                    self.evidence
                        .non_mcp_approvals
                        .insert(entry.id.clone(), entry.fingerprint.clone());
                }
                CustomizationKind::McpServer => {
                    if bridge.is_some() && declaration.name == SERVER_NAME {
                        self.evidence.excluded.insert(
                            entry.id.clone(),
                            "This MCP name is reserved for the OmniAgent run bridge".into(),
                        );
                        continue;
                    }
                    ensure!(
                        mcp.insert(declaration.name.clone(), declaration.body.clone())
                            .is_none(),
                        "Multiple approved Claude declarations use the same MCP server name"
                    );
                    self.evidence.mcp_sources.insert(
                        configuration::mcp_identity(&declaration.name),
                        entry.fingerprint.clone(),
                    );
                }
                _ => {
                    self.evidence.excluded.insert(
                        entry.id.clone(),
                        "Restricted Claude launch supports projected hooks and MCP servers".into(),
                    );
                    continue;
                }
            }
            self.evidence
                .approved
                .insert(entry.id.clone(), entry.fingerprint.clone());
        }
        let (instruction_text, sources) =
            configuration::claude_workspace_projection(layout, version)?;
        self.evidence.instruction_sources = sources;
        let settings = self.write(
            "settings.json",
            serde_json::to_string(&json!({
                "hooks": hooks, "autoMemoryEnabled": false,
            }))?,
        )?;
        let mcp = self.write(
            "mcp.json",
            serde_json::to_string(&json!({"mcpServers": mcp}))?,
        )?;
        self.arguments.retain(|argument| argument != "--safe-mode");
        let existing_mcp = self
            .arguments
            .iter()
            .position(|argument| argument == "--mcp-config")
            .context("Claude argument contract omitted --mcp-config")?;
        *self
            .arguments
            .get_mut(existing_mcp + 1)
            .context("Claude argument contract omitted the MCP configuration value")? =
            mcp.into_os_string();
        self.arguments.extend([
            OsString::from("--restricted"),
            OsString::from("--system-prompt-snapshot"),
            OsString::from("off"),
            OsString::from("--settings"),
            settings.into_os_string(),
        ]);
        if !instruction_text.is_empty() {
            let path = self.write("instructions.md", instruction_text)?;
            self.arguments.extend([
                OsString::from("--append-system-prompt-file"),
                path.into_os_string(),
            ]);
        }
        Ok(())
    }

    fn record_argument(&mut self, name: &str, contents: String) {
        self.arguments.extend([name.into(), (&contents).into()]);
        self.evidence.inputs.push(LaunchInput {
            name: name.into(),
            contents,
        });
    }

    fn codex(
        &mut self,
        layout: &CheckoutLayout,
        review: &LaunchReview,
        bridge: Option<&BridgeLaunch>,
    ) -> Result<()> {
        self.evidence.strategy = "codex_untrusted_project".into();
        let mut projects = toml::Table::new();
        for directory in layout
            .cwd
            .ancestors()
            .take_while(|directory| directory.starts_with(&layout.root))
        {
            projects.insert(
                directory.to_string_lossy().into_owned(),
                toml::Value::Table(
                    [(
                        "trust_level".into(),
                        toml::Value::String("untrusted".into()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            );
        }
        let mut servers = toml::Table::new();
        if let Some(bridge) = bridge {
            servers.insert(
                SERVER_NAME.into(),
                toml::Value::try_from(json!({
                    "command": bridge.command,
                    "args": bridge.args,
                    "tools": {
                        INSPECT_SCOPE: {"approval_mode": "approve"},
                        INSPECT_OMNIAGENT: {"approval_mode": "approve"},
                        SUBMIT_JOB: {"approval_mode": "approve"},
                        GET_JOB_STATUS: {"approval_mode": "approve"},
                        WAIT_FOR_JOBS: {"approval_mode": "approve"},
                        REPORT_GOAL_PROGRESS: {"approval_mode": "approve"},
                    },
                }))?,
            );
        }
        for (entry, declaration) in &review.approved {
            if entry.kind == CustomizationKind::McpServer {
                if bridge.is_some() && declaration.name == SERVER_NAME {
                    self.evidence.excluded.insert(
                        entry.id.clone(),
                        "This MCP name is reserved for the OmniAgent run bridge".into(),
                    );
                    continue;
                }
                let mut server = toml::Value::try_from(&declaration.body)
                    .context("could not encode the approved MCP server")?;
                if declaration.body["command"].is_string() {
                    let server = server
                        .as_table_mut()
                        .context("approved Codex MCP declaration is not a table")?;
                    server.entry("cwd").or_insert_with(|| {
                        toml::Value::String(layout.cwd.to_string_lossy().into_owned())
                    });
                }
                ensure!(
                    servers.insert(declaration.name.clone(), server).is_none(),
                    "Multiple approved Codex declarations use the same MCP server name"
                );
                self.evidence.mcp_sources.insert(
                    configuration::mcp_identity(&declaration.name),
                    entry.fingerprint.clone(),
                );
                self.evidence
                    .approved
                    .insert(entry.id.clone(), entry.fingerprint.clone());
            } else {
                self.evidence.excluded.insert(entry.id.clone(),
                    "Scoped Codex launch preserves native hook trust and supports approved MCP servers".into());
            }
        }
        let flags = [
            format!("projects={}", toml::Value::Table(projects)),
            format!("mcp_servers={}", toml::Value::Table(servers)),
            "project_doc_max_bytes=0".into(),
            // A restored active goal can start a turn before the managed scheduler dispatches.
            "features.goals=false".into(),
        ];
        let mut arguments: Vec<_> = flags
            .iter()
            .flat_map(|flag| [OsString::from("-c"), OsString::from(flag)])
            .collect();
        arguments.append(&mut self.arguments);
        self.arguments = arguments;
        self.evidence.inputs = flags
            .into_iter()
            .map(|contents| LaunchInput {
                name: "--config".into(),
                contents,
            })
            .collect();
        let (instructions, sources) = configuration::codex_workspace_projection(layout)?;
        self.evidence.instruction_sources = sources;
        if !instructions.is_empty() {
            self.evidence.inputs.push(LaunchInput {
                name: "developerInstructions".into(),
                contents: instructions.clone(),
            });
            self.protocol_instructions = Some(instructions);
        }
        Ok(())
    }

    fn antigravity(&mut self, review: &LaunchReview) -> Result<()> {
        let executable_scope = review
            .inventory
            .entries
            .iter()
            .any(|entry| entry.review != ReviewState::Disabled)
            || review.inventory.unresolved_sources.iter().any(|path| {
                Path::new(path).components().any(|component| {
                    matches!(
                        component.as_os_str().to_str(),
                        Some(".agents" | ".agent" | "_agents" | "_agent" | ".gemini")
                    )
                })
            });
        if executable_scope {
            bail!(
                "Antigravity ACP cannot enforce the repository's individual executable approvals with this launch profile"
            );
        }
        self.evidence.strategy = "acp_workspace_without_executable_customizations".into();
        Ok(())
    }

    fn write(&mut self, name: &str, contents: String) -> Result<PathBuf> {
        if self.directory.is_none() {
            self.directory = Some(
                tempfile::Builder::new()
                    .prefix("omniagent-launch-")
                    .tempdir()?,
            );
        }
        let path = self
            .directory
            .as_ref()
            .expect("launch staging")
            .path()
            .join(name);
        fs::write(&path, contents.as_bytes())?;
        self.evidence.inputs.push(LaunchInput {
            name: name.into(),
            contents,
        });
        Ok(path)
    }
}

fn at_least(version: &str, minimum: (u32, u32, u32)) -> bool {
    let mut parts = version
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.');
    let version = (|| {
        Some((
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ))
    })();
    version.is_some_and(|version| version >= minimum)
}
