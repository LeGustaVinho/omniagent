use std::{io, path::Path, time::Duration};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    conversations::SessionScope,
    diagnostics,
    process::TerminalOutcome,
    process_io::CapturedProcess,
    protection,
    runner::{DirectWindowsRunner, LaunchRequest, Runner},
    store::Store,
};

use super::{
    Provider,
    profiles::{ConfiguredExecutable, command_probe, command_probe_with_limits, published_version},
};

pub mod management;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum PluginInventoryStatus {
    Reported,
    Partial,
    ExecutableMissing,
    Unsupported,
    ProbeFailed,
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ReportedPlugin {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub enabled: Option<bool>,
    pub scope: Option<String>,
    pub marketplace: Option<String>,
    pub source: Option<String>,
    pub source_location: Option<String>,
    pub install_path: Option<String>,
    pub skills: Option<Vec<String>>,
    pub hooks: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct PluginReport {
    pub provider: Provider,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub status: PluginInventoryStatus,
    /// The inventory command runs in a neutral working directory.
    pub plugins: Vec<ReportedPlugin>,
    pub load_telemetry_reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct PluginInventory {
    pub id: String,
    pub checked_at: String,
    pub report: PluginReport,
}

pub fn probe_plugins(configured: ConfiguredExecutable) -> Result<PluginReport> {
    let provider = configured.provider;
    let mut report = PluginReport {
        provider,
        executable: None,
        version: None,
        status: PluginInventoryStatus::ExecutableMissing,
        plugins: Vec::new(),
        load_telemetry_reported: false,
    };
    let path = match provider
        .adapter()
        .discover(configured.executable.as_deref().map(Path::new))
    {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            report.executable = configured.executable;
            return Ok(report);
        }
        Err(error) => return Err(error.into()),
    };
    report.executable = Some(path.to_string_lossy().into_owned());
    let directory = tempfile::Builder::new()
        .prefix("omniagent-plugins-")
        .tempdir()?;
    let cwd = directory.path();
    if let Ok(output) = command_probe(&path, cwd, provider.adapter().version_arguments())
        && output.status == TerminalOutcome::Exited(0)
    {
        report.version = published_version(provider, &output.stdout)
            .or_else(|| published_version(provider, &output.stderr));
    }
    read_inventory(
        &mut report,
        |request| DirectWindowsRunner.launch(request),
        &path,
        cwd,
        Duration::from_secs(25),
    );
    Ok(report)
}

pub(crate) fn probe_run_plugins(
    scope: &SessionScope,
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
) -> PluginReport {
    let mut report = PluginReport {
        provider: scope.provider,
        executable: scope.executable.to_str().map(str::to_owned),
        version: Some(scope.cli_version.clone()),
        status: PluginInventoryStatus::ProbeFailed,
        plugins: Vec::new(),
        load_telemetry_reported: false,
    };
    if let Ok(directory) = tempfile::Builder::new()
        .prefix("omniagent-plugins-")
        .tempdir()
    {
        read_inventory(
            &mut report,
            launch,
            &scope.executable,
            directory.path(),
            Duration::from_secs(5),
        );
    }
    report
}

fn read_inventory(
    report: &mut PluginReport,
    launch: impl FnOnce(LaunchRequest<'_>) -> io::Result<CapturedProcess>,
    executable: &Path,
    cwd: &Path,
    timeout: Duration,
) {
    let args = ["plugin", "list", "--json"].map(Into::into).to_vec();
    let output = command_probe_with_limits(launch, executable, cwd, args, timeout, 2 * 1024 * 1024);
    match output {
        Ok(output) if output.status == TerminalOutcome::Exited(0) => {
            (report.status, report.plugins) = decode_plugins(report.provider, &output.stdout);
        }
        Ok(output)
            if command_unsupported(&output.stderr) || command_unsupported(&output.stdout) =>
        {
            report.status = PluginInventoryStatus::Unsupported;
        }
        _ => report.status = PluginInventoryStatus::ProbeFailed,
    }
}

fn command_unsupported(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    [
        "unrecognized subcommand",
        "unknown command",
        "unknown option",
        "unexpected argument '--json'",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

fn decode_plugins(
    provider: Provider,
    output: &str,
) -> (PluginInventoryStatus, Vec<ReportedPlugin>) {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return (PluginInventoryStatus::InvalidResponse, Vec::new());
    };
    let rows = match provider {
        Provider::Codex => value["installed"].as_array(),
        Provider::Claude | Provider::Antigravity => value.as_array(),
    };
    let Some(rows) = rows else {
        return (PluginInventoryStatus::InvalidResponse, Vec::new());
    };
    let mut plugins = Vec::new();
    let mut partial = false;
    for row in rows {
        if let Some(plugin) = decode_plugin(provider, row) {
            plugins.push(plugin);
        } else {
            partial = true;
        }
    }
    plugins.sort_by(|a, b| {
        (&a.id, &a.scope, &a.install_path).cmp(&(&b.id, &b.scope, &b.install_path))
    });
    let status = if partial {
        PluginInventoryStatus::Partial
    } else {
        PluginInventoryStatus::Reported
    };
    (status, plugins)
}

fn decode_plugin(provider: Provider, row: &Value) -> Option<ReportedPlugin> {
    let id_field = match provider {
        Provider::Codex => "pluginId",
        Provider::Claude => "id",
        Provider::Antigravity => "name",
    };
    let id = row[id_field].as_str()?.trim();
    if id.is_empty() || id.len() > 1024 {
        return None;
    }
    let source = &row["source"];
    let marketplace = text(&row["marketplaceName"]).or_else(|| {
        (provider == Provider::Claude)
            .then(|| id.rsplit_once('@'))
            .flatten()
            .and_then(|(name, market)| {
                (!name.is_empty() && !market.is_empty()).then(|| market.to_owned())
            })
    });
    let source_location = ["url", "repo", "path"]
        .into_iter()
        .find_map(|key| source.get(key).and_then(Value::as_str))
        .map(source_location);
    Some(ReportedPlugin {
        id: id.to_owned(),
        name: text(&row["name"]),
        version: text(&row["version"]),
        enabled: row["enabled"].as_bool(),
        scope: text(&row["scope"]),
        marketplace,
        source: source
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(self::source_location)
            .or_else(|| text(&source["source"]))
            .or_else(|| text(&source["type"])),
        source_location,
        install_path: text(&row["installPath"]).or_else(|| text(&row["path"])),
        skills: components(&row["skills"]),
        hooks: components(&row["hooks"]),
        mcp_servers: components(&row["mcpServers"]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_installed_and_disabled_records_and_keeps_missing_metadata_unknown() {
        let (status, codex) = decode_plugins(
            Provider::Codex,
            r#"{
            "installed": [{"pluginId":"first@local","name":"First","marketplaceName":"local","enabled":false,
            "source":{"source":"url","url":"https://user:password@example.invalid/repo?secret=query"}}],
            "available":[{"pluginId":"uninstalled"}]
        }"#,
        );
        assert_eq!(status, PluginInventoryStatus::Reported);
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].id, "first@local");
        assert_eq!(codex[0].enabled, Some(false));
        assert_eq!(
            codex[0].source_location.as_deref(),
            Some("https://example.invalid/repo")
        );
        assert!(codex[0].skills.is_none());
        assert!(codex[0].version.is_none());

        let (status, claude) = decode_plugins(
            Provider::Claude,
            r#"[{"id":"first@fixture","version":"2.0","scope":"user","enabled":true}]"#,
        );
        assert_eq!(status, PluginInventoryStatus::Reported);
        assert_eq!(claude[0].marketplace.as_deref(), Some("fixture"));
        assert_eq!(claude[0].version.as_deref(), Some("2.0"));
        assert_eq!(claude[0].scope.as_deref(), Some("user"));
        assert_eq!(claude[0].enabled, Some(true));

        let (status, agy) = decode_plugins(
            Provider::Antigravity,
            r#"[{"name":"first","source":"builtin","skills":["a","b"],"hooks":[]} ,{}]"#,
        );
        assert_eq!(status, PluginInventoryStatus::Partial);
        assert_eq!(agy[0].skills.as_ref().unwrap(), &["a", "b"]);
        assert_eq!(agy[0].hooks, Some(Vec::new()));
        assert!(agy[0].mcp_servers.is_none());
        assert!(agy[0].enabled.is_none());
        assert_eq!(
            decode_plugins(Provider::Codex, "{}").0,
            PluginInventoryStatus::InvalidResponse
        );
        assert!(command_unsupported("error: unrecognized subcommand 'list'"));
        assert!(!command_unsupported("authentication failed"));
    }

    #[tokio::test]
    async fn preserves_plugin_observations_per_provider() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let db = tmp.path().join("plugins.sqlite");
        let store = Store::open(db.clone())?;
        let (status, plugins) = decode_plugins(
            Provider::Claude,
            r#"[{"id":"first@fixture","version":"2.0","enabled":false}]"#,
        );
        let report = PluginReport {
            provider: Provider::Claude,
            executable: Some(r"C:\fixture\claude.exe".into()),
            version: Some("2.1.296".into()),
            status,
            plugins,
            load_telemetry_reported: false,
        };
        let first = store.save_plugin_inventory(report.clone()).await?;
        assert!(
            store
                .latest_plugin_inventory(Provider::Codex)
                .await?
                .is_none()
        );
        let second = store
            .save_plugin_inventory(PluginReport {
                status: PluginInventoryStatus::Unsupported,
                plugins: Vec::new(),
                ..report
            })
            .await?;
        assert_ne!(second.id, first.id);
        let store = Store::open(db)?;
        assert_eq!(
            store.latest_plugin_inventory(Provider::Claude).await?,
            Some(second)
        );
        assert_eq!(
            store
                .execute(|conn| Ok(conn.query_row(
                    "SELECT count(*) FROM plugin_inventories",
                    [],
                    |row| row.get::<_, i64>(0)
                )?))
                .await?,
            2
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires installed CLIs with JSON plugin list support"]
    fn live_inventory_reads_the_vendor_profiles() -> Result<()> {
        for provider in Provider::ALL {
            let report = probe_plugins(ConfiguredExecutable {
                provider,
                executable: None,
            })?;
            assert_eq!(
                report.status,
                PluginInventoryStatus::Reported,
                "{provider:?}"
            );
            assert!(report.executable.is_some());
            assert!(report.version.is_some());
            assert!(!report.load_telemetry_reported);
        }
        Ok(())
    }
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(diagnostics::preview)
}

fn source_location(value: &str) -> String {
    if let Ok(mut url) = tauri::Url::parse(value)
        && matches!(url.scheme(), "http" | "https" | "ssh")
    {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return diagnostics::preview(url.as_str());
    }
    diagnostics::preview(value)
}

fn components(value: &Value) -> Option<Vec<String>> {
    if let Some(rows) = value.as_array() {
        return rows
            .iter()
            .map(|entry| {
                text(entry)
                    .or_else(|| text(&entry["name"]))
                    .or_else(|| text(&entry["id"]))
            })
            .collect();
    }
    value
        .as_object()
        .map(|fields| fields.keys().map(|key| diagnostics::preview(key)).collect())
}

impl Store {
    pub async fn save_plugin_inventory(&self, report: PluginReport) -> Result<PluginInventory> {
        let payload = protection::protect(&serde_json::to_vec(&report)?)?;
        self.execute(move |db| {
            let id = Uuid::new_v4().to_string();
            let checked_at = db.query_row(
                "INSERT INTO plugin_inventories(id, provider, payload)
                 VALUES (?1, ?2, ?3) RETURNING checked_at",
                params![id, report.provider.as_str(), payload],
                |row| row.get(0),
            )?;
            Ok(PluginInventory {
                id,
                checked_at,
                report,
            })
        })
        .await
    }

    pub async fn latest_plugin_inventory(
        &self,
        provider: Provider,
    ) -> Result<Option<PluginInventory>> {
        self.execute(move |db| {
            let record = db
                .query_row(
                    "SELECT id, checked_at, payload FROM plugin_inventories
                 WHERE provider = ?1 ORDER BY seq DESC LIMIT 1",
                    [provider.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                        ))
                    },
                )
                .optional()?;
            record
                .map(|(id, checked_at, payload)| {
                    let report = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                    Ok(PluginInventory {
                        id,
                        checked_at,
                        report,
                    })
                })
                .transpose()
        })
        .await
    }
}
