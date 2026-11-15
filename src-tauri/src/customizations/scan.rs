use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::Url;

use crate::{
    diagnostics,
    providers::{
        Provider,
        capabilities::revision,
        configuration::{normalize_path, plain_metadata, read_local_file},
    },
};

use super::{
    CustomizationEntry, CustomizationInventory, CustomizationKind, MarkdownInstruction,
    RawDeclaration, ReviewState, ScannedFile,
};

const TEXT_BUDGET: usize = 4 * 1024 * 1024;
const DIRECTORY_LIMIT: usize = 128;

pub(super) fn inspect(
    root: &Path,
    cwd: &Path,
    capture_commands: bool,
) -> Result<(CustomizationInventory, Vec<RawDeclaration>)> {
    ensure!(
        cwd.starts_with(root),
        "customization directory is outside the checkout"
    );
    let mut scan = Scan {
        root,
        cwd,
        text_left: TEXT_BUDGET,
        commands: capture_commands.then(Vec::new),
        inventory: CustomizationInventory {
            working_directory: path_string(cwd)?,
            instructions: Vec::new(),
            entries: Vec::new(),
            unresolved_sources: Vec::new(),
        },
    };
    for directory in cwd.ancestors().take_while(|path| path.starts_with(root)) {
        scan.directory(directory)?;
    }
    scan.inventory
        .instructions
        .sort_by(|a, b| a.file.path.cmp(&b.file.path));
    scan.inventory
        .instructions
        .dedup_by(|a, b| a.file.path == b.file.path);
    scan.inventory
        .entries
        .sort_by(|a, b| (&a.source, &a.label, &a.id).cmp(&(&b.source, &b.label, &b.id)));
    scan.inventory.unresolved_sources.sort();
    scan.inventory.unresolved_sources.dedup();
    Ok((scan.inventory, scan.commands.unwrap_or_default()))
}

struct Scan<'a> {
    root: &'a Path,
    cwd: &'a Path,
    text_left: usize,
    commands: Option<Vec<RawDeclaration>>,
    inventory: CustomizationInventory,
}

struct Declaration<'a> {
    file: &'a ScannedFile,
    provider: Provider,
    kind: CustomizationKind,
    key: String,
    label: String,
    scope: String,
    body: &'a Value,
    target: Option<&'a str>,
    hook_event: Option<&'a str>,
    arguments: Vec<String>,
    shell: bool,
    enabled: bool,
}

