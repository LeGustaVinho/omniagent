use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::repository::CheckoutLayout;

use super::{Discovery, MAX_SKILL_ENTRIES, plain_metadata};

const CODEX_DEFAULT_BUDGET: usize = 32 * 1024;
const CLAUDE_IMPORT_DEPTH: usize = 4;
const MAX_INSTRUCTIONS: usize = 512;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexInstructionSettings {
    pub project_doc_fallback_filenames: Option<Vec<String>>,
    pub project_doc_max_bytes: Option<usize>,
}

impl CodexInstructionSettings {
    fn merge(&mut self, other: Self) {
        if let Some(names) = other.project_doc_fallback_filenames {
            self.project_doc_fallback_filenames = Some(names);
        }
        if let Some(limit) = other.project_doc_max_bytes {
            self.project_doc_max_bytes = Some(limit);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionInventory {
    pub checkout_root: PathBuf,
    pub primary_root: PathBuf,
    pub working_directory: PathBuf,
    pub sources: Vec<InstructionSource>,
    pub unresolved: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSource {
    pub path: PathBuf,
    pub scope: InstructionScope,
    pub revision: Option<String>,
    pub selection: InstructionSelection,
    pub imported_from: Option<PathBuf>,
    pub included_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionScope {
    Checkout,
    PrimaryCheckout,
    Ancestor,
    Account,
    Managed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionSelection {
    Expected,
    Candidate,
    Shadowed,
    Empty,
    BudgetExceeded,
    DisabledByProfile,
    UnsupportedCliBuild,
    ExternalImport,
}

pub(super) fn workspace_directories(layout: &CheckoutLayout) -> Vec<&Path> {
    let mut directories: Vec<_> = layout
        .cwd
        .ancestors()
        .take_while(|path| path.starts_with(&layout.root))
        .collect();
    directories.reverse();
    directories
}

impl InstructionInventory {
    pub(super) fn new(layout: &CheckoutLayout) -> Self {
        Self {
            checkout_root: layout.root.clone(),
            primary_root: layout.primary_root.clone(),
            working_directory: layout.cwd.clone(),
            sources: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    fn directory_scope(&self, path: &Path) -> InstructionScope {
        if path.starts_with(&self.checkout_root) {
            InstructionScope::Checkout
        } else if path.starts_with(&self.primary_root) {
            InstructionScope::PrimaryCheckout
        } else {
            InstructionScope::Ancestor
        }
    }

    pub(super) fn codex(
        &mut self,
        discovery: &mut Discovery,
        directories: &[&Path],
        profile: Option<&Path>,
        reported: Option<&CodexInstructionSettings>,
    ) {
        let mut settings = CodexInstructionSettings::default();
        let paths = profile
            .into_iter()
            .map(|path| path.join("config.toml"))
            .chain(
                directories
                    .iter()
                    .map(|path| path.join(".codex/config.toml")),
            );
        for path in paths {
            if let Some((bytes, _)) = discovery.read(&path) {
                match std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| toml::from_str(text).ok())
                {
                    Some(parsed) => settings.merge(parsed),
                    None => self.unresolved.push(path),
                }
            }
        }
        if let Some(reported) = reported {
            settings.merge(reported.clone());
        }
        let base = ["AGENTS.override.md", "AGENTS.md"];
        if let Some(profile) = profile {
            let mut profile_budget = usize::MAX;
            self.codex_directory(
                discovery,
                profile,
                InstructionScope::Account,
                base,
                &mut profile_budget,
            );
        }
        let names = settings.project_doc_fallback_filenames.unwrap_or_default();
        let safe_names = names.iter().filter(|name| is_normal_component(name));
        let mut budget = settings
            .project_doc_max_bytes
            .unwrap_or(CODEX_DEFAULT_BUDGET);
        for directory in directories {
            self.codex_directory(
                discovery,
                directory,
                InstructionScope::Checkout,
                base.into_iter()
                    .chain(safe_names.clone().map(String::as_str)),
                &mut budget,
            );
        }
    }

    fn codex_directory<'a>(
        &mut self,
        discovery: &mut Discovery,
        directory: &Path,
        scope: InstructionScope,
        names: impl IntoIterator<Item = &'a str>,
        budget: &mut usize,
    ) {
        let mut selected = false;
        let mut uncertain = false;
        let mut visited = HashSet::new();
        for name in names {
            if !visited.insert(name) {
                continue;
            }
            let path = directory.join(name);
            let unresolved_count = discovery.unresolved.len();
            let Some((bytes, revision)) = discovery.read(&path) else {
                uncertain |= discovery.unresolved.len() > unresolved_count;
                continue;
            };
            let mut included_bytes = None;
            let selection = if String::from_utf8_lossy(&bytes).trim().is_empty() {
                InstructionSelection::Empty
            } else if selected {
                InstructionSelection::Shadowed
            } else if uncertain {
                InstructionSelection::Candidate
            } else if *budget == 0 {
                InstructionSelection::BudgetExceeded
            } else {
                selected = true;
                let used = bytes.len().min(*budget);
                *budget -= used;
                included_bytes = Some(used);
                InstructionSelection::Expected
            };
            self.sources.push(InstructionSource {
                path,
                scope,
                revision: Some(revision),
                selection,
                imported_from: None,
                included_bytes,
            });
        }
    }

    pub(super) fn claude(
        &mut self,
        discovery: &mut Discovery,
        profile: Option<&Path>,
        managed: Option<&Path>,
        version: &str,
    ) {
        let mut paths = Vec::new();
        for (root, scope) in [
            (managed, InstructionScope::Managed),
            (profile, InstructionScope::Account),
        ] {
            if let Some(root) = root {
                paths.push((root.join("CLAUDE.md"), root.to_path_buf(), scope, false));
            }
        }
        let directories: Vec<_> = self
            .working_directory
            .ancestors()
            .map(Path::to_path_buf)
            .collect();
        for directory in directories.iter().rev() {
            let boundary = if directory.starts_with(&self.checkout_root) {
                self.checkout_root.clone()
            } else {
                directory.clone()
            };
            let scope = self.directory_scope(directory);
            for name in [
                "CLAUDE.md",
                ".claude/CLAUDE.md",
                "CLAUDE.local.md",
                "AGENTS.md",
                ".claude/AGENTS.md",
            ] {
                paths.push((
                    directory.join(name),
                    boundary.clone(),
                    scope,
                    name.ends_with("AGENTS.md"),
                ));
            }
        }
        let direct_agents = claude_supports_agents(version);
        let mut visited = HashSet::new();
        for (path, boundary, scope, agents) in paths {
            let selection = if agents && !direct_agents {
                InstructionSelection::UnsupportedCliBuild
            } else {
                // The managed streaming launch disables implicit customizations.
                InstructionSelection::DisabledByProfile
            };
            self.claude_file(
                discovery,
                path,
                &boundary,
                scope,
                selection,
                None,
                &mut visited,
            );
        }
    }

    fn claude_file(
        &mut self,
        discovery: &mut Discovery,
        path: PathBuf,
        boundary: &Path,
        scope: InstructionScope,
        selection: InstructionSelection,
        import: Option<(&Path, usize)>,
        visited: &mut HashSet<PathBuf>,
    ) {
        if self.sources.len() >= MAX_INSTRUCTIONS {
            if self.unresolved.last() != Some(&self.working_directory) {
                self.unresolved.push(self.working_directory.clone());
            }
            return;
        }
        if !visited.insert(path.clone()) {
            return;
        }
        let Some((bytes, revision)) = discovery.read(&path) else {
            if import.is_some() {
                self.unresolved.push(path);
            }
            return;
        };
        self.sources.push(InstructionSource {
            path: path.clone(),
            scope,
            revision: Some(revision),
            selection,
            imported_from: import.map(|(source, _)| source.to_path_buf()),
            included_bytes: None,
        });
        let depth = import.map_or(0, |(_, depth)| depth);
        if depth >= CLAUDE_IMPORT_DEPTH || selection == InstructionSelection::UnsupportedCliBuild {
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        for token in claude_imports(&String::from_utf8_lossy(&bytes)).take(MAX_INSTRUCTIONS) {
            let lookup = if let Some(relative) = token
                .strip_prefix("~/")
                .or_else(|| token.strip_prefix("~\\"))
            {
                super::configured_directory("USERPROFILE").map(|home| home.join(relative))
            } else {
                Some(parent.join(&token))
            };
            let Some(target) = lookup.as_deref().and_then(normalize_path) else {
                continue;
            };
            if !target.starts_with(boundary) {
                self.sources.push(InstructionSource {
                    path: target,
                    scope,
                    revision: None,
                    selection: InstructionSelection::ExternalImport,
                    imported_from: Some(path.clone()),
                    included_bytes: None,
                });
                continue;
            }
            self.claude_file(
                discovery,
                target,
                boundary,
                scope,
                selection,
                Some((&path, depth + 1)),
                visited,
            );
        }
    }

    pub(super) fn antigravity(
        &mut self,
        discovery: &mut Discovery,
        directories: &[&Path],
        profile: Option<&Path>,
    ) {
        if let Some(profile) = profile {
            self.antigravity_files(discovery, profile, InstructionScope::Account);
            let config = profile.join("config");
            self.antigravity_directory(
                discovery,
                &config,
                InstructionScope::Account,
                profile,
                &config,
            );
        }
        let workspace = self.checkout_root.clone();
        for directory in directories {
            self.antigravity_files(discovery, directory, InstructionScope::Checkout);
            for name in [".agents", ".agent", "_agents", "_agent"] {
                self.antigravity_directory(
                    discovery,
                    &directory.join(name),
                    InstructionScope::Checkout,
                    &workspace,
                    directory,
                );
            }
        }
    }

    fn antigravity_files(
        &mut self,
        discovery: &mut Discovery,
        root: &Path,
        scope: InstructionScope,
    ) {
        for name in ["AGENTS.md", "GEMINI.md"] {
            self.note_file(
                discovery,
                root.join(name),
                scope,
                InstructionSelection::Expected,
            );
        }
    }

    fn antigravity_directory(
        &mut self,
        discovery: &mut Discovery,
        path: &Path,
        scope: InstructionScope,
        boundary: &Path,
        base: &Path,
    ) {
        self.antigravity_files(discovery, path, scope);
        discovery.antigravity_settings(path);
        discovery.skills(path, "skills");
        self.antigravity_rules(discovery, &path.join("rules"), scope);
        for (name, skills) in [("skills.json", true), ("rules.json", false)] {
            let manifest = path.join(name);
            let Some((bytes, _)) = discovery.read(&manifest) else {
                continue;
            };
            let Ok(config) = serde_json::from_slice::<AntigravityManifest>(&bytes) else {
                self.unresolved.push(manifest);
                continue;
            };
            if !config.inherits.is_empty() {
                // Inherited manifests have their own relative root and policy.
                self.unresolved.push(manifest.clone());
            }
            for entry in config.entries.into_iter().take(MAX_SKILL_ENTRIES) {
                let Some(mut target) = normalize_path(&base.join(&entry.path))
                    .filter(|target| target.starts_with(boundary))
                else {
                    self.unresolved.push(manifest.clone());
                    continue;
                };
                if !entry.path.is_absolute()
                    && base != boundary
                    && matches!(plain_metadata(&target), Ok(None))
                    && let Some(fallback) = normalize_path(&boundary.join(&entry.path))
                        .filter(|candidate| candidate.starts_with(boundary))
                {
                    target = fallback;
                }
                for item in manifest_entries(&target, &entry, &config.exclude, skills) {
                    if skills {
                        discovery.file(item);
                    } else {
                        self.note_file(discovery, item, scope, InstructionSelection::Candidate);
                    }
                }
            }
        }
    }

    fn antigravity_rules(
        &mut self,
        discovery: &mut Discovery,
        path: &Path,
        scope: InstructionScope,
    ) {
        for file in configuration_children(path).filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        }) {
            self.note_file(discovery, file, scope, InstructionSelection::Candidate);
        }
    }

    fn note_file(
        &mut self,
        discovery: &mut Discovery,
        path: PathBuf,
        scope: InstructionScope,
        selection: InstructionSelection,
    ) {
        if let Some((_, revision)) = discovery.read(&path) {
            self.sources.push(InstructionSource {
                path,
                scope,
                revision: Some(revision),
                selection,
                imported_from: None,
                included_bytes: None,
            });
        }
    }
}

pub(super) fn claude_supports_agents(version: &str) -> bool {
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
    version.is_some_and(|value: (u32, u32, u32)| value >= (2, 1, 277))
}

fn is_normal_component(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(value)) if value == name)
        && parts.next().is_none()
}

pub(crate) fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir if !result.pop() => return None,
            Component::ParentDir | Component::CurDir => {}
            _ => result.push(part),
        }
    }
    result.is_absolute().then_some(result)
}

