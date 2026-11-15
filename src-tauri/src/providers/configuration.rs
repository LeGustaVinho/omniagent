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
    FILE_FLAG_OPEN_REPARSE_POINT, GetFinalPathNameByHandleW,
};

use super::{
    Provider,
    capabilities::{ConfigurationEvidence, ConfigurationFile},
    skills::{SkillEntry, SkillSource},
};
use crate::{conversations::SessionScope, repository::CheckoutLayout};

mod instructions;
pub use instructions::{
    CodexInstructionSettings, InstructionInventory, InstructionScope, InstructionSelection,
    InstructionSource,
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
    let configuration = discovery.finish(checkout, reported.configuration_revision.as_deref())?;
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
    fn finish(
        mut self,
        checkout: &Path,
        reported_revision: Option<&str>,
    ) -> anyhow::Result<ConfigurationEvidence> {
        self.detected.sort_by(|a, b| a.path.cmp(&b.path));
        self.detected.dedup_by(|a, b| a.path == b.path);
        self.unresolved.sort();
        self.unresolved.dedup();
        let effective_revision = reported_revision
            .filter(|_| self.unresolved.is_empty())
            .map(|reported| {
                let mut sources = self
                    .detected
                    .iter()
                    .map(|file| {
                        (
                            file.path.strip_prefix(checkout).unwrap_or(&file.path),
                            &file.revision,
                        )
                    })
                    .collect::<Vec<_>>();
                sources.sort_unstable();
                super::capabilities::revision(&(reported, sources))
            })
            .transpose()?;
        Ok(ConfigurationEvidence {
            detected: self.detected,
            unresolved: self.unresolved,
            provider_revision: reported_revision.map(str::to_owned),
            effective_revision,
        })
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

fn read_local_file(path: &Path) -> io::Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
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
            "could not confirm configuration path",
        ));
    }
    let actual = PathBuf::from(OsString::from_wide(&final_name[..length]));
    if dunce::simplified(&actual) != dunce::simplified(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "configuration path changed",
        ));
    }
    let mut bytes = Vec::new();
    if file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)? as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration is too large",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let assess = |checkout: &Path, reported| {
            let mut discovery = Discovery::default();
            discovery.files(checkout, &["AGENTS.md"]);
            discovery.files(&profile, &["AGENTS.md"]);
            discovery.finish(checkout, reported)
        };
        let original = assess(&fixture, Some("reported"))?;
        let matching = assess(&task, Some("reported"))?;
        assert_eq!(original.detected.len(), 2, "{:?}", original.unresolved);
        assert!(original.effective_revision.is_some());
        assert_eq!(original.effective_revision, matching.effective_revision);
        fs::write(task.join("AGENTS.md"), b"changed rules")?;
        assert_ne!(
            original.effective_revision,
            assess(&task, Some("reported"))?.effective_revision
        );
        let passive = assess(&task, None)?;
        assert_eq!(passive.detected.len(), 2);
        assert!(passive.effective_revision.is_none());
        Ok(())
    }
}

fn plain_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
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
