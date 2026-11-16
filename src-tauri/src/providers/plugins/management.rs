use std::{
    env,
    fs::{File, OpenOptions},
    io,
    mem::size_of,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    ptr,
};

use anyhow::{Context, bail, ensure};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx,
};

use crate::providers::{configuration, profiles::CommandOutput};

use super::*;

#[cfg(test)]
mod tests;

static MANAGE_PROFILE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const CATALOG_LIMIT: usize = 16 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(25);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const USER_SCOPE: &str = "user";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeUserPluginAction {
    Install,
    Enable,
    Disable,
}

impl ClaudeUserPluginAction {
    fn subcommand(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }

    fn resulting_enabled(self) -> bool {
        !matches!(self, Self::Disable)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClaudeUserPluginRequest {
    pub action: ClaudeUserPluginAction,
    pub plugin_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUserPluginPreview {
    pub id: String,
    pub action: ClaudeUserPluginAction,
    pub plugin_id: String,
    pub provider: Provider,
    pub execution_scope: String,
    pub configuration_root: PathBuf,
    pub executable: PathBuf,
    pub cli_version: String,
    pub persists: bool,
    pub version: Option<String>,
    pub marketplace: String,
    pub source: Option<String>,
    pub inventory_before: PluginInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ClaudeUserPluginOutcome {
    Applied {
        inventory_after: PluginInventory,
    },
    Stale {
        reason: String,
        inventory_after: Option<PluginInventory>,
    },
    NeedsConfirmation {
        reason: String,
        command_fingerprint: Option<String>,
        inventory_after: Option<PluginInventory>,
    },
    Failed {
        reason: String,
        inventory_after: Option<PluginInventory>,
    },
    Unconfirmed {
        reason: String,
        inventory_after: Option<PluginInventory>,
    },
}

impl ClaudeUserPluginOutcome {
    fn inventory_after(&self) -> Option<&PluginInventory> {
        match self {
            Self::Applied { inventory_after } => Some(inventory_after),
            Self::Stale {
                inventory_after, ..
            }
            | Self::NeedsConfirmation {
                inventory_after, ..
            }
            | Self::Failed {
                inventory_after, ..
            }
            | Self::Unconfirmed {
                inventory_after, ..
            } => inventory_after.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct EffectiveProfile {
    executable: PathBuf,
    executable_revision: String,
    cli_version: String,
    configuration_root: PathBuf,
    configuration_identity: String,
}

struct LocalClaude {
    profile: EffectiveProfile,
    cwd: tempfile::TempDir,
    _directory: File,
    _executable: File,
}

impl LocalClaude {
    fn qualify(configured: ConfiguredExecutable, action: ClaudeUserPluginAction) -> Result<Self> {
        ensure!(
            configured.provider == Provider::Claude,
            "Plugin management requires Claude Code"
        );
        let executable = dunce::canonicalize(
            Provider::Claude
                .adapter()
                .discover(configured.executable.as_deref().map(Path::new))?,
        )?;
        let locked_executable = configuration::open_locked_local_file(&executable)?;
        let revision = DirectWindowsRunner
            .executable_revision(&executable)
            .context("Could not verify the Claude executable fingerprint")?;
        let root = config_root()?;
        let directory = open_config_root(&root)?;
        let identity = directory_identity(&directory)?;
        let cwd = tempfile::Builder::new()
            .prefix("omniagent-claude-plugins-")
            .tempdir()?;
        let run = |args: &[&str]| run_claude(&executable, cwd.path(), args, READ_TIMEOUT);
        let version = run(&["--version"])?;
        let cli_version = published_version(Provider::Claude, &version.stdout)
            .or_else(|| published_version(Provider::Claude, &version.stderr))
            .filter(|_| version.status == TerminalOutcome::Exited(0))
            .context("Could not verify the Claude CLI version")?;
        let help = run(&["plugin", action.subcommand(), "--help"])?;
        ensure!(
            help.status == TerminalOutcome::Exited(0)
                && help.stdout.contains("--scope")
                && help.stdout.contains("--json"),
            "This Claude CLI build does not support JSON plugin actions with an explicit scope"
        );
        if action == ClaudeUserPluginAction::Install {
            ensure!(
                help.stdout.contains("--accept-command") && help.stdout.contains("--yes"),
                "This Claude CLI build does not expose bounded confirmation for marketplace commands"
            );
        }
        Ok(Self {
            profile: EffectiveProfile {
                executable,
                executable_revision: revision,
                cli_version,
                configuration_root: root,
                configuration_identity: identity,
            },
            cwd,
            _directory: directory,
            _executable: locked_executable,
        })
    }

    fn verify_root(&self) -> Result<()> {
        let root = config_root()?;
        ensure!(
            root == self.profile.configuration_root
                && directory_identity(&open_config_root(&root)?)?
                    == self.profile.configuration_identity,
            "The active Claude configuration root changed"
        );
        Ok(())
    }

    fn probe(&self, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
        self.verify_root()?;
        run_claude(&self.profile.executable, self.cwd.path(), args, timeout)
    }

    fn installed(&self) -> Result<PluginReport> {
        let response = self.probe(&["plugin", "list", "--json"], READ_TIMEOUT)?;
        let (status, plugins) = decode_plugins(Provider::Claude, &response.stdout);
        ensure!(
            response.status == TerminalOutcome::Exited(0)
                && status == PluginInventoryStatus::Reported,
            "Claude Code did not report a complete installed plugin inventory"
        );
        Ok(PluginReport {
            provider: Provider::Claude,
            executable: self.profile.executable.to_str().map(str::to_owned),
            version: Some(self.profile.cli_version.clone()),
            status,
            plugins,
            load_telemetry_reported: false,
        })
    }

    fn catalog_entry(&self, id: &str) -> Result<CatalogEntry> {
        let response = self.probe(&["plugin", "list", "--available", "--json"], READ_TIMEOUT)?;
        ensure!(
            response.status == TerminalOutcome::Exited(0),
            "Claude could not list available plugins"
        );
        let catalog: Value = serde_json::from_str(&response.stdout)
            .context("Claude returned an invalid plugin catalog")?;
        catalog_entry(&catalog, id)
    }

    fn config_revision(&self) -> Result<[Option<String>; 2]> {
        let root = &self.profile.configuration_root;
        let digest = |path| match configuration::read_local_file(&root.join(path)) {
            Ok(bytes) => Ok(Some(format!("{:x}", Sha256::digest(bytes)))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => bail!("Cannot verify the local Claude plugin configuration"),
        };
        Ok([
            digest("settings.json")?,
            digest("plugins/known_marketplaces.json")?,
        ])
    }
}

fn run_claude(path: &Path, cwd: &Path, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
    command_probe_with_limits(
        |request| DirectWindowsRunner.launch(request),
        path,
        cwd,
        args.iter().map(std::ffi::OsString::from).collect(),
        timeout,
        CATALOG_LIMIT,
    )
}

fn config_root() -> Result<PathBuf> {
    let path = if let Some(configured) =
        env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty())
    {
        PathBuf::from(configured)
    } else {
        PathBuf::from(env::var_os("USERPROFILE").context("Cannot locate the Claude user profile")?)
            .join(".claude")
    };
    ensure!(
        path.is_absolute(),
        "The Claude user configuration path must be absolute"
    );
    let resolved = dunce::canonicalize(&path)
        .context("Cannot locate the existing Claude configuration directory")?;
    ensure!(
        dunce::simplified(&path) == resolved,
        "The Claude user configuration path must be canonical"
    );
    Ok(resolved)
}

fn open_config_root(path: &Path) -> Result<File> {
    let directory = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "The active Claude configuration must be an ordinary directory"
    );
    Ok(directory)
}

fn directory_identity(directory: &File) -> Result<String> {
    let mut identity = FILE_ID_INFO::default();
    let read = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            FileIdInfo,
            ptr::from_mut(&mut identity).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(format!(
        "{:016x}:{:032x}",
        identity.VolumeSerialNumber,
        u128::from_be_bytes(identity.FileId.Identifier)
    ))
}

fn plugin_selector(id: &str) -> Result<(&str, &str)> {
    let part = |value: &str| {
        value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let (name, marketplace) = id
        .split_once('@')
        .context("Choose a plugin with an explicit marketplace")?;
    ensure!(
        part(name) && part(marketplace),
        "The plugin marketplace selector is invalid"
    );
    ensure!(
        !matches!(marketplace, "npm" | "synced" | "skills-dir"),
        "This plugin source is outside local marketplace management"
    );
    Ok((name, marketplace))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct CatalogEntry {
    revision: String,
    version: Option<String>,
    source: Option<String>,
    requires_confirmation: bool,
}

fn catalog_entry(catalog: &Value, id: &str) -> Result<CatalogEntry> {
    let mut matching = catalog["available"]
        .as_array()
        .context("Claude did not report the available plugin catalog")?
        .iter()
        .filter(|entry| entry["pluginId"] == id);
    let mut entry = matching
        .next()
        .context("The exact plugin is unavailable in the configured marketplaces")?
        .clone();
    ensure!(
        matching.next().is_none(),
        "The marketplace returned multiple entries for this plugin"
    );
    let (_, marketplace) = plugin_selector(id)?;
    ensure!(
        entry["marketplaceName"] == marketplace,
        "The plugin marketplace identity did not match"
    );
    entry.sort_all_objects();
    let source = entry
        .get("source")
        .filter(|value| !value.is_null())
        .context("Claude did not identify the source for this installation")?;
    let requires_confirmation = source["source"] == "command"
        || source["headersHelper"].is_object()
        || source["headersHelper"].is_string();
    let source = if let Some(relative) = source.as_str() {
        let relative_path = Path::new(relative);
        ensure!(
            relative.starts_with("./")
                && !relative_path.is_absolute()
                && !relative_path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)),
            "The marketplace reported an invalid relative plugin source"
        );
        Some(source_location(relative))
    } else {
        let kind = source["source"]
            .as_str()
            .context("The marketplace did not identify a recognized plugin source")?;
        ensure!(
            matches!(
                kind,
                "github" | "git-subdir" | "url" | "archive" | "command"
            ),
            "This plugin source cannot be installed through the supported management action"
        );
        let location = ["repo", "url", "path"]
            .into_iter()
            .find_map(|key| source[key].as_str())
            .map(source_location);
        Some(location.map_or_else(
            || kind.to_owned(),
            |location| format!("{kind} · {location}"),
        ))
    };
    Ok(CatalogEntry {
        revision: format!("{:x}", Sha256::digest(serde_json::to_vec(&entry)?)),
        version: text(&entry["version"]),
        source,
        requires_confirmation,
    })
}

fn installed_target<'a>(report: &'a PluginReport, id: &str) -> Result<Option<&'a ReportedPlugin>> {
    let mut matching = report.plugins.iter().filter(|entry| entry.id == id);
    let Some(plugin) = matching.next() else {
        return Ok(None);
    };
    ensure!(
        matching.next().is_none() && plugin.scope.as_deref() == Some(USER_SCOPE),
        "The plugin is ambiguous or installed outside the Claude user scope"
    );
    ensure!(
        plugin.enabled.is_some(),
        "Claude did not report the plugin's activation state"
    );
    Ok(Some(plugin))
}

#[derive(Deserialize, Serialize)]
struct SealedPreview {
    preview: ClaudeUserPluginPreview,
    target: EffectiveProfile,
    config_revision: [Option<String>; 2],
    installed: Option<ReportedPlugin>,
    catalog: Option<CatalogEntry>,
}

struct Evaluation {
    profile: EffectiveProfile,
    report: PluginReport,
    config_revision: [Option<String>; 2],
    installed: Option<ReportedPlugin>,
    catalog: Option<CatalogEntry>,
}

fn evaluate(
    cli: &LocalClaude,
    request: &ClaudeUserPluginRequest,
    report: PluginReport,
) -> Result<Evaluation> {
    let installed = installed_target(&report, &request.plugin_id)?.cloned();
    let catalog = match request.action {
        ClaudeUserPluginAction::Install => {
            ensure!(
                installed.is_none(),
                "The plugin is already installed in this Claude profile"
            );
            Some(cli.catalog_entry(&request.plugin_id)?)
        }
        ClaudeUserPluginAction::Enable | ClaudeUserPluginAction::Disable => {
            let plugin = installed
                .as_ref()
                .context("The plugin is unavailable in the Claude user scope")?;
            ensure!(
                plugin.enabled != Some(request.action.resulting_enabled()),
                "The plugin is already in the requested state"
            );
            None
        }
    };
    Ok(Evaluation {
        profile: cli.profile.clone(),
        report,
        config_revision: cli.config_revision()?,
        installed,
        catalog,
    })
}

fn protected<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn decode<T: DeserializeOwned>(value: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(value)?)?)
}

impl Store {
    pub async fn preview_claude_user_plugin_action(
        &self,
        request: ClaudeUserPluginRequest,
    ) -> Result<ClaudeUserPluginPreview> {
        plugin_selector(&request.plugin_id)?;
        let _guard = MANAGE_PROFILE.lock().await;
        let configured = self.configured_executable(Provider::Claude).await?;
        let (request, evaluation) = tokio::task::spawn_blocking(move || {
            let cli = LocalClaude::qualify(configured, request.action)?;
            let evaluation = evaluate(&cli, &request, cli.installed()?)?;
            Ok::<_, anyhow::Error>((request, evaluation))
        })
        .await??;
        let inventory_before = self.save_plugin_inventory(evaluation.report).await?;
        let (_, marketplace) = plugin_selector(&request.plugin_id)?;
        let preview = ClaudeUserPluginPreview {
            id: Uuid::new_v4().to_string(),
            action: request.action,
            plugin_id: request.plugin_id.clone(),
            provider: Provider::Claude,
            execution_scope: USER_SCOPE.into(),
            configuration_root: evaluation.profile.configuration_root.clone(),
            executable: evaluation.profile.executable.clone(),
            cli_version: evaluation.profile.cli_version.clone(),
            persists: true,
            marketplace: marketplace.into(),
            version: evaluation
                .catalog
                .as_ref()
                .and_then(|entry| entry.version.clone())
                .or_else(|| {
                    evaluation
                        .installed
                        .as_ref()
                        .and_then(|entry| entry.version.clone())
                }),
            source: evaluation
                .catalog
                .as_ref()
                .and_then(|entry| entry.source.clone())
                .or_else(|| {
                    evaluation
                        .installed
                        .as_ref()
                        .and_then(|entry| entry.install_path.clone())
                }),
            inventory_before,
        };
        let sealed = SealedPreview {
            preview: preview.clone(),
            target: evaluation.profile,
            config_revision: evaluation.config_revision,
            installed: evaluation.installed,
            catalog: evaluation.catalog,
        };
        self.execute(move |db| {
            db.execute(
                "INSERT INTO claude_user_plugin_actions(id, state, before_inventory_id, preview)
                 VALUES (?1, 'ready', ?2, ?3)",
                params![
                    sealed.preview.id,
                    sealed.preview.inventory_before.id,
                    protected(&sealed)?
                ],
            )?;
            Ok(())
        })
        .await?;
        Ok(preview)
    }

    pub async fn apply_claude_user_plugin_action(
        &self,
        preview_id: String,
    ) -> Result<ClaudeUserPluginOutcome> {
        let store = self.clone();
        tokio::spawn(async move {
            let _guard = MANAGE_PROFILE.lock().await;
            apply(&store, preview_id).await
        })
        .await?
    }
}

enum Claim {
    Ready(SealedPreview),
    Recorded(ClaudeUserPluginOutcome),
}

async fn apply(store: &Store, id: String) -> Result<ClaudeUserPluginOutcome> {
    let claim = store.execute({
        let id = id.clone();
        move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let (preview, outcome): (Vec<u8>, Option<Vec<u8>>) = tx.query_row(
                "SELECT preview, outcome FROM claude_user_plugin_actions WHERE id = ?1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.context("The Claude plugin preview is unavailable")?;
            if let Some(previous) = outcome {
                return Ok(Claim::Recorded(decode(&previous)?));
            }
            let prepared = ClaudeUserPluginOutcome::Unconfirmed {
                reason: "This operation may have been interrupted. Refresh the CLI inventory before reviewing a new operation.".into(),
                inventory_after: None,
            };
            ensure!(tx.execute(
                "UPDATE claude_user_plugin_actions SET state = 'applying', outcome = ?2 WHERE id = ?1 AND state = 'ready'",
                params![id, protected(&prepared)?],
            )? == 1, "The Claude plugin preview was already consumed");
            let sealed = decode(&preview)?;
            tx.commit()?;
            Ok(Claim::Ready(sealed))
        }
    }).await?;
    let sealed = match claim {
        Claim::Ready(sealed) => sealed,
        Claim::Recorded(outcome) => return Ok(outcome),
    };
    let configured = store.configured_executable(Provider::Claude).await?;
    let checked = tokio::task::spawn_blocking(move || apply_native(configured, sealed)).await?;
    let after = match checked.report {
        Some(report) => Some(store.save_plugin_inventory(report).await?),
        None => None,
    };
    let outcome = checked.result.with_inventory(after);
    store.execute({
        let outcome = outcome.clone();
        move |db| {
            ensure!(db.execute(
                "UPDATE claude_user_plugin_actions SET state = 'settled', outcome = ?2, after_inventory_id = ?3
                 WHERE id = ?1 AND state = 'applying'",
                params![id, protected(&outcome)?, outcome.inventory_after().map(|item| &item.id)],
            )? == 1, "The Claude plugin result could not be settled");
            Ok(())
        }
    }).await?;
    Ok(outcome)
}

enum NativeResult {
    Applied,
    Stale(String),
    NeedsConfirmation(Option<String>),
    Failed(String),
    Unconfirmed(String),
}

impl NativeResult {
    fn with_inventory(self, inventory_after: Option<PluginInventory>) -> ClaudeUserPluginOutcome {
        match self {
            Self::Applied => inventory_after
                .map(|inventory_after| ClaudeUserPluginOutcome::Applied { inventory_after })
                .unwrap_or_else(|| ClaudeUserPluginOutcome::Unconfirmed {
                    reason: "The refreshed inventory could not be retained.".into(),
                    inventory_after: None,
                }),
            Self::Stale(reason) => ClaudeUserPluginOutcome::Stale {
                reason,
                inventory_after,
            },
            Self::NeedsConfirmation(command_fingerprint) => {
                ClaudeUserPluginOutcome::NeedsConfirmation {
                    reason: "Claude requires separate confirmation for the marketplace's command."
                        .into(),
                    command_fingerprint,
                    inventory_after,
                }
            }
            Self::Failed(reason) => ClaudeUserPluginOutcome::Failed {
                reason,
                inventory_after,
            },
            Self::Unconfirmed(reason) => ClaudeUserPluginOutcome::Unconfirmed {
                reason,
                inventory_after,
            },
        }
    }
}

struct NativeOutcome {
    result: NativeResult,
    report: Option<PluginReport>,
}

fn apply_native(configured: ConfiguredExecutable, sealed: SealedPreview) -> NativeOutcome {
    match apply_to_fresh_profile(configured, sealed) {
        Ok(native) => native,
        Err(error) => NativeOutcome {
            result: NativeResult::Stale(diagnostics::preview(&error.to_string())),
            report: None,
        },
    }
}

fn apply_to_fresh_profile(
    configured: ConfiguredExecutable,
    sealed: SealedPreview,
) -> Result<NativeOutcome> {
    let request = ClaudeUserPluginRequest {
        action: sealed.preview.action,
        plugin_id: sealed.preview.plugin_id,
    };
    let cli = LocalClaude::qualify(configured, request.action)?;
    ensure!(
        cli.profile == sealed.target,
        "The configured Claude CLI build or user root changed after review"
    );
    let report = cli.installed()?;
    let current = match evaluate(&cli, &request, report.clone()) {
        Ok(current) => current,
        Err(error) => {
            return Ok(NativeOutcome {
                result: NativeResult::Stale(diagnostics::preview(&error.to_string())),
                report: Some(report),
            });
        }
    };
    if current.config_revision != sealed.config_revision
        || current.installed != sealed.installed
        || current.catalog != sealed.catalog
    {
        return Ok(NativeOutcome {
            result: NativeResult::Stale("The Claude plugin configuration or the selected installation changed after review.".into()),
            report: Some(report),
        });
    }
    if current
        .catalog
        .as_ref()
        .is_some_and(|entry| entry.requires_confirmation)
    {
        return Ok(NativeOutcome {
            result: NativeResult::NeedsConfirmation(None),
            report: Some(report),
        });
    }
    // Claude itself chooses the user configuration. This closed argument list
    // never supplies a shell, user config path, --yes or --accept-command.
    let response = cli.probe(
        &[
            "plugin",
            request.action.subcommand(),
            &request.plugin_id,
            "--scope",
            USER_SCOPE,
            "--json",
        ],
        if request.action == ClaudeUserPluginAction::Install {
            INSTALL_TIMEOUT
        } else {
            READ_TIMEOUT
        },
    );
    let report = cli
        .installed()
        .and_then(|report| {
            cli.verify_root()?;
            Ok(report)
        })
        .ok();
    let result = match response {
        Ok(response) => classify_result(response, &request, &current, report.as_ref()),
        Err(_) => NativeResult::Unconfirmed("Claude did not return a complete result. Check the refreshed CLI inventory before another operation.".into()),
    };
    Ok(NativeOutcome { result, report })
}

fn classify_result(
    response: CommandOutput,
    request: &ClaudeUserPluginRequest,
    current: &Evaluation,
    report: Option<&PluginReport>,
) -> NativeResult {
    let decoded = serde_json::from_str::<Value>(&response.stdout).ok();
    if request.action == ClaudeUserPluginAction::Install
        && let Some(shown) = decoded.as_ref().and_then(|value| value.get("shownCommand"))
        && !shown.is_null()
    {
        let hash = shown["sha256"]
            .as_str()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        return NativeResult::NeedsConfirmation(hash.map(str::to_owned));
    }
    let reached = report.is_some_and(|report| observed_result(report, request, current));
    if response.status == TerminalOutcome::Exited(0)
        && decoded
            .as_ref()
            .is_some_and(|response| native_success(response, request))
        && reached
    {
        return NativeResult::Applied;
    }
    if matches!(response.status, TerminalOutcome::Exited(code) if code != 0)
        && report.is_some()
        && !reached
    {
        return NativeResult::Failed(
            "Claude declined the requested plugin operation. Review the refreshed CLI inventory."
                .into(),
        );
    }
    NativeResult::Unconfirmed(
        "Claude did not confirm the requested state in the refreshed user plugin inventory.".into(),
    )
}

fn native_success(response: &Value, request: &ClaudeUserPluginRequest) -> bool {
    response["command"] == request.action.subcommand()
        && response["outcome"] == "ok"
        && response["pluginId"] == request.plugin_id
        && response["scope"] == USER_SCOPE
}

fn observed_result(
    report: &PluginReport,
    request: &ClaudeUserPluginRequest,
    current: &Evaluation,
) -> bool {
    let Ok(Some(after)) = installed_target(report, &request.plugin_id) else {
        return false;
    };
    match request.action {
        ClaudeUserPluginAction::Install => current.catalog.as_ref().is_some_and(|entry| {
            entry
                .version
                .as_ref()
                .is_none_or(|version| after.version.as_ref() == Some(version))
        }),
        ClaudeUserPluginAction::Enable | ClaudeUserPluginAction::Disable => {
            current.installed.as_ref().is_some_and(|before| {
                let mut expected = before.clone();
                expected.enabled = Some(request.action.resulting_enabled());
                &expected == after
            })
        }
    }
}
