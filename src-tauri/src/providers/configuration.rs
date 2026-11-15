use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Read},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, GetFinalPathNameByHandleW,
};

use super::{
    Provider,
    capabilities::{ConfigurationEvidence, ConfigurationFile},
    skills::{SkillEntry, SkillSource},
};
use crate::{conversations::SessionScope, repository::CheckoutLayout};

mod instructions;
mod qualification;
pub(crate) use instructions::normalize_path;
pub use instructions::{
    CodexInstructionSettings, InstructionInventory, InstructionScope, InstructionSelection,
    InstructionSource,
};
pub use qualification::ReportedConfiguration;
pub(crate) use qualification::{
    codex_configuration, mcp_identity, resolve_effective_configuration,
};

const REPARSE_POINT: u32 = 0x400;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SKILL_ENTRIES: usize = 512;

pub(crate) fn inspect(
    scope: &SessionScope,
    layout: &CheckoutLayout,
    reported: &super::capabilities::ReportedCapabilities,
) -> anyhow::Result<(ConfigurationEvidence, Vec<SkillEntry>, InstructionInventory)> {
    let provider = scope.provider;
    let checkout = &layout.root;
    let mut discovery = Discovery::default();
    let mut instructions = InstructionInventory::new(layout);
    let home = configured_directory("USERPROFILE");
    let directories = instructions::workspace_directories(layout);
    match provider {
        Provider::Codex => {
            let profile = profile_root("CODEX_HOME", home.as_deref(), ".codex");
            instructions.codex(
                &mut discovery,
                &directories,
                profile.as_deref(),
                reported.codex_instruction_settings.as_ref(),
            );
            for directory in &directories {
                discovery.skills(directory, ".agents/skills");
            }
            if let Some(profile) = &profile {
                discovery.files(profile, &["hooks.json"]);
                discovery.skills(profile, "skills");
            }
            if let Some(home) = &home {
                discovery.skills(home, ".agents/skills");
            }
        }
        Provider::Claude => {
            let profile = profile_root("CLAUDE_CONFIG_DIR", home.as_deref(), ".claude");
            let managed = configured_directory("ProgramFiles").map(|path| path.join("ClaudeCode"));
            instructions.claude(
                &mut discovery,
                profile.as_deref(),
                managed.as_deref(),
                &scope.cli_version,
            );
            for directory in layout.cwd.ancestors() {
                discovery.files(
                    directory,
                    &[
                        ".claude/settings.json",
                        ".claude/settings.local.json",
                        ".mcp.json",
                    ],
                );
            }
            for directory in &directories {
                discovery.skills(directory, ".claude/skills");
            }
            if layout.root != layout.primary_root
                && instructions::claude_supports_agents(&scope.cli_version)
                && matches!(
                    plain_metadata(&layout.root.join(".claude/skills")),
                    Ok(None)
                )
            {
                discovery.skills(&layout.primary_root, ".claude/skills");
            }
            if let Some(profile) = profile {
                discovery.files(
                    &profile,
                    &["settings.json", "plugins/installed_plugins.json"],
                );
                discovery.skills(&profile, "skills");
            }
            if let Some(home) = &home {
                discovery.files(home, &[".claude.json"]);
            }
            if let Some(managed) = managed {
                discovery.files(&managed, &["managed-settings.json", "managed-mcp.json"]);
            }
        }
        Provider::Antigravity => {
            let gemini = home.map(|home| home.join(".gemini"));
            instructions.antigravity(&mut discovery, &directories, gemini.as_deref());
            if let Some(gemini) = gemini {
                discovery.files(&gemini, &["settings.json"]);
                discovery.skills(&gemini, "antigravity-cli/skills");
                discovery.skills(&gemini, "skills");
            }
        }
    }
    discovery
        .unresolved
        .extend(instructions.unresolved.iter().cloned());
    let configuration = discovery.finish(reported.configuration_revision.as_deref());
    let skills = configuration
        .detected
        .iter()
        .filter_map(|file| {
            if !file.path.ends_with("SKILL.md") {
                return None;
            }
            let name = file.path.parent()?.file_name()?.to_str()?.to_owned();
            Some(SkillEntry::identity(provider, &file.path).map(|id| {
                SkillEntry {
                    id,
                    provider,
                    source: SkillSource::Filesystem,
                    name,
                    path: file.path.clone(),
                    scope: Some(
                        if file.path.starts_with(checkout) {
                            "repo"
                        } else if file.path.starts_with(&layout.primary_root) {
                            "primary_checkout"
                        } else {
                            "user"
                        }
                        .into(),
                    ),
                    plugin_id: None,
                    enabled: None,
                    revision: Some(file.revision.clone()),
                }
            }))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((configuration, skills, instructions))
}

pub(crate) fn claude_workspace_projection(
    layout: &CheckoutLayout,
    version: &str,
) -> anyhow::Result<(String, Vec<InstructionSource>)> {
    let mut inventory = InstructionInventory::new(layout);
    inventory.claude(&mut Discovery::default(), None, None, version);
    let belongs = |source: &InstructionSource| {
        source.scope == InstructionScope::Checkout
            && source.selection == InstructionSelection::DisabledByProfile
    };
    let has_claude = inventory
        .sources
        .iter()
        .filter(|source| belongs(source) && source.imported_from.is_none())
        .any(|source| {
            source.path.ends_with("CLAUDE.md") || source.path.ends_with("CLAUDE.local.md")
        });
    let sources: Vec<_> = inventory
        .sources
        .into_iter()
        .filter(|source| {
            belongs(source)
                && (!has_claude
                    || source.imported_from.is_some()
                    || !source.path.ends_with("AGENTS.md"))
        })
        .collect();
    project_sources(layout, sources)
}

pub(crate) fn codex_workspace_projection(
    layout: &CheckoutLayout,
) -> anyhow::Result<(String, Vec<InstructionSource>)> {
    let home = configured_directory("USERPROFILE");
    let profile = profile_root("CODEX_HOME", home.as_deref(), ".codex");
    let mut inventory = InstructionInventory::new(layout);
    inventory.codex(
        &mut Discovery::default(),
        &instructions::workspace_directories(layout),
        profile.as_deref(),
        None,
    );
    let sources = inventory
        .sources
        .into_iter()
        .filter(|source| {
            source.scope == InstructionScope::Checkout
                && source.selection == InstructionSelection::Expected
        })
        .collect();
    project_sources(layout, sources)
}

fn project_sources(
    layout: &CheckoutLayout,
    sources: Vec<InstructionSource>,
) -> anyhow::Result<(String, Vec<InstructionSource>)> {
    let mut instructions = String::new();
    for source in &sources {
        let bytes = read_local_file(&source.path)?;
        anyhow::ensure!(
            source.revision.as_deref() == Some(&format!("{:x}", Sha256::digest(&bytes))),
            "instruction changed while preparing the launch"
        );
        use std::fmt::Write;
        let logical_path = source
            .path
            .strip_prefix(&layout.root)
            .or_else(|_| source.path.strip_prefix(&layout.primary_root))
            .unwrap_or(&source.path);
        writeln!(
            instructions,
            "\n# Repository instructions from {}\n",
            logical_path.display()
        )?;
        let text = String::from_utf8(bytes)?;
        let length = source.included_bytes.unwrap_or(text.len()).min(text.len());
        instructions.push_str(&text[..text.floor_char_boundary(length)]);
        instructions.push('\n');
    }
    Ok((instructions, sources))
}

fn profile_root(variable: &str, home: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    configured_directory(variable).or_else(|| home.map(|home| home.join(fallback)))
}

fn configured_directory(variable: &str) -> Option<PathBuf> {
    let path = env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)?;
    // Configured roots can use Windows short names or different casing. Keep
    // the original path if it cannot be inspected so the scan reports the gap.
    if plain_metadata(&path)
        .ok()
        .flatten()
        .is_some_and(|metadata| metadata.is_dir())
    {
        return Some(dunce::canonicalize(&path).unwrap_or(path));
    }
    Some(path)
}

#[derive(Default)]
struct Discovery {
    detected: Vec<ConfigurationFile>,
    unresolved: Vec<PathBuf>,
}

impl Discovery {
    fn finish(mut self, reported_revision: Option<&str>) -> ConfigurationEvidence {
        self.detected.sort_by(|a, b| a.path.cmp(&b.path));
        self.detected.dedup_by(|a, b| a.path == b.path);
        self.unresolved.sort();
        self.unresolved.dedup();
        ConfigurationEvidence {
            detected: self.detected,
            unresolved: self.unresolved,
            provider_revision: reported_revision.map(str::to_owned),
            effective_revision: None,
            attachment_revision: None,
            environment_revision: None,
            tool_revisions: Default::default(),
        }
    }

    fn files(&mut self, root: &Path, names: &[&str]) {
        for name in names {
            self.file(root.join(name));
        }
    }

    fn file(&mut self, path: PathBuf) {
        self.read(&path);
    }

    fn read(&mut self, path: &Path) -> Option<(Vec<u8>, String)> {
        match plain_metadata(&path) {
            Ok(Some(metadata)) if metadata.is_file() && metadata.len() <= MAX_FILE_BYTES => {
                match read_local_file(path) {
                    Ok(bytes) => {
                        let revision = format!("{:x}", Sha256::digest(&bytes));
                        self.detected.push(ConfigurationFile {
                            path: path.to_path_buf(),
                            revision: revision.clone(),
                            environment_revision: None,
                        });
                        return Some((bytes, revision));
                    }
                    Err(_) => self.unresolved.push(path.to_path_buf()),
                }
            }
            Ok(None) => {}
            _ => self.unresolved.push(path.to_path_buf()),
        }
        None
    }

    fn skills(&mut self, root: &Path, relative: &str) {
        let directory = root.join(relative);
        match plain_metadata(&directory) {
            Ok(None) => return,
            Ok(Some(metadata)) if metadata.is_dir() => {}
            _ => {
                self.unresolved.push(directory);
                return;
            }
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            self.unresolved.push(directory);
            return;
        };
        for (index, entry) in entries.take(MAX_SKILL_ENTRIES + 1).enumerate() {
            if index == MAX_SKILL_ENTRIES {
                self.unresolved.push(directory);
                break;
            }
            match entry {
                Ok(entry) => self.file(entry.path().join("SKILL.md")),
                Err(_) => self.unresolved.push(directory.clone()),
            }
        }
    }

    fn antigravity_settings(&mut self, path: &Path) {
        self.files(
            path,
            &[
                "agents.json",
                "plugins.json",
                "hooks.json",
                "mcp_config.json",
                "workbench.json",
            ],
        );
    }
}

pub(crate) fn read_local_file(path: &Path) -> io::Result<Vec<u8>> {
    let file = open_verified_local_file(path)?;
    let mut bytes = Vec::new();
    if file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)? as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration is too large",
        ));
    }
    Ok(bytes)
}

