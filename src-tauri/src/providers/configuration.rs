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
};

const REPARSE_POINT: u32 = 0x400;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SKILL_ENTRIES: usize = 512;

pub(crate) fn inspect(
    provider: Provider,
    checkout: &Path,
    reported_revision: Option<&str>,
) -> anyhow::Result<ConfigurationEvidence> {
    let mut discovery = Discovery::default();
    let home = env::var_os("USERPROFILE").map(PathBuf::from);
    match provider {
        Provider::Codex => {
            discovery.files(
                checkout,
                &[
                    "AGENTS.override.md",
                    "AGENTS.md",
                    "CODEX.md",
                    ".codex/config.toml",
                ],
            );
            discovery.skills(checkout, ".agents/skills");
            if let Some(profile) = profile_root("CODEX_HOME", home.as_deref(), ".codex") {
                discovery.files(
                    &profile,
                    &[
                        "config.toml",
                        "AGENTS.override.md",
                        "AGENTS.md",
                        "hooks.json",
                    ],
                );
                discovery.skills(&profile, "skills");
            }
            if let Some(home) = &home {
                discovery.skills(home, ".agents/skills");
            }
        }
        Provider::Claude => {
            for directory in checkout.ancestors() {
                discovery.files(
                    directory,
                    &[
                        "CLAUDE.md",
                        "CLAUDE.local.md",
                        ".claude/CLAUDE.md",
                        "AGENTS.md",
                        ".claude/AGENTS.md",
                        ".claude/settings.json",
                        ".claude/settings.local.json",
                        ".mcp.json",
                    ],
                );
            }
            discovery.skills(checkout, ".claude/skills");
            if let Some(profile) = profile_root("CLAUDE_CONFIG_DIR", home.as_deref(), ".claude") {
                discovery.files(
                    &profile,
                    &[
                        "settings.json",
                        "CLAUDE.md",
                        "plugins/installed_plugins.json",
                    ],
                );
                discovery.skills(&profile, "skills");
            }
            if let Some(home) = &home {
                discovery.files(home, &[".claude.json"]);
            }
            if let Some(program_files) = env::var_os("ProgramFiles").map(PathBuf::from) {
                discovery.files(
                    &program_files,
                    &[
                        "ClaudeCode/managed-settings.json",
                        "ClaudeCode/managed-mcp.json",
                        "ClaudeCode/CLAUDE.md",
                    ],
                );
            }
        }
        Provider::Antigravity => {
            discovery.files(checkout, &["AGENTS.md", "GEMINI.md"]);
            for directory in [".agents", ".agent", "_agents", "_agent"] {
                discovery.antigravity_directory(&checkout.join(directory));
            }
            if let Some(home) = &home {
                let gemini = home.join(".gemini");
                discovery.files(&gemini, &["AGENTS.md", "GEMINI.md", "settings.json"]);
                discovery.antigravity_directory(&gemini.join("config"));
                discovery.skills(&gemini, "antigravity-cli/skills");
                discovery.skills(&gemini, "skills");
            }
        }
    }
    discovery.finish(checkout, reported_revision)
}

fn profile_root(variable: &str, home: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(fallback)))
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
        match plain_metadata(&path) {
            Ok(Some(metadata)) if metadata.is_file() && metadata.len() <= MAX_FILE_BYTES => {
                match local_file_revision(&path) {
                    Ok(revision) => self.detected.push(ConfigurationFile { path, revision }),
                    Err(_) => self.unresolved.push(path),
                }
            }
            Ok(None) => {}
            _ => self.unresolved.push(path),
        }
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

    fn antigravity_directory(&mut self, path: &Path) {
        self.files(
            path,
            &[
                "AGENTS.md",
                "GEMINI.md",
                "skills.json",
                "rules.json",
                "agents.json",
                "plugins.json",
                "hooks.json",
                "mcp_config.json",
                "workbench.json",
            ],
        );
        self.skills(path, "skills");
    }
}

fn local_file_revision(path: &Path) -> io::Result<String> {
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
    let mut hash = Sha256::new();
    if io::copy(&mut file.take(MAX_FILE_BYTES + 1), &mut hash)? > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "configuration is too large",
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_revisions_follow_relative_scope_and_file_changes() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let fixture = dir.path().join("a-fixture");
        let profile = dir.path().join("b-profile");
        let task = dir.path().join("c-task");
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
        assert_eq!(original.detected.len(), 2);
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