impl Scan<'_> {
    fn file(&mut self, path: &Path) -> Result<Option<(ScannedFile, Vec<u8>)>> {
        let bytes = match plain_metadata(path) {
            Ok(None) => return Ok(None),
            Ok(Some(metadata)) if metadata.is_file() => read_local_file(path),
            Ok(Some(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            )),
            Err(error) => Err(error),
        };
        let label = path_string(path.strip_prefix(self.root)?)?;
        match bytes {
            Ok(bytes) => {
                let revision = bytes_revision(&bytes);
                Ok(Some((
                    ScannedFile {
                        path: label,
                        revision,
                    },
                    bytes,
                )))
            }
            Err(_) => {
                self.inventory.unresolved_sources.push(label);
                Ok(None)
            }
        }
    }

    fn markdown(&mut self, path: &Path) -> Result<()> {
        let Some((file, bytes)) = self.file(path)? else {
            return Ok(());
        };
        if bytes.len() > self.text_left {
            self.inventory.unresolved_sources.push(file.path);
            return Ok(());
        }
        match String::from_utf8(bytes) {
            Ok(text) => {
                self.text_left -= text.len();
                self.inventory
                    .instructions
                    .push(MarkdownInstruction { file, text });
            }
            Err(_) => self.inventory.unresolved_sources.push(file.path),
        }
        Ok(())
    }

    fn directory(&mut self, directory: &Path) -> Result<()> {
        for name in [
            "AGENTS.override.md",
            "AGENTS.md",
            "CODEX.md",
            "CLAUDE.md",
            "CLAUDE.local.md",
            ".claude/CLAUDE.md",
            ".claude/AGENTS.md",
            "GEMINI.md",
        ] {
            self.markdown(&directory.join(name))?;
        }
        self.configuration(Provider::Codex, &directory.join(".codex/config.toml"))?;
        self.configuration(Provider::Codex, &directory.join(".codex/hooks.json"))?;
        for name in [
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".mcp.json",
        ] {
            self.configuration(Provider::Claude, &directory.join(name))?;
        }
        self.claude_skills(&directory.join(".claude/skills"))?;
        if let Some((file, contents)) = self.file(&directory.join(".gemini/config.json"))? {
            let includes_personal_directory = serde_json::from_slice::<Value>(&contents)
                .ok()
                .and_then(|data| {
                    data.as_object()
                        .map(|fields| fields.contains_key("personal_customization_dir"))
                });
            if includes_personal_directory != Some(false) {
                self.inventory.unresolved_sources.push(file.path);
            }
        }
        for folder in [".agents", ".agent", "_agents", "_agent"] {
            let path = directory.join(folder);
            for name in ["AGENTS.md", "GEMINI.md"] {
                self.markdown(&path.join(name))?;
            }
            for source in self.children(&path.join("rules"))? {
                if source
                    .extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case("md"))
                {
                    self.markdown(&source)?;
                }
            }
            for name in ["hooks.json", "mcp_config.json", "plugins.json"] {
                self.configuration(Provider::Antigravity, &path.join(name))?;
            }
            for plugin in self.children(&path.join("plugins"))? {
                for name in [
                    "hooks.json",
                    "mcp_config.json",
                    "plugin.json",
                    ".antigravity-plugin/plugin.json",
                    ".gemini-plugin/plugin.json",
                ] {
                    self.configuration(Provider::Antigravity, &plugin.join(name))?;
                }
            }
        }
        Ok(())
    }

    fn children(&mut self, path: &Path) -> Result<Vec<PathBuf>> {
        let present = plain_metadata(path);
        if matches!(present, Ok(None)) {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        if present
            .ok()
            .flatten()
            .is_some_and(|metadata| metadata.is_dir())
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.take(DIRECTORY_LIMIT + 1) {
                if paths.len() == DIRECTORY_LIMIT || entry.is_err() {
                    self.inventory
                        .unresolved_sources
                        .push(path_string(path.strip_prefix(self.root)?)?);
                    break;
                }
                paths.push(entry?.path());
            }
        } else {
            self.inventory
                .unresolved_sources
                .push(path_string(path.strip_prefix(self.root)?)?);
        }
        paths.sort();
        Ok(paths)
    }

    fn configuration(&mut self, provider: Provider, path: &Path) -> Result<()> {
        let Some((file, bytes)) = self.file(path)? else {
            return Ok(());
        };
        let root = if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(text).ok())
                .and_then(|value| serde_json::to_value(value).ok())
        } else {
            serde_json::from_slice::<Value>(&bytes).ok()
        };
        let Some(mut root) = root.filter(Value::is_object) else {
            self.inventory.unresolved_sources.push(file.path);
            return Ok(());
        };
        root.sort_all_objects();

        if path.file_name().is_some_and(|name| name == "hooks.json") && root.get("hooks").is_none()
        {
            if provider == Provider::Antigravity {
                for (name, group) in root.as_object().expect("object") {
                    self.hooks(provider, &file, group, name, group["enabled"] != false)?;
                }
            } else {
                self.hooks(provider, &file, &root, "", true)?;
            }
        } else if let Some(hooks) = root.get("hooks") {
            if hooks.is_object() {
                self.hooks(provider, &file, hooks, "", root["enabled"] != false)?;
            } else {
                self.reference(
                    provider,
                    &file,
                    CustomizationKind::Plugin,
                    "hooks",
                    hooks,
                    true,
                )?;
            }
        }
        for key in ["mcpServers", "mcp_servers"] {
            if let Some(servers) = root[key].as_object() {
                for (name, server) in servers {
                    self.mcp(&file, provider, key, name, server)?;
                }
            } else if let Some(servers) = root.get(key) {
                self.reference(
                    provider,
                    &file,
                    CustomizationKind::Plugin,
                    key,
                    servers,
                    true,
                )?;
            }
        }
        for key in ["enabledPlugins", "plugins"] {
            if let Some(plugins) = root[key].as_object() {
                for (name, item) in plugins {
                    self.reference(
                        provider,
                        &file,
                        CustomizationKind::Plugin,
                        &format!("{key}/{name}"),
                        item,
                        item != &Value::Bool(false) && item["enabled"] != false,
                    )?;
                }
            }
        }
        if path.file_name().is_some_and(|name| name == "plugins.json")
            && let Some(entries) = root["entries"].as_array()
        {
            for (index, entry) in entries.iter().enumerate() {
                self.reference(
                    provider,
                    &file,
                    CustomizationKind::Plugin,
                    &format!("Plugin manifest entry {}", index + 1),
                    entry,
                    true,
                )?;
            }
        }
        if provider == Provider::Antigravity
            && path.file_name().is_some_and(|name| name == "plugins.json")
            && root["inherits"]
                .as_array()
                .is_some_and(|manifests| !manifests.is_empty())
        {
            self.inventory.unresolved_sources.push(file.path.clone());
        }
        if provider == Provider::Claude {
            for key in [
                "statusLine",
                "fileSuggestion",
                "apiKeyHelper",
                "awsAuthRefresh",
                "awsCredentialExport",
                "otelHeadersHelper",
            ] {
                if let Some(body) = root.get(key) {
                    self.add(Declaration {
                        file: &file,
                        provider,
                        kind: CustomizationKind::Helper,
                        key: key.into(),
                        label: key.into(),
                        scope: "Claude startup or UI helper".into(),
                        body,
                        target: body.as_str().or_else(|| body["command"].as_str()),
                        hook_event: None,
                        arguments: Vec::new(),
                        shell: true,
                        enabled: true,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn hooks(
        &mut self,
        provider: Provider,
        file: &ScannedFile,
        root: &Value,
        name: &str,
        group_enabled: bool,
    ) -> Result<()> {
        let Some(events) = root.as_object() else {
            self.inventory.unresolved_sources.push(file.path.clone());
            return Ok(());
        };
        for (event, matchers) in events {
            if matches!(
                event.as_str(),
                "enabled" | "description" | "version" | "$schema"
            ) {
                continue;
            }
            let Some(matchers) = matchers.as_array() else {
                self.inventory.unresolved_sources.push(file.path.clone());
                continue;
            };
            for (index, matcher) in matchers.iter().enumerate() {
                let Some(handlers) = matcher["hooks"].as_array() else {
                    self.inventory.unresolved_sources.push(file.path.clone());
                    continue;
                };
                for (handler_index, handler) in handlers.iter().enumerate() {
                    let filter = matcher["matcher"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("*");
                    let body = json!({"matcher": matcher["matcher"], "handler": handler});
                    self.add(Declaration {
                        file,
                        provider,
                        kind: CustomizationKind::Hook,
                        key: format!("{name}/{event}/{index}/{handler_index}"),
                        label: if name.is_empty() {
                            event.to_owned()
                        } else {
                            format!("{name} · {event}")
                        },
                        scope: format!("{event} ({filter})"),
                        body: &body,
                        target: handler["command"]
                            .as_str()
                            .or_else(|| handler["url"].as_str()),
                        hook_event: Some(event),
                        arguments: strings(&handler["args"]),
                        shell: handler["command"].is_string(),
                        enabled: group_enabled
                            && matcher["enabled"] != false
                            && handler["enabled"] != false,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn mcp(
        &mut self,
        file: &ScannedFile,
        provider: Provider,
        table: &str,
        name: &str,
        body: &Value,
    ) -> Result<()> {
        let target = body["command"]
            .as_str()
            .or_else(|| body["url"].as_str())
            .or_else(|| body["httpUrl"].as_str());
        let scope = body["cwd"].as_str().map_or_else(
            || "Available to the CLI session".to_owned(),
            |cwd| format!("CLI session · {}", diagnostics::preview(cwd)),
        );
        self.add(Declaration {
            file,
            provider,
            kind: CustomizationKind::McpServer,
            key: format!("{table}/{name}"),
            label: name.into(),
            scope,
            body,
            target,
            hook_event: None,
            arguments: strings(&body["args"]),
            shell: false,
            enabled: body["enabled"] != false,
        })
    }

    fn reference(
        &mut self,
        provider: Provider,
        file: &ScannedFile,
        kind: CustomizationKind,
        name: &str,
        body: &Value,
        enabled: bool,
    ) -> Result<()> {
        self.add(Declaration {
            file,
            provider,
            kind,
            key: name.into(),
            label: name.into(),
            scope: "Plugin components require resolved sources".into(),
            body,
            target: None,
            hook_event: None,
            arguments: Vec::new(),
            shell: false,
            enabled,
        })
    }

    fn claude_skills(&mut self, directory: &Path) -> Result<()> {
        static COMMAND: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"!`([^`\r\n]+)`").expect("skill command pattern"));
        for skill in self.children(directory)? {
            let path = skill.join("SKILL.md");
            let Some((file, bytes)) = self.file(&path)? else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                self.inventory.unresolved_sources.push(file.path);
                continue;
            };
            for (index, capture) in COMMAND.captures_iter(text).enumerate() {
                let command = &capture[1];
                let body = json!({"command": command, "skillRevision": &file.revision});
                self.add(Declaration {
                    file: &file,
                    provider: Provider::Claude,
                    kind: CustomizationKind::SkillCommand,
                    key: index.to_string(),
                    label: format!("Skill command {}", index + 1),
                    scope: "On skill expansion".into(),
                    body: &body,
                    target: Some(command),
                    hook_event: None,
                    arguments: Vec::new(),
                    shell: true,
                    enabled: true,
                })?;
            }
        }
        Ok(())
    }

    fn add(&mut self, declaration: Declaration<'_>) -> Result<()> {
        let Declaration {
            file,
            provider,
            kind,
            key,
            label,
            scope,
            body,
            target,
            hook_event,
            arguments,
            shell,
            enabled,
        } = declaration;
        let id = revision(&(provider, kind, &file.path, key))?;
        let (target, arguments, local_files, issue) =
            self.resolve_target(target, arguments, shell, body)?;
        let cwd = self.cwd.strip_prefix(self.root)?;
        let fingerprint = revision(&(&id, body, &scope, cwd, &target, &local_files, enabled))?;
        let review = if !enabled {
            ReviewState::Disabled
        } else if issue.is_some() {
            ReviewState::Unresolved
        } else {
            ReviewState::Pending
        };
        if review == ReviewState::Pending
            && let Some(commands) = &mut self.commands
        {
            commands.push(RawDeclaration {
                id: id.clone(),
                name: label.clone(),
                event: hook_event.map(str::to_owned),
                body: body.clone(),
            });
        }
        self.inventory.entries.push(CustomizationEntry {
            id,
            provider,
            kind,
            source: file.path.clone(),
            source_revision: file.revision.clone(),
            label: diagnostics::preview(&label),
            target,
            arguments,
            requested_scope: diagnostics::preview(&scope),
            local_files,
            fingerprint,
            review,
            issue,
        });
        Ok(())
    }

    fn resolve_target(
        &mut self,
        target: Option<&str>,
        mut args: Vec<String>,
        shell: bool,
        body: &Value,
    ) -> Result<(
        Option<String>,
        Vec<String>,
        Vec<ScannedFile>,
        Option<String>,
    )> {
        let Some(target) = target.filter(|target| !target.trim().is_empty()) else {
            return Ok((
                None,
                Vec::new(),
                Vec::new(),
                Some("No executable or endpoint could be resolved.".into()),
            ));
        };
        if let Ok(mut url) = Url::parse(target)
            && matches!(url.scheme(), "http" | "https")
        {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            return Ok((Some(url.to_string()), Vec::new(), Vec::new(), None));
        }
        let mut issue = None;
        let executable = if shell {
            let tokens = shell_tokens(target);
            if target.contains(['|', '&', '<', '>', ';', '`', '\r', '\n']) || target.contains("$(")
            {
                issue = Some(
                    "The shell expression needs a statically resolved executable entry.".into(),
                );
            }
            args = tokens.iter().skip(1).cloned().chain(args).collect();
            tokens.into_iter().next().unwrap_or_default()
        } else {
            target.to_owned()
        };
        let working = body["cwd"]
            .as_str()
            .and_then(|path| normalize_path(&self.cwd.join(path)));
        if body["cwd"].is_string()
            && working
                .as_ref()
                .is_none_or(|cwd| !cwd.starts_with(self.root))
        {
            issue.get_or_insert_with(|| {
                "The declared working directory is outside the checkout.".into()
            });
        }
        let working = working.unwrap_or_else(|| self.cwd.to_path_buf());
        let mut files = BTreeMap::new();
        let mut display = executable.clone();
        if expand_checkout(&executable, self.root).contains(['$', '%']) {
            issue.get_or_insert_with(|| {
                "The executable relies on an unresolved environment variable.".into()
            });
        }
        if let Some(args) = body.get("args")
            && !args
                .as_array()
                .is_some_and(|args| args.iter().all(Value::is_string))
        {
            issue.get_or_insert_with(|| "The argument list is invalid.".into());
        }
        for (index, token) in std::iter::once(&executable).chain(&args).enumerate() {
            let expanded = expand_checkout(token, self.root);
            let looks_like_path = executable_path(&expanded, index == 0)
                || (index == 0
                    && working.starts_with(self.root)
                    && plain_metadata(&working.join(&expanded))
                        .ok()
                        .flatten()
                        .is_some_and(|file| file.is_file()));
            if !looks_like_path {
                continue;
            }
            let candidate = normalize_path(&working.join(&expanded));
            let Some(path) = candidate.filter(|path| path.starts_with(self.root)) else {
                issue.get_or_insert_with(|| {
                    "A referenced executable path is outside the checkout.".into()
                });
                continue;
            };
            if let Some((file, _)) = self.file(&path)? {
                if index == 0 {
                    display = path_string(&path)?;
                }
                files.insert(file.path.clone(), file);
            } else if index == 0 || is_script_path(Path::new(&expanded)) {
                issue.get_or_insert_with(|| {
                    "A referenced script or executable could not be read.".into()
                });
            }
        }
        if display == executable
            && !executable_path(&executable, true)
            && let Some(path) = discover_executable(&executable)
        {
            display = path_string(&path)?;
        }
        if executable.is_empty() {
            issue.get_or_insert_with(|| "No executable was declared.".into());
        }
        Ok((
            Some(diagnostics::preview(&display)),
            args.iter().map(|arg| diagnostics::preview(arg)).collect(),
            files.into_values().collect(),
            issue,
        ))
    }
}

fn path_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .context("customization path is not UTF-8")
}

fn bytes_revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn shell_tokens(command: &str) -> Vec<String> {
    static TOKEN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"[^\s"']+|"[^"]*"|'[^']*'"#).expect("shell token pattern"));
    let mut cursor = 0;
    let mut tokens: Vec<String> = Vec::new();
    for item in TOKEN.find_iter(command) {
        let part = item.as_str().trim_matches(['"', '\'']);
        if item.start() == cursor
            && let Some(previous) = tokens.last_mut()
        {
            previous.push_str(part);
        } else {
            tokens.push(part.to_owned());
        }
        cursor = item.end();
    }
    tokens
}

fn expand_checkout(token: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    [
        "${CLAUDE_PROJECT_DIR}",
        "$CLAUDE_PROJECT_DIR",
        "%CLAUDE_PROJECT_DIR%",
        "${workspaceFolder}",
    ]
    .into_iter()
    .fold(token.to_owned(), |token, variable| {
        token.replace(variable, &root)
    })
}

fn is_script_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|extension| {
            [
                "exe", "cmd", "bat", "ps1", "sh", "py", "js", "cjs", "mjs", "ts", "rb", "jar",
                "dll", "wasm",
            ]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn executable_path(token: &str, executable: bool) -> bool {
    let path = Path::new(token);
    token.contains(['/', '\\'])
        || path.is_absolute()
        || (!executable && !token.starts_with('-') && is_script_path(path))
}

fn discover_executable(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| [directory.join(name), directory.join(format!("{name}.exe"))])
        .find(|candidate| {
            plain_metadata(candidate)
                .ok()
                .flatten()
                .is_some_and(|metadata| metadata.is_file())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_codex_and_antigravity_declaration_shapes_as_data() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = dunce::canonicalize(directory.path())?;
        fs::create_dir(root.join(".codex"))?;
        fs::create_dir(root.join(".agents"))?;
        fs::write(root.join("launch.ps1"), "Set-Content marker started")?;
        fs::write(
            root.join(".codex/config.toml"),
            r#"
[[hooks.SessionStart]]
matcher = "startup"
[[hooks.SessionStart.hooks]]
type = "command"
command = "powershell.exe -NoProfile -File ./launch.ps1"
[mcp_servers.repo]
command = "node"
args = ["./server.js"]
"#,
        )?;
        fs::write(
            root.join("server.js"),
            "throw Error('scanner executed repository code')",
        )?;
        fs::write(
            root.join(".agents/hooks.json"),
            r#"{
            "local": {"enabled":false,"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"./launch.ps1"}]}]}
        }"#,
        )?;
        fs::write(
            root.join(".agents/mcp_config.json"),
            r#"{"mcpServers":{"remote":{
            "httpUrl":"https://user:password@example.invalid/mcp?secret=token"
        }}}"#,
        )?;
        let (result, _) = inspect(&root, &root, false)?;
        assert!(result.unresolved_sources.is_empty());
        assert_eq!(result.entries.len(), 4);
        let find = |provider, kind| {
            result
                .entries
                .iter()
                .find(|entry| entry.provider == provider && entry.kind == kind)
                .unwrap()
        };
        let codex = find(Provider::Codex, CustomizationKind::Hook);
        assert_eq!(codex.review, ReviewState::Pending);
        assert_eq!(codex.local_files[0].path, "launch.ps1");
        assert_eq!(
            find(Provider::Codex, CustomizationKind::McpServer).local_files[0].path,
            "server.js"
        );
        assert_eq!(
            find(Provider::Antigravity, CustomizationKind::Hook).review,
            ReviewState::Disabled
        );
        assert_eq!(
            find(Provider::Antigravity, CustomizationKind::McpServer)
                .target
                .as_deref(),
            Some("https://example.invalid/mcp")
        );
        assert!(!root.join("marker").exists());
        Ok(())
    }
}