fn claude_imports(text: &str) -> impl Iterator<Item = String> + '_ {
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:^|\s)@((?:\\ |[^\s`"'])+)"#).expect("valid import pattern")
    });
    let mut fence = None;
    text.lines().flat_map(move |line| {
        let trimmed = line.trim_start();
        if let Some(marker) = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker))
        {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            return Vec::new();
        }
        if fence.is_some() {
            return Vec::new();
        }
        without_code_spans(line)
            .into_iter()
            .flat_map(|part| IMPORT.captures_iter(part))
            .map(|entry| entry[1].replace("\\ ", " "))
            .collect()
    })
}

fn without_code_spans(line: &str) -> Vec<&str> {
    let mut text = line;
    let mut prose = Vec::new();
    while let Some(start) = text.find('`') {
        prose.push(&text[..start]);
        let count = text[start..]
            .bytes()
            .take_while(|byte| *byte == b'`')
            .count();
        let mut ending = &text[start + count..];
        let mut closed = false;
        while let Some(index) = ending.find('`') {
            let width = ending[index..]
                .bytes()
                .take_while(|byte| *byte == b'`')
                .count();
            ending = &ending[index + width..];
            if width == count {
                closed = true;
                break;
            }
        }
        text = if closed { ending } else { "" };
    }
    prose.push(text);
    prose
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct AntigravityManifest {
    entries: Vec<ManifestEntry>,
    inherits: Vec<serde_json::Value>,
    exclude: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestEntry {
    path: PathBuf,
    #[serde(default)]
    include_only: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn configuration_children(path: &Path) -> impl Iterator<Item = PathBuf> {
    plain_metadata(path)
        .ok()
        .flatten()
        .filter(|metadata| metadata.is_dir())
        .and_then(|_| fs::read_dir(path).ok())
        .into_iter()
        .flatten()
        .take(MAX_SKILL_ENTRIES)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
}

fn manifest_entries(
    path: &Path,
    entry: &ManifestEntry,
    excludes: &[String],
    skills: bool,
) -> Vec<PathBuf> {
    let children = if plain_metadata(path)
        .ok()
        .flatten()
        .is_some_and(|metadata| metadata.is_dir())
    {
        if skills
            && plain_metadata(&path.join("SKILL.md"))
                .ok()
                .flatten()
                .is_some_and(|file| file.is_file())
        {
            vec![path.to_path_buf()]
        } else if entry.include_only.is_empty() {
            configuration_children(path).collect()
        } else {
            entry
                .include_only
                .iter()
                .filter_map(|name| {
                    normalize_path(&path.join(name)).filter(|candidate| candidate.starts_with(path))
                })
                .collect()
        }
    } else {
        vec![path.to_path_buf()]
    };
    children
        .into_iter()
        .filter(|item| {
            let names = [
                item.file_stem(),
                item.file_name(),
                item.strip_prefix(path).ok().map(Path::as_os_str),
            ];
            !entry
                .exclude
                .iter()
                .chain(excludes)
                .any(|excluded| names.contains(&Some(excluded.as_ref())))
        })
        .filter_map(|item| {
            if skills && !item.ends_with("SKILL.md") {
                return Some(item.join("SKILL.md"));
            }
            item.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                .then_some(item)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> anyhow::Result<(tempfile::TempDir, CheckoutLayout, PathBuf)> {
        let parent = tempfile::tempdir()?;
        let root = dunce::canonicalize(parent.path())?;
        let layout = CheckoutLayout {
            root: root.join("linked"),
            primary_root: root.join("primary"),
            cwd: root.join("linked").join("api"),
        };
        let profile = root.join("profile");
        for directory in [&layout.cwd, &layout.primary_root, &profile] {
            fs::create_dir_all(directory)?;
        }
        Ok((parent, layout, profile))
    }

    #[test]
    fn codex_selects_the_configured_fallback_and_nearest_override() -> anyhow::Result<()> {
        let (_temp, layout, profile) = layout()?;
        fs::write(
            profile.join("config.toml"),
            "project_doc_fallback_filenames = [\"CODEX.md\"]",
        )?;
        fs::write(profile.join("AGENTS.override.md"), "   \n")?;
        fs::write(profile.join("AGENTS.md"), "account instructions")?;
        fs::write(layout.root.join("CODEX.md"), "linked instructions")?;
        fs::write(layout.primary_root.join("AGENTS.md"), "other worktree")?;
        fs::write(layout.cwd.join("AGENTS.override.md"), "nested override")?;
        fs::write(layout.cwd.join("AGENTS.md"), "shadowed")?;
        let mut inventory = InstructionInventory::new(&layout);
        inventory.codex(
            &mut Discovery::default(),
            &workspace_directories(&layout),
            Some(&profile),
            None,
        );
        let expected = inventory
            .sources
            .iter()
            .filter(|entry| entry.selection == InstructionSelection::Expected)
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            expected,
            [
                profile.join("AGENTS.md"),
                layout.root.join("CODEX.md"),
                layout.cwd.join("AGENTS.override.md")
            ]
        );
        assert!(
            inventory
                .sources
                .iter()
                .any(|entry| entry.path == layout.cwd.join("AGENTS.md")
                    && entry.selection == InstructionSelection::Shadowed)
        );
        assert!(
            inventory
                .sources
                .iter()
                .all(|entry| !entry.path.starts_with(&layout.primary_root))
        );

        let mut reported = InstructionInventory::new(&layout);
        reported.codex(
            &mut Discovery::default(),
            &workspace_directories(&layout),
            Some(&profile),
            Some(&CodexInstructionSettings {
                project_doc_fallback_filenames: Some(Vec::new()),
                project_doc_max_bytes: Some(6),
            }),
        );
        assert!(
            reported
                .sources
                .iter()
                .all(|entry| !entry.path.ends_with("CODEX.md"))
        );
        assert_eq!(
            reported
                .sources
                .iter()
                .find(|entry| entry.path == layout.cwd.join("AGENTS.override.md"))
                .unwrap()
                .included_bytes,
            Some(6)
        );
        Ok(())
    }

    #[test]
    fn claude_tracks_imports_and_reports_which_builds_recognize_direct_agents() -> anyhow::Result<()>
    {
        let (_temp, layout, _) = layout()?;
        fs::write(
            layout.root.join("CLAUDE.md"),
            "@AGENTS.md\n- follow @docs/rules\\ file.md\n`@ignored.md`\n``@ignored.md``\n```text\n@ignored.md\n```\n@../primary/outside.md",
        )?;
        fs::write(layout.root.join("CLAUDE.local.md"), "local rules")?;
        fs::write(layout.root.join("AGENTS.md"), "@CLAUDE.md")?;
        fs::create_dir(layout.root.join("docs"))?;
        fs::write(
            layout.root.join("docs").join("rules file.md"),
            "imported rules",
        )?;
        fs::write(layout.primary_root.join("outside.md"), "external rules")?;
        fs::write(layout.cwd.join("AGENTS.md"), "direct candidate")?;
        let mut inventory = InstructionInventory::new(&layout);
        inventory.claude(&mut Discovery::default(), None, None, "2.1.296");
        let imported = inventory
            .sources
            .iter()
            .find(|entry| entry.path == layout.root.join("AGENTS.md"))
            .unwrap();
        assert_eq!(
            imported.imported_from.as_deref(),
            Some(layout.root.join("CLAUDE.md").as_path())
        );
        assert_eq!(imported.selection, InstructionSelection::DisabledByProfile);
        assert!(
            inventory
                .sources
                .iter()
                .any(|entry| entry.path.ends_with("rules file.md") && entry.revision.is_some())
        );
        assert!(
            inventory
                .sources
                .iter()
                .any(|entry| entry.path == layout.primary_root.join("outside.md")
                    && entry.selection == InstructionSelection::ExternalImport
                    && entry.revision.is_none())
        );
        assert!(
            !inventory
                .unresolved
                .iter()
                .any(|path| path.ends_with("ignored.md"))
        );

        let mut old = InstructionInventory::new(&layout);
        old.claude(&mut Discovery::default(), None, None, "2.1.276");
        assert!(
            old.sources
                .iter()
                .any(|entry| entry.path == layout.cwd.join("AGENTS.md")
                    && entry.selection == InstructionSelection::UnsupportedCliBuild)
        );
        assert!(
            old.sources
                .iter()
                .any(|entry| entry.path == layout.root.join("AGENTS.md")
                    && entry.imported_from.is_some()
                    && entry.selection == InstructionSelection::DisabledByProfile)
        );
        Ok(())
    }

    #[test]
    fn antigravity_discovers_workspace_documents_and_manifest_entries() -> anyhow::Result<()> {
        let (_temp, layout, profile) = layout()?;
        let agents = layout.root.join(".agents");
        fs::create_dir_all(agents.join("rules"))?;
        fs::create_dir_all(layout.root.join("custom/nested/should-load"))?;
        fs::write(layout.root.join("AGENTS.md"), "workspace instructions")?;
        fs::write(layout.cwd.join("GEMINI.md"), "nested instructions")?;
        fs::write(agents.join("rules").join("fixture.md"), "workspace rule")?;
        fs::write(layout.primary_root.join("GEMINI.md"), "other worktree")?;
        fs::write(
            agents.join("skills.json"),
            r#"{"entries":[{"path":"custom","include_only":["nested/should-load"]}]}"#,
        )?;
        fs::write(
            layout.root.join("custom/nested/should-load/SKILL.md"),
            "manifest skill",
        )?;
        let mut inventory = InstructionInventory::new(&layout);
        let mut discovery = Discovery::default();
        inventory.antigravity(
            &mut discovery,
            &workspace_directories(&layout),
            Some(&profile),
        );
        for name in ["AGENTS.md", "GEMINI.md", "fixture.md"] {
            assert!(
                inventory
                    .sources
                    .iter()
                    .any(|entry| entry.path.ends_with(name) && entry.revision.is_some())
            );
        }
        assert!(
            inventory
                .sources
                .iter()
                .all(|entry| !entry.path.starts_with(&layout.primary_root))
        );
        assert!(
            discovery
                .detected
                .iter()
                .any(|entry| entry.path.ends_with("should-load/SKILL.md"))
        );
        Ok(())
    }
}
