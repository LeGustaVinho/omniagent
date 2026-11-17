use std::io::Read;

use super::*;

mod package;
#[cfg(test)]
mod tests;

use package::Package;

const CODEX_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeUserPluginRequest {
    pub provider: Provider,
    pub action: UserPluginAction,
    pub target: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeUserPluginPreview {
    pub id: String,
    pub provider: Provider,
    pub action: UserPluginAction,
    pub plugin_id: String,
    pub execution_scope: String,
    pub configuration_root: PathBuf,
    pub executable: PathBuf,
    pub cli_version: String,
    pub persists: bool,
    pub version: String,
    pub marketplace: Option<String>,
    pub source: PathBuf,
    pub inventory_before: PluginInventory,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUserPluginCatalogEntry {
    pub plugin_id: String,
    pub version: String,
    pub source: PathBuf,
    #[serde(skip)]
    marketplace_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct Target {
    plugin_id: String,
    version: String,
    marketplace: Option<String>,
    source: PathBuf,
    package_revision: String,
    marketplace_revision: Option<String>,
    installed: Option<ReportedPlugin>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SealedNativePreview {
    preview: NativeUserPluginPreview,
    request: NativeUserPluginRequest,
    profile: EffectiveProfile,
    config_revision: [Option<String>; 2],
    target: Target,
}

struct LocalCli {
    provider: Provider,
    profile: EffectiveProfile,
    cwd: tempfile::TempDir,
    directories: Vec<(PathBuf, String, File)>,
    _executable: File,
}

impl LocalCli {
    fn qualify(configured: ConfiguredExecutable) -> Result<Self> {
        let provider = configured.provider;
        ensure!(
            matches!(provider, Provider::Codex | Provider::Antigravity),
            "Use the reviewed Claude plugin manager for this provider"
        );
        let executable = dunce::canonicalize(
            provider
                .adapter()
                .discover(configured.executable.as_deref().map(Path::new))?,
        )?;
        let locked = configuration::open_locked_local_file(&executable)?;
        let executable_revision = DirectWindowsRunner
            .executable_revision(&executable)
            .context("Could not verify the CLI executable fingerprint")?;
        let (root, directories) = configuration_directories(provider)?;
        let cwd = tempfile::tempdir()?;
        let version = run_plugin_command(&executable, cwd.path(), &["--version"], READ_TIMEOUT)?;
        let cli_version = published_version(provider, &version.stdout)
            .or_else(|| published_version(provider, &version.stderr))
            .filter(|_| version.status == TerminalOutcome::Exited(0))
            .context("Could not verify the CLI version")?;
        let configuration_identity = scope_revision(&directories)?;
        Ok(Self {
            provider,
            profile: EffectiveProfile {
                executable,
                executable_revision,
                cli_version,
                configuration_root: root,
                configuration_identity,
            },
            cwd,
            directories,
            _executable: locked,
        })
    }

    fn verify_root(&self) -> Result<()> {
        let (root, directories) = configuration_directories(self.provider)?;
        ensure!(
            root == self.profile.configuration_root
                && self.directories.iter().all(|(path, id, _)| {
                    directories
                        .iter()
                        .any(|(actual, identity, _)| actual == path && identity == id)
                }),
            "The active CLI configuration root changed"
        );
        Ok(())
    }

    fn current_profile(&self) -> Result<EffectiveProfile> {
        self.verify_root()?;
        let (_, directories) = configuration_directories(self.provider)?;
        Ok(EffectiveProfile {
            configuration_identity: scope_revision(&directories)?,
            ..self.profile.clone()
        })
    }

    fn probe(&self, args: &[&str], timeout: Duration) -> Result<CommandOutput> {
        self.verify_root()?;
        run_plugin_command(&self.profile.executable, self.cwd.path(), args, timeout)
    }

    fn installed(&self) -> Result<PluginReport> {
        let timeout = if self.provider == Provider::Codex {
            CODEX_READ_TIMEOUT
        } else {
            READ_TIMEOUT
        };
        let response = self.probe(&["plugin", "list", "--json"], timeout)?;
        let (status, plugins) = decode_plugins(self.provider, &response.stdout);
        ensure!(
            response.status == TerminalOutcome::Exited(0)
                && status == PluginInventoryStatus::Reported,
            "The CLI did not report a complete installed plugin inventory"
        );
        if self.provider == Provider::Antigravity {
            for plugin in &plugins {
                agy_installed_source(&self.profile.configuration_root, plugin)?;
            }
        }
        Ok(PluginReport {
            provider: self.provider,
            executable: self.profile.executable.to_str().map(str::to_owned),
            version: Some(self.profile.cli_version.clone()),
            status,
            plugins,
            load_telemetry_reported: false,
        })
    }

    fn codex_catalog(&self, marketplace: &str) -> Result<Vec<CodexUserPluginCatalogEntry>> {
        let response = self.probe(
            &[
                "plugin",
                "list",
                "--marketplace",
                marketplace,
                "--available",
                "--json",
            ],
            CODEX_READ_TIMEOUT,
        )?;
        ensure!(
            response.status == TerminalOutcome::Exited(0),
            "Codex could not list the requested marketplace"
        );
        let catalog: Value = serde_json::from_str(&response.stdout)?;
        let rows = catalog["available"]
            .as_array()
            .context("Codex did not report an available plugin catalog")?;
        let mut entries: Vec<_> = rows
            .iter()
            .filter_map(|row| codex_local_entry(row, marketplace))
            .collect();
        entries.sort_by(|first, second| first.plugin_id.cmp(&second.plugin_id));
        ensure!(
            !entries
                .windows(2)
                .any(|pair| pair[0].plugin_id == pair[1].plugin_id),
            "The configured marketplace contains duplicate plugin identities"
        );
        Ok(entries)
    }

    fn configuration_revision(&self) -> Result<[Option<String>; 2]> {
        let root = &self.profile.configuration_root;
        let digest = |name: &str| match configuration::read_local_file(&root.join(name)) {
            Ok(bytes) => Ok(Some(format!("{:x}", Sha256::digest(bytes)))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("Could not verify the CLI plugin configuration"),
        };
        match self.provider {
            Provider::Codex => Ok([digest("config.toml")?, None]),
            _ => Ok([digest("config.json")?, digest("import_manifest.json")?]),
        }
    }

    fn supports(&self, action: UserPluginAction) -> Result<()> {
        match self.provider {
            Provider::Codex => {
                ensure!(
                    action == UserPluginAction::Install,
                    "Codex exposes native plugin installation here"
                );
                let help = self.probe(&["plugin", "add", "--help"], READ_TIMEOUT)?;
                ensure!(
                    help.status == TerminalOutcome::Exited(0) && help.stdout.contains("--json"),
                    "This Codex build does not expose JSON plugin installation"
                );
            }
            _ => {
                let help = self.probe(&["plugin", "--help"], READ_TIMEOUT)?;
                ensure!(
                    help.status == TerminalOutcome::Exited(0)
                        && help.stdout.lines().chain(help.stderr.lines()).any(|line| line.split_whitespace().next() == Some(action.subcommand())),
                    "This Antigravity build does not expose the requested plugin operation"
                );
            }
        }
        Ok(())
    }
}

type ScopeDirectories = Vec<(PathBuf, String, File)>;

fn scope_revision(directories: &ScopeDirectories) -> Result<String> {
    let mut digest = Sha256::new();
    for (path, identity, _) in directories {
        digest.update(serde_json::to_vec(&(path, identity))?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn ordinary_root(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "Select an absolute plugin directory");
    let canonical = dunce::canonicalize(path)?;
    ensure!(
        canonical == dunce::simplified(path),
        "The plugin directory is redirected"
    );
    open_config_root(&canonical)?;
    Ok(canonical)
}

fn configuration_directories(provider: Provider) -> Result<(PathBuf, ScopeDirectories)> {
    let user = || {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .context("Cannot locate the Windows user profile")
    };
    let (anchor, directories, scope_root_length): (PathBuf, &[_], usize) = match provider {
        Provider::Codex => {
            let configured = env::var_os("CODEX_HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from);
            let home = match configured {
                Some(path) => path,
                None => user()?.join(".codex"),
            };
            (home, &["plugins", "cache"], 0)
        }
        _ => (user()?, &[".gemini", "config", "plugins"], 2),
    };
    let anchor = ordinary_root(&anchor).context("Cannot establish the CLI's user plugin scope")?;
    let mut path = anchor.clone();
    let mut locked = Vec::new();
    for name in std::iter::once("").chain(directories.iter().copied()) {
        path.push(name);
        match open_config_root(&path) {
            Ok(directory) => {
                let id = directory_identity(&directory)?;
                locked.push((path.clone(), id, directory));
            }
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let root = directories
        .iter()
        .take(scope_root_length)
        .fold(anchor, |path, child| path.join(child));
    Ok((root, locked))
}

fn exact_name(name: &str) -> Result<()> {
    ensure!(
        name.as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "Choose an exact plugin or marketplace name"
    );
    Ok(())
}

fn codex_local_entry(row: &Value, marketplace: &str) -> Option<CodexUserPluginCatalogEntry> {
    let id = row["pluginId"].as_str()?;
    let (_, market) = plugin_selector(id).ok()?;
    if market != marketplace
        || row["marketplaceName"] != marketplace
        || row["installed"] != false
        || row["source"]["source"] != "local"
        || row["marketplaceSource"]["sourceType"] != "local"
        || row["installPolicy"] != "AVAILABLE"
    {
        return None;
    }
    let market_root =
        ordinary_root(Path::new(row["marketplaceSource"]["source"].as_str()?)).ok()?;
    let source = ordinary_root(Path::new(row["source"]["path"].as_str()?)).ok()?;
    if source == market_root || !source.starts_with(&market_root) {
        return None;
    }
    Some(CodexUserPluginCatalogEntry {
        plugin_id: id.into(),
        version: row["version"].as_str()?.into(),
        source,
        marketplace_root: market_root,
    })
}

fn pin_codex_marketplace(
    entry: &CodexUserPluginCatalogEntry,
    package: &mut Package,
) -> Result<String> {
    let (name, marketplace) = plugin_selector(&entry.plugin_id)?;
    let manifest = entry
        .marketplace_root
        .join(".agents/plugins/marketplace.json");
    let file = configuration::open_locked_local_file(&manifest)
        .context("The reviewed local marketplace declaration is unavailable")?;
    let mut bytes = Vec::new();
    (&file)
        .take(CATALOG_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= CATALOG_LIMIT,
        "The marketplace declaration exceeds its supported size"
    );
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        value["name"] == marketplace,
        "The local marketplace identity changed"
    );
    let mut plugins = value["plugins"]
        .as_array()
        .context("The marketplace has no local plugin declarations")?
        .iter()
        .filter(|plugin| plugin["name"] == name);
    let selected = plugins
        .next()
        .context("The reviewed plugin is missing from the local marketplace")?;
    ensure!(
        plugins.next().is_none() && selected["source"]["source"] == "local",
        "The local marketplace plugin is ambiguous"
    );
    let path = selected["source"]["path"]
        .as_str()
        .context("The marketplace does not declare a local path")?;
    let declared = dunce::canonicalize(entry.marketplace_root.join(path))?;
    ensure!(
        declared == entry.source,
        "The marketplace source changed after discovery"
    );
    package.marketplace = Some(file);
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn agy_installed_source(root: &Path, installed: &ReportedPlugin) -> Result<PathBuf> {
    exact_name(&installed.id)?;
    let directory = root.join("plugins").join(&installed.id);
    ensure!(
        installed.install_path.as_deref().map(Path::new) == Some(directory.as_path()),
        "The installed Antigravity plugin is outside the verified user scope"
    );
    ordinary_root(&directory)
}

fn installed_target<'a>(report: &'a PluginReport, id: &str) -> Result<Option<&'a ReportedPlugin>> {
    let mut matched = report
        .plugins
        .iter()
        .filter(|plugin| plugin.id.eq_ignore_ascii_case(id));
    let first = matched.next();
    ensure!(
        matched.next().is_none() && first.is_none_or(|plugin| plugin.id == id),
        "The installed plugin identity is ambiguous"
    );
    Ok(first)
}

fn evaluate(
    cli: &LocalCli,
    request: &NativeUserPluginRequest,
    report: &PluginReport,
) -> Result<(Target, Package)> {
    match (request.provider, request.action) {
        (Provider::Codex, UserPluginAction::Install) => {
            let (_, marketplace) = plugin_selector(&request.target)?;
            ensure!(
                installed_target(report, &request.target)?.is_none(),
                "This Codex plugin is already installed"
            );
            let catalog = cli.codex_catalog(marketplace)?;
            let mut rows = catalog
                .into_iter()
                .filter(|entry| entry.plugin_id == request.target);
            let entry = rows
                .next()
                .context("The exact plugin is unavailable from the configured local marketplace")?;
            ensure!(
                rows.next().is_none(),
                "The configured Codex plugin is ambiguous"
            );
            let mut package = Package::open(&entry.source)?;
            let marketplace_revision = pin_codex_marketplace(&entry, &mut package)?;
            Ok((
                Target {
                    plugin_id: entry.plugin_id,
                    version: entry.version,
                    marketplace: Some(marketplace.to_owned()),
                    source: entry.source,
                    package_revision: package.revision.clone(),
                    marketplace_revision: Some(marketplace_revision),
                    installed: None,
                },
                package,
            ))
        }
        (Provider::Antigravity, UserPluginAction::Install) => {
            let root = ordinary_root(Path::new(&request.target))?;
            ensure!(
                root.join("plugin.json").is_file()
                    || root.join(".antigravity-plugin/plugin.json").is_file(),
                "Select a local Antigravity plugin folder containing its manifest"
            );
            let mut package = Package::open(&root)?;
            let staged = package.stage()?;
            let staged_path = staged
                .path()
                .to_str()
                .context("The staged plugin path is unavailable")?;
            let outcome = cli.probe(
                &["plugin", "validate", "--strict", "--json", staged_path],
                READ_TIMEOUT,
            )?;
            ensure!(
                outcome.status == TerminalOutcome::Exited(0),
                "Antigravity rejected validation of the local plugin"
            );
            let validation: Value = serde_json::from_str(&outcome.stdout)?;
            let id = validation["name"]
                .as_str()
                .context("Antigravity did not identify the plugin")?;
            exact_name(id)?;
            let manifest = validation["manifest"]
                .as_str()
                .context("Antigravity did not identify the plugin manifest")?;
            ensure!(
                matches!(
                    manifest,
                    "plugin.json"
                        | ".antigravity-plugin/plugin.json"
                        | ".antigravity-plugin\\plugin.json"
                ),
                "Antigravity reported an unexpected manifest path"
            );
            let manifest: Value = serde_json::from_slice(&configuration::read_local_file(
                &staged.path().join(manifest),
            )?)?;
            let version = manifest["version"]
                .as_str()
                .filter(|version| !version.trim().is_empty())
                .context("The reviewed plugin has no version")?;
            ensure!(
                installed_target(report, id)?.is_none(),
                "This Antigravity plugin is already installed"
            );
            Ok((
                Target {
                    plugin_id: id.to_owned(),
                    version: version.to_owned(),
                    marketplace: None,
                    source: package.root.clone(),
                    package_revision: package.revision.clone(),
                    marketplace_revision: None,
                    installed: None,
                },
                package,
            ))
        }
        (Provider::Antigravity, action) => {
            exact_name(&request.target)?;
            let installed = installed_target(report, &request.target)?
                .context("Choose an installed Antigravity plugin")?;
            ensure!(
                installed.enabled == Some(!action.resulting_enabled()),
                "The installed plugin is already in the requested state or its state is unavailable"
            );
            let package = Package::open(&agy_installed_source(
                &cli.profile.configuration_root,
                installed,
            )?)?;
            Ok((
                Target {
                    plugin_id: installed.id.clone(),
                    version: installed
                        .version
                        .clone()
                        .context("The installed plugin version is unavailable")?,
                    marketplace: None,
                    source: package.root.clone(),
                    package_revision: package.revision.clone(),
                    marketplace_revision: None,
                    installed: Some(installed.clone()),
                },
                package,
            ))
        }
        _ => bail!("This CLI has no qualified native command for the selected operation"),
    }
}

impl Store {
    pub async fn native_codex_plugin_catalog(
        &self,
        marketplace: String,
    ) -> Result<Vec<CodexUserPluginCatalogEntry>> {
        exact_name(&marketplace)?;
        let configured = self.configured_executable(Provider::Codex).await?;
        tokio::task::spawn_blocking(move || {
            LocalCli::qualify(configured)?.codex_catalog(&marketplace)
        })
        .await?
    }

    pub async fn preview_native_user_plugin_action(
        &self,
        request: NativeUserPluginRequest,
    ) -> Result<NativeUserPluginPreview> {
        let _guard = MANAGE_PROFILE.lock().await;
        let configured = self.configured_executable(request.provider).await?;
        let (request, profile, revision, report, target) = tokio::task::spawn_blocking(move || {
            let cli = LocalCli::qualify(configured)?;
            cli.supports(request.action)?;
            let report = cli.installed()?;
            let (target, _) = evaluate(&cli, &request, &report)?;
            Ok::<_, anyhow::Error>((
                request,
                cli.current_profile()?,
                cli.configuration_revision()?,
                report,
                target,
            ))
        })
        .await??;
        let inventory_before = self.save_plugin_inventory(report).await?;
        let preview = NativeUserPluginPreview {
            id: Uuid::new_v4().to_string(),
            provider: request.provider,
            action: request.action,
            plugin_id: target.plugin_id.clone(),
            execution_scope: USER_SCOPE.into(),
            configuration_root: profile.configuration_root.clone(),
            executable: profile.executable.clone(),
            cli_version: profile.cli_version.clone(),
            persists: true,
            version: target.version.clone(),
            marketplace: target.marketplace.clone(),
            source: target.source.clone(),
            inventory_before,
        };
        let sealed = SealedNativePreview {
            preview: preview.clone(),
            request,
            profile,
            config_revision: revision,
            target,
        };
        ActionTable::Native
            .save(self, &preview.id, &preview.inventory_before.id, &sealed)
            .await?;
        Ok(preview)
    }

    pub async fn apply_native_user_plugin_action(
        &self,
        id: String,
    ) -> Result<ClaudeUserPluginOutcome> {
        let store = self.clone();
        tokio::spawn(async move {
            let _guard = MANAGE_PROFILE.lock().await;
            apply(&store, id).await
        })
        .await?
    }
}

async fn apply(store: &Store, id: String) -> Result<ClaudeUserPluginOutcome> {
    let claim = ActionTable::Native
        .claim::<SealedNativePreview>(store, &id)
        .await?;
    let sealed = match claim {
        Claim::Ready(sealed) => sealed,
        Claim::Recorded(outcome) => return Ok(outcome),
    };
    ensure!(
        sealed.preview.id == id,
        "The plugin preview does not match the recorded identity"
    );
    let configured = store.configured_executable(sealed.request.provider).await?;
    let changed = tokio::task::spawn_blocking(move || {
        apply_review(configured, *sealed).unwrap_or_else(|error| NativeOutcome {
            result: NativeResult::Stale(diagnostics::preview(&error.to_string())),
            report: None,
        })
    })
    .await?;
    let after = match changed.report {
        Some(report) => Some(store.save_plugin_inventory(report).await?),
        None => None,
    };
    let outcome = changed.result.with_inventory(after);
    ActionTable::Native.settle(store, id, &outcome).await?;
    Ok(outcome)
}

fn apply_review(
    configured: ConfiguredExecutable,
    sealed: SealedNativePreview,
) -> Result<NativeOutcome> {
    let cli = LocalCli::qualify(configured)?;
    ensure!(
        cli.profile == sealed.profile,
        "The CLI executable or configuration root changed"
    );
    cli.supports(sealed.request.action)?;
    let before = cli.installed()?;
    let (target, mut package) = evaluate(&cli, &sealed.request, &before)?;
    if target != sealed.target || cli.configuration_revision()? != sealed.config_revision {
        return Ok(NativeOutcome {
            result: NativeResult::Stale(
                "The plugin or local CLI configuration changed after review".into(),
            ),
            report: Some(before),
        });
    }

    let action = sealed.request.action;
    let source = if cli.provider == Provider::Antigravity && action == UserPluginAction::Install {
        Some(package.stage()?)
    } else {
        None
    };
    ensure!(
        cli.configuration_revision()? == sealed.config_revision,
        "The CLI configuration changed while preparing the reviewed action"
    );
    let operation = match cli.provider {
        Provider::Codex => cli.probe(
            &["plugin", "add", &target.plugin_id, "--json"],
            INSTALL_TIMEOUT,
        ),
        _ => {
            let argument = match &source {
                Some(stage) => stage
                    .path()
                    .to_str()
                    .context("The staged plugin path is unavailable")?,
                None => &target.plugin_id,
            };
            cli.probe(&["plugin", action.subcommand(), argument], INSTALL_TIMEOUT)
        }
    };
    let Ok(after) = cli.installed() else {
        return Ok(NativeOutcome {
            result: NativeResult::Unconfirmed(
                "The CLI did not return a refreshed inventory".into(),
            ),
            report: None,
        });
    };

    let refreshed = installed_target(&after, &target.plugin_id);
    let exited = operation
        .as_ref()
        .is_ok_and(|output| output.status == TerminalOutcome::Exited(0));
    let exact_outcome = refreshed.as_ref().is_ok_and(|plugin| {
        plugin.is_some_and(|plugin| {
            if plugin.version.as_deref() != Some(target.version.as_str()) {
                return false;
            }
            match (cli.provider, action) {
                (Provider::Codex, _) => {
                    plugin.enabled == Some(true)
                        && plugin.source.as_deref() == Some("local")
                        && plugin.source_location.as_deref().is_some_and(|source| {
                            dunce::canonicalize(source).is_ok_and(|path| path == target.source)
                        })
                        && operation
                            .as_ref()
                            .is_ok_and(|output| codex_install_receipt(&cli, &target, output))
                }
                (_, UserPluginAction::Install) => plugin.enabled.is_some(),
                _ => plugin.enabled == Some(action.resulting_enabled()),
            }
        })
    });
    let result = if exited && exact_outcome {
        NativeResult::Applied
    } else if operation
        .as_ref()
        .is_ok_and(|output| matches!(output.status, TerminalOutcome::Exited(code) if code != 0))
        && refreshed.is_ok_and(|plugin| plugin == target.installed.as_ref())
    {
        NativeResult::Failed(
            "The native plugin command failed and the selected state stayed the same".into(),
        )
    } else {
        NativeResult::Unconfirmed(
            "The native plugin update could not be confirmed against the reviewed target".into(),
        )
    };
    Ok(NativeOutcome {
        result,
        report: Some(after),
    })
}

fn codex_install_receipt(cli: &LocalCli, target: &Target, output: &CommandOutput) -> bool {
    let Ok(receipt) = serde_json::from_str::<Value>(&output.stdout) else {
        return false;
    };
    let Some(path) = receipt["installedPath"].as_str() else {
        return false;
    };
    receipt["pluginId"] == target.plugin_id
        && receipt["version"] == target.version
        && ordinary_root(Path::new(path)).is_ok_and(|path| {
            path.starts_with(cli.profile.configuration_root.join("plugins/cache"))
        })
        && Package::open(&target.source)
            .is_ok_and(|package| package.revision == target.package_revision)
}