pub(crate) fn open_verified_local_file(path: &Path) -> io::Result<fs::File> {
    open_local_file(path, &mut fs::OpenOptions::new())
}

pub(crate) fn open_locked_local_file(path: &Path) -> io::Result<fs::File> {
    open_local_file(path, fs::OpenOptions::new().share_mode(FILE_SHARE_READ))
}

fn open_local_file(path: &Path, options: &mut fs::OpenOptions) -> io::Result<fs::File> {
    let file = options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a local file",
        ));
    }
    // Check the opened handle as an ancestor could have changed after discovery.
    let mut final_name = vec![0u16; path.as_os_str().encode_wide().count() + 8];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            final_name.as_mut_ptr(),
            final_name.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= final_name.len() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "could not confirm local file path",
        ));
    }
    let actual = PathBuf::from(OsString::from_wide(&final_name[..length]));
    if dunce::simplified(&actual) != dunce::simplified(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local file path changed",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{
        launch_scope::ScopedLaunchEvidence,
        plugins::{PluginInventory, PluginInventoryStatus, PluginReport},
        skills::SkillCatalog,
    };
    use serde_json::json;

    #[test]
    fn effective_revisions_follow_relative_scope_and_file_changes() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dunce::canonicalize(dir.path())?;
        let fixture = root.join("a-fixture");
        let profile = root.join("b-profile");
        let task = root.join("c-task");
        for directory in [&fixture, &profile, &task] {
            fs::create_dir(directory)?;
            fs::write(directory.join("AGENTS.md"), b"project rules")?;
        }
        let layout = |checkout: &Path| CheckoutLayout {
            root: checkout.into(),
            primary_root: checkout.into(),
            cwd: checkout.into(),
        };
        let launch = ScopedLaunchEvidence {
            provider: Provider::Codex,
            strategy: "codex_untrusted_project".into(),
            approved: Default::default(),
            excluded: Default::default(),
            instruction_sources: Vec::new(),
            inputs: Vec::new(),
            mcp_sources: [(mcp_identity("reader"), "installed executable digest".into())].into(),
            non_mcp_approvals: Default::default(),
        };
        let plugins = PluginInventory {
            id: "fixture".into(),
            checked_at: "fixture".into(),
            report: PluginReport {
                provider: Provider::Codex,
                executable: None,
                version: None,
                status: PluginInventoryStatus::Reported,
                plugins: Vec::new(),
                load_telemetry_reported: false,
            },
        };
        let skills = SkillCatalog {
            entries: Vec::new(),
            issues: Vec::new(),
            reported: true,
        };
        let assess = |checkout: &Path, arguments: &str, use_report: bool| -> anyhow::Result<_> {
            let config = json!({ "model": "fixture", "projects": { (checkout.to_str().unwrap()): {"trust_level":"untrusted"} },
                "mcp_servers": { "reader": { "command": checkout.join("reader.exe"), "args": [arguments], "cwd": checkout } }});
            let reported =
                codex_configuration(config.as_object().unwrap().clone(), checkout, None)?;
            let mut discovery = Discovery::default();
            discovery.files(checkout, &["AGENTS.md"]);
            discovery.files(&profile, &["AGENTS.md"]);
            let mut evidence = discovery.finish(use_report.then_some("reported"));
            resolve_effective_configuration(
                &mut evidence,
                &layout(checkout),
                use_report.then_some(&reported),
                &launch,
                Some(&plugins),
                Some(&skills),
            )?;
            Ok(evidence)
        };
        let original = assess(&fixture, "--first", true)?;
        let matching = assess(&task, "--first", true)?;
        assert_eq!(original.detected.len(), 2, "{:?}", original.unresolved);
        assert!(original.effective_revision.is_some());
        assert_eq!(original.effective_revision, matching.effective_revision);
        let tool_changed = assess(&task, "--second", true)?;
        assert_eq!(
            original.environment_revision,
            tool_changed.environment_revision
        );
        assert_ne!(original.tool_revisions, tool_changed.tool_revisions);
        assert_ne!(original.effective_revision, tool_changed.effective_revision);
        fs::write(task.join("AGENTS.md"), b"changed rules")?;
        assert_ne!(
            original.effective_revision,
            assess(&task, "--first", true)?.effective_revision
        );
        let passive = assess(&task, "--first", false)?;
        assert_eq!(passive.detected.len(), 2);
        assert!(passive.effective_revision.is_none());
        Ok(())
    }
}

pub(crate) fn plain_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
    // Do not follow a checkout junction into user files or a network share.
    use std::path::{Component, Prefix};
    if !path.is_absolute()
        || !matches!(
            path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configuration path is not local",
        ));
    }
    let mut current = PathBuf::new();
    let mut last = None;
    for component in path.components() {
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.file_attributes() & REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "configuration path is a reparse point",
            ));
        }
        last = Some(metadata);
    }
    Ok(last)
}
