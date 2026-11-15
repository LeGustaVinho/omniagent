use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::{LazyLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use rusqlite::OptionalExtension;
use serde::Serialize;
use ts_rs::TS;

use crate::{
    model_catalog::{MODEL_CATALOG, ModelOption, ObservedModel},
    process::TerminalOutcome,
    process_io::{CapturedProcess, Stream},
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    store::Store,
};

use super::{
    Authentication, CliAdapter, ProbeKind, Provider, ProviderEventData, ReportedModel,
    SessionIntent, SessionMode, SessionOption, SessionRequest,
};

const PROBE_TIME: Duration = Duration::from_secs(10);
const MAX_PROBE_OUTPUT: usize = 32 * 1024;
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]{1,80})?$").expect("valid version pattern")
});

#[derive(Debug, Clone, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConfiguredExecutable {
    pub provider: Provider,
    pub executable: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ProtocolCheck {
    Unchecked,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum PermissionCheck {
    Unverified,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct CliProbe {
    pub provider: Provider,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub protocol: ProtocolCheck,
    pub authentication: Authentication,
    pub effective_permissions: PermissionCheck,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopModel {
    pub option: ModelOption,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

pub const DESKTOP_PERMISSION_REVISION: &str = "desktop-direct-v1";

pub struct DesktopProfile {
    pub executable: PathBuf,
    pub version: String,
    pub models: Vec<DesktopModel>,
}

/// Query the CLI in a neutral directory with no prompt or project customizations.
pub fn desktop_profile(
    configured: ConfiguredExecutable,
    model_id: Option<String>,
) -> Result<DesktopProfile> {
    let provider = configured.provider;
    let check = probe_cli(configured)?;
    let executable = PathBuf::from(
        check
            .executable
            .context("The CLI executable is unavailable")?,
    );
    let version = check
        .version
        .context("The CLI did not report its version")?;
    ensure!(
        check.protocol == ProtocolCheck::Verified,
        "The CLI transport is unavailable"
    );
    ensure!(
        check.authentication == Authentication::Authenticated
            || provider == Provider::Antigravity
                && check.authentication != Authentication::Required,
        "Sign in to the configured CLI before opening a conversation"
    );
    let neutral = tempfile::tempdir()?;
    let cwd = dunce::canonicalize(neutral.path())?;
    let mut session = provider.adapter().create_session(SessionRequest {
        checkout: cwd.clone(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: model_id,
        effort: None,
    })?;
    let models = run_probe(
        |request| DirectWindowsRunner.launch(request),
        &executable,
        &cwd,
        &session.arguments(),
        PROBE_TIME,
        |mut process, deadline| {
            for frame in session.start()?.outbound {
                process.input.send(frame.as_bytes())?;
            }
            let mut models: Option<Vec<ReportedModel>> = None;
            let mut session_model = None;
            let mut session_effort = None;
            loop {
                let frame = process
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
                let update = session.receive(frame)?;
                for frame in update.outbound {
                    process.input.send(frame.as_bytes())?;
                }
                for event in update.events {
                    match event.data {
                        ProviderEventData::Error { .. } => bail!("The CLI could not list models"),
                        ProviderEventData::ModelInventory { models: inventory } => {
                            models = Some(inventory)
                        }
                        ProviderEventData::SessionOptions { options } => {
                            session_effort = options.into_iter().find(|option| {
                                option.category.as_deref() == Some("thought_level")
                                    || option.id == "thought_level"
                            });
                        }
                        ProviderEventData::SessionReady { model, .. } => session_model = model,
                        _ => {}
                    }
                }
                if let Some(models) = models.take() {
                    let observed = models
                        .iter()
                        .map(|model| ObservedModel {
                            id: model.id.clone(),
                            canonical_id: model.canonical_id.clone(),
                            name: model.name.clone(),
                            selectable: model.selectable != Some(false),
                            unavailable_reason: (model.selectable == Some(false))
                                .then_some("Unavailable in this CLI session".into()),
                        })
                        .collect::<Vec<_>>();
                    let options = MODEL_CATALOG.project_options(
                        provider,
                        &format!("desktop-direct:{}", provider.as_str()),
                        &observed,
                    );
                    return Ok(options
                        .into_iter()
                        .zip(models)
                        .map(|(option, reported)| {
                            let (efforts, default_effort) = model_effort_levels(
                                reported,
                                session_model.as_deref(),
                                session_effort.as_ref(),
                            );
                            DesktopModel {
                                option,
                                efforts,
                                default_effort,
                            }
                        })
                        .collect());
                }
            }
        },
    )?;
    Ok(DesktopProfile {
        executable,
        version,
        models,
    })
}

fn model_effort_levels(
    reported: ReportedModel,
    session_model: Option<&str>,
    session_effort: Option<&SessionOption>,
) -> (Vec<String>, Option<String>) {
    if let Some(efforts) = reported.effort_levels {
        return (efforts, reported.default_effort);
    }
    if session_model == Some(reported.id.as_str())
        && let Some(effort) = session_effort
    {
        return (
            effort
                .choices
                .iter()
                .map(|choice| choice.value.clone())
                .collect(),
            effort.current.clone(),
        );
    }
    (Vec::new(), None)
}

impl Store {
    pub async fn configured_executable(&self, provider: Provider) -> Result<ConfiguredExecutable> {
        let executable = self
            .execute(move |connection| {
                Ok(connection
                    .query_row(
                        "SELECT executable FROM cli_executables WHERE provider = ?1",
                        [provider.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await?;
        Ok(ConfiguredExecutable {
            provider,
            executable,
        })
    }

    pub async fn set_cli_executable(
        &self,
        provider: Provider,
        claimed: Option<PathBuf>,
    ) -> Result<ConfiguredExecutable> {
        let executable = claimed
            .map(|path| {
                provider
                    .adapter()
                    .discover(Some(&path))?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| {
                        anyhow::anyhow!("CLI executable path cannot be represented in UTF-8")
                    })
            })
            .transpose()?;
        let saved = executable.clone();
        self.execute(move |connection| {
            match executable {
                Some(path) => {
                    connection.execute(
                        "INSERT INTO cli_executables (provider, executable) VALUES (?1, ?2)
                         ON CONFLICT (provider) DO UPDATE SET executable = excluded.executable",
                        [provider.as_str(), &path],
                    )?;
                }
                None => {
                    connection.execute(
                        "DELETE FROM cli_executables WHERE provider = ?1",
                        [provider.as_str()],
                    )?;
                }
            }
            Ok(())
        })
        .await?;
        Ok(ConfiguredExecutable {
            provider,
            executable: saved,
        })
    }
}

pub fn probe_cli(configured: ConfiguredExecutable) -> Result<CliProbe> {
    let provider = configured.provider;
    let adapter = provider.adapter();
    let mut check = CliProbe {
        provider,
        executable: None,
        version: None,
        protocol: ProtocolCheck::Unchecked,
        authentication: Authentication::Unknown,
        effective_permissions: PermissionCheck::Unverified,
    };
    let exe = match adapter.discover(configured.executable.as_deref().map(Path::new)) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(check),
        Err(error) => return Err(error).context("verify CLI executable"),
    };
    check.executable = Some(exe.to_string_lossy().into_owned());
    // Discovery and auth checks run outside the project, with vendor-managed
    // credentials. No prompt is sent and raw process output is never persisted.
    let neutral = tempfile::Builder::new()
        .prefix("omniagent-cli-probe-")
        .tempdir()?;
    let cwd = neutral.path();
    let version = command_probe(&exe, cwd, adapter.version_arguments());
    check.version = version.ok().and_then(|output| {
        (output.status == TerminalOutcome::Exited(0))
            .then(|| {
                published_version(provider, &output.stdout)
                    .or_else(|| published_version(provider, &output.stderr))
            })
            .flatten()
    });
    if check.version.is_none() {
        return Ok(check);
    }
    let authentication = adapter.authentication_probe();
    if matches!(authentication.kind, ProbeKind::Command)
        && let Ok(output) = command_probe(&exe, cwd, authentication.arguments)
    {
        check.authentication = adapter.decode_authentication(output.status, &output.stdout);
        if check.authentication == Authentication::Unknown {
            check.authentication = adapter.decode_authentication(output.status, &output.stderr);
        }
    }
    match handshake_probe(
        adapter,
        &exe,
        cwd,
        matches!(authentication.kind, ProbeKind::SessionHandshake),
    ) {
        Ok(auth) => {
            check.protocol = ProtocolCheck::Verified;
            if auth != Authentication::Unknown {
                check.authentication = auth;
            }
        }
        Err(_) => check.protocol = ProtocolCheck::Failed,
    }
    Ok(check)
}

pub(super) fn published_version(provider: Provider, output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let candidate = match provider {
            Provider::Codex => line.trim().strip_prefix("codex-cli ")?,
            Provider::Claude => line.trim().strip_suffix(" (Claude Code)")?,
            Provider::Antigravity => line.trim(),
        };
        VERSION.is_match(candidate).then(|| candidate.to_owned())
    })
}

pub(super) struct CommandOutput {
    pub status: TerminalOutcome,
    pub stdout: String,
    pub stderr: String,
}

pub(super) fn command_probe(
    executable: &Path,
    cwd: &Path,
    args: Vec<OsString>,
) -> Result<CommandOutput> {
    command_probe_with_limits(
        |request| DirectWindowsRunner.launch(request),
        executable,
        cwd,
        args,
        PROBE_TIME,
        MAX_PROBE_OUTPUT,
    )
}

pub(super) fn command_probe_with_limits(
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
    executable: &Path,
    cwd: &Path,
    args: Vec<OsString>,
    time_limit: Duration,
    output_limit: usize,
) -> Result<CommandOutput> {
    run_probe(
        launch,
        executable,
        cwd,
        &args,
        time_limit,
        |process, deadline| {
            let CapturedProcess {
                input,
                output,
                control,
            } = process;
            drop(input);
            let mut stdout = String::new();
            let mut stderr = String::new();
            loop {
                match output.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(Ok(frame)) => {
                        ensure!(
                            stdout.len() + stderr.len() + frame.data.len() <= output_limit,
                            "CLI probe output exceeded its limit"
                        );
                        match frame.stream {
                            Stream::Stdout => stdout.push_str(&frame.data),
                            Stream::Stderr => stderr.push_str(&frame.data),
                        }
                    }
                    Ok(Err(error)) => return Err(error.into()),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => bail!("CLI probe timed out"),
                }
            }
            Ok(CommandOutput {
                status: control.wait()?,
                stdout,
                stderr,
            })
        },
    )
}

fn handshake_probe(
    adapter: &dyn CliAdapter,
    executable: &Path,
    cwd: &Path,
    check_session_auth: bool,
) -> Result<Authentication> {
    let mut session = adapter.create_session(SessionRequest {
        checkout: cwd.into(),
        intent: SessionIntent::Start,
        mode: SessionMode::ReadOnly,
        model: None,
        effort: None,
    })?;
    run_probe(
        |request| DirectWindowsRunner.launch(request),
        executable,
        cwd,
        &session.arguments(),
        PROBE_TIME,
        |mut process, deadline| {
            for frame in session.start()?.outbound {
                process.input.send(frame.as_bytes())?;
            }
            loop {
                let frame = process
                    .output
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
                let response = session.receive(frame)?;
                for event in response.events {
                    match event.data {
                        ProviderEventData::Handshake { .. } if !check_session_auth => {
                            return Ok(Authentication::Unknown);
                        }
                        ProviderEventData::Authentication {
                            authenticated: Some(false),
                        } => return Ok(Authentication::Required),
                        ProviderEventData::SessionReady { .. } => {
                            return Ok(Authentication::Unknown);
                        }
                        ProviderEventData::Error { .. } => bail!("CLI protocol negotiation failed"),
                        _ => {}
                    }
                }
                for frame in response.outbound {
                    process.input.send(frame.as_bytes())?;
                }
            }
        },
    )
}

fn run_probe<T>(
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
    executable: &Path,
    cwd: &Path,
    args: &[OsString],
    time_limit: Duration,
    collect: impl FnOnce(CapturedProcess, Instant) -> Result<T>,
) -> Result<T> {
    let arguments = args
        .iter()
        .map(|value| value.as_os_str())
        .collect::<Vec<_>>();
    let process = launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable,
        checkout: cwd,
        arguments: &arguments,
    })?;
    let control = process.control.clone();
    let timeout_control = control.clone();
    let deadline = Instant::now() + time_limit;
    let (finished, done) = mpsc::channel();
    thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            if done
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                let _ = timeout_control.cancel();
            }
        });
        let result = collect(process, deadline);
        let stopped = control.cancel();
        let _ = finished.send(());
        watchdog.join().expect("CLI probe watchdog");
        stopped?;
        control.wait()?;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_options_are_scoped_to_the_reported_model() -> Result<()> {
        let advertised: ReportedModel = serde_json::from_value(serde_json::json!({
            "id": "advertised", "name": "Advertised",
            "effortLevels": ["low", "medium", "high", "xhigh", "max", "ultra"],
            "defaultEffort": "low"
        }))?;
        let discovered: ReportedModel =
            serde_json::from_value(serde_json::json!({ "id": "selected", "name": "Selected" }))?;
        let session = serde_json::from_value(serde_json::json!({
            "id": "thought_level", "name": "Thinking level", "current": "medium",
            "choices": [{"name": "Medium", "value": "medium"}, {"name": "Extra high", "value": "xhigh"}]
        }))?;
        let (levels, default) = model_effort_levels(advertised, Some("selected"), Some(&session));
        assert_eq!(levels, ["low", "medium", "high", "xhigh", "max", "ultra"]);
        assert_eq!(default.as_deref(), Some("low"));
        let (levels, default) =
            model_effort_levels(discovered.clone(), Some("selected"), Some(&session));
        assert_eq!(levels, ["medium", "xhigh"]);
        assert_eq!(default.as_deref(), Some("medium"));
        assert_eq!(
            model_effort_levels(discovered, Some("different-model"), Some(&session)),
            (Vec::new(), None)
        );
        Ok(())
    }

    #[tokio::test]
    async fn overrides_are_scoped_and_survive_reopening_the_store() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let db = temporary.path().join("state.sqlite");
        let executable = temporary.path().join("custom-claude.exe");
        std::fs::write(&executable, [])?;
        let store = Store::open(db.clone())?;
        assert!(
            store
                .set_cli_executable(Provider::Claude, Some("relative.exe".into()))
                .await
                .is_err()
        );
        let saved = store
            .set_cli_executable(Provider::Claude, Some(executable))
            .await?;
        assert!(saved.executable.is_some());
        assert!(
            store
                .configured_executable(Provider::Codex)
                .await?
                .executable
                .is_none()
        );
        drop(store);
        let reopened = Store::open(db)?;
        assert_eq!(
            reopened.configured_executable(Provider::Claude).await?,
            saved
        );
        reopened.set_cli_executable(Provider::Claude, None).await?;
        assert!(
            reopened
                .configured_executable(Provider::Claude)
                .await?
                .executable
                .is_none()
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires all three installed CLI executables and vendor login for Codex and Claude"]
    fn live_probe_keeps_negotiation_authentication_and_permissions_separate() -> Result<()> {
        for provider in Provider::ALL {
            let result = probe_cli(ConfiguredExecutable {
                provider,
                executable: None,
            })?;
            assert!(result.version.is_some(), "missing {provider:?} version");
            assert_eq!(result.protocol, ProtocolCheck::Verified, "{provider:?}");
            assert_eq!(result.effective_permissions, PermissionCheck::Unverified);
            if provider != Provider::Antigravity {
                assert_eq!(
                    result.authentication,
                    Authentication::Authenticated,
                    "{provider:?}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn version_parser_keeps_only_vendor_version_tokens() {
        assert_eq!(
            published_version(Provider::Codex, "codex-cli 0.169.0-alpha.11.1\n"),
            Some("0.169.0-alpha.11.1".into())
        );
        assert_eq!(
            published_version(Provider::Claude, "2.1.296 (Claude Code)\n"),
            Some("2.1.296".into())
        );
        assert_eq!(published_version(Provider::Claude, "api-key=fixture"), None);
        assert_eq!(
            published_version(Provider::Antigravity, "1.5.9\n"),
            Some("1.5.9".into())
        );
    }
}
