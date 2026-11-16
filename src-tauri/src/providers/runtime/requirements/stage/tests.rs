use super::*;
use crate::providers::{
    configuration::InstructionScope,
    plugins::{PluginReport, ReportedPlugin},
    skills::{SkillEntry, SkillSource},
};
use crate::workflows::stage_requirements::StageSourcePath;
use std::path::{Path, PathBuf};

fn instruction_inventory(root: &Path, sources: Vec<InstructionSource>) -> InstructionInventory {
    InstructionInventory {
        checkout_root: root.to_owned(),
        primary_root: root.to_owned(),
        working_directory: root.to_owned(),
        sources,
        unresolved: vec![],
    }
}

#[test]
fn instructions_require_the_exact_qualified_source_and_current_revision() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    let applied = InstructionSource {
        path: root.join("AGENTS.md"),
        scope: InstructionScope::Checkout,
        revision: Some("1".repeat(64)),
        selection: InstructionSelection::Expected,
        imported_from: None,
        included_bytes: Some(12),
    };
    let required = RequiredStageSource::Instruction {
        provider: Provider::Codex,
        path: StageSourcePath::Checkout {
            path: PathBuf::from("AGENTS.md"),
        },
        revision: "1".repeat(64),
    };
    let check = |present: InstructionSource, applied: &[InstructionSource]| {
        qualify_source(
            &required,
            Provider::Codex,
            &instruction_inventory(&root, vec![present]),
            applied,
            None,
            None,
            true,
        )
        .is_ok()
    };
    assert!(check(applied.clone(), std::slice::from_ref(&applied)));
    assert!(!check(applied.clone(), &[]));
    assert!(!check(
        InstructionSource {
            revision: Some("2".repeat(64)),
            ..applied.clone()
        },
        std::slice::from_ref(&applied)
    ));
    assert!(!check(applied.clone(), &[applied.clone(), applied.clone()]));
    Ok(())
}

#[test]
fn plugins_need_a_unique_installation_and_an_enabled_attributable_session_skill() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    let plugin = ReportedPlugin {
        id: "review@store".into(),
        name: Some("Review".into()),
        version: Some("3".into()),
        enabled: Some(true),
        scope: Some("user".into()),
        marketplace: Some("store".into()),
        source: Some("marketplace".into()),
        source_location: Some("local-store".into()),
        install_path: Some(root.join("entry").display().to_string()),
        skills: None,
        hooks: None,
        mcp_servers: None,
    };
    let requirement = RequiredStageSource::Plugin {
        provider: Provider::Codex,
        identity: StagePluginIdentity::from(&plugin),
    };
    let skill = SkillEntry {
        id: "reported-skill".into(),
        provider: Provider::Codex,
        source: SkillSource::Provider,
        name: "plugin-review".into(),
        path: root.join("entry/SKILL.md"),
        scope: None,
        plugin_id: Some(plugin.id.clone()),
        enabled: Some(true),
        revision: None,
    };
    let catalog = SkillCatalog {
        entries: vec![skill],
        issues: vec![],
        reported: true,
    };
    let mut inventory = PluginInventory {
        id: "inventory".into(),
        checked_at: "now".into(),
        report: PluginReport {
            provider: Provider::Codex,
            executable: None,
            version: None,
            status: PluginInventoryStatus::Reported,
            plugins: vec![plugin.clone()],
            load_telemetry_reported: false,
        },
    };
    let instructions = instruction_inventory(&root, vec![]);
    let check = |catalog: Option<&SkillCatalog>, inventory: &PluginInventory| {
        qualify_source(
            &requirement,
            Provider::Codex,
            &instructions,
            &[],
            catalog,
            Some(inventory),
            true,
        )
        .is_ok()
    };
    assert!(check(Some(&catalog), &inventory));
    assert!(!check(None, &inventory));
    inventory.report.plugins.push(ReportedPlugin {
        scope: Some("project".into()),
        ..plugin
    });
    assert!(!check(Some(&catalog), &inventory));
    Ok(())
}
