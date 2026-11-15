use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    mcp_bridge::{BridgeLaunch, SERVER_NAME},
    repository::CheckoutLayout,
};

use super::{
    super::{
        capabilities::{ConfigurationEvidence, revision},
        launch_scope::ScopedLaunchEvidence,
        plugins::{PluginInventory, PluginInventoryStatus},
        skills::SkillCatalog,
    },
    normalize_path,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportedConfiguration {
    pub environment_revision: String,
    pub tool_revisions: BTreeMap<String, String>,
}

pub(crate) fn mcp_identity(name: &str) -> String {
    format!("mcp/{name}")
}

fn checkout_relative(root: &Path, path: &Path) -> Option<Vec<String>> {
    let absolute = normalize_path(dunce::simplified(path))?;
    let path = absolute.strip_prefix(root).ok()?;
    path.iter()
        .map(|part| part.to_str().map(str::to_owned))
        .collect()
}

fn normalize_path_value(root: &Path, value: &mut Value) {
    if let Some(parts) = value
        .as_str()
        .and_then(|path| checkout_relative(root, Path::new(path)))
    {
        *value = json!({"checkoutRelative": parts});
    }
}

pub(crate) fn codex_configuration(
    mut config: serde_json::Map<String, Value>,
    root: &Path,
    bridge: Option<&BridgeLaunch>,
) -> Result<ReportedConfiguration> {
    let mut tools = BTreeMap::new();
    if let Some(servers) = config.remove("mcp_servers") {
        if let Value::Object(servers) = servers {
            for (name, mut server) in servers {
                if server["enabled"] == false || !server.is_object() {
                    continue;
                }
                if name == SERVER_NAME
                    && let Some(bridge) = bridge
                    && server["command"].as_str() == Some(bridge.command.as_str())
                    && server["args"] == json!(bridge.args)
                {
                    // Keep the exact command, remaining options, and operating mode in the key.
                    // The protected ticket belongs to this attempt and expires with it.
                    server["args"] = json!({ "runBoundBridge": bridge.permission_prompt });
                }
                for field in ["cwd", "command"] {
                    if let Some(value) = server.get_mut(field) {
                        normalize_path_value(root, value);
                    }
                }
                if let Some(args) = server.get_mut("args").and_then(Value::as_array_mut) {
                    for arg in args {
                        normalize_path_value(root, arg);
                    }
                }
                server.sort_all_objects();
                tools.insert(mcp_identity(&name), revision(&server)?);
            }
        } else if !servers.is_null() {
            // Keep an unrecognized representation in the environment fingerprint.
            config.insert("mcp_servers".into(), servers);
        }
    }
    if let Some(Value::Object(projects)) = config.get_mut("projects") {
        let entries = std::mem::take(projects);
        let mut projects = entries
            .into_iter()
            .map(|(path, value)| {
                let mut path = Value::String(path);
                normalize_path_value(root, &mut path);
                (path.to_string(), value)
            })
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| left.0.cmp(&right.0));
        config.insert("projects".into(), json!(projects));
    }
    let mut config = Value::Object(config);
    config.sort_all_objects();
    Ok(ReportedConfiguration {
        environment_revision: revision(&config)?,
        tool_revisions: tools,
    })
}

pub(crate) fn resolve_effective_configuration(
    configuration: &mut ConfigurationEvidence,
    layout: &CheckoutLayout,
    reported: Option<&ReportedConfiguration>,
    launch: &ScopedLaunchEvidence,
    plugins: Option<&PluginInventory>,
    skills: Option<&SkillCatalog>,
) -> Result<()> {
    configuration.environment_revision = None;
    configuration.effective_revision = None;
    configuration.tool_revisions.clear();
    let (Some(reported), Some(plugins), Some(skills)) = (reported, plugins, skills) else {
        return Ok(());
    };
    if !configuration.unresolved.is_empty()
        || plugins.report.status != PluginInventoryStatus::Reported
        || !skills.reported
        || !skills.issues.is_empty()
    {
        return Ok(());
    }
    let mut sources = configuration
        .detected
        .iter()
        .map(|source| {
            (
                logical_location(&layout.root, &source.path),
                source
                    .environment_revision
                    .as_deref()
                    .unwrap_or(&source.revision),
            )
        })
        .collect::<Vec<_>>();
    sources.sort_unstable();
    let mut skills = skills
        .entries
        .iter()
        .map(|skill| {
            (
                logical_location(&layout.root, &skill.path),
                &skill.name,
                &skill.scope,
                &skill.plugin_id,
                skill.enabled,
                &skill.revision,
            )
        })
        .collect::<Vec<_>>();
    skills.sort_unstable();
    let instructions = launch
        .instruction_sources
        .iter()
        .map(|source| {
            (
                logical_location(&layout.root, &source.path),
                &source.revision,
                source.included_bytes,
            )
        })
        .collect::<Vec<_>>();
    let environment = revision(&(
        &reported.environment_revision,
        &launch.strategy,
        sources,
        instructions,
        skills,
        &plugins.report.plugins,
        &launch.non_mcp_approvals,
    ))?;
    for (id, report) in &reported.tool_revisions {
        if let Some(source) = launch.mcp_sources.get(id) {
            configuration
                .tool_revisions
                .insert(id.clone(), revision(&(source, report))?);
        }
    }
    // Include unqualified reported tools for operations that depend on the whole configuration.
    configuration.effective_revision = Some(revision(&(
        &environment,
        &reported.tool_revisions,
        &launch.mcp_sources,
        &launch.approved,
    ))?);
    configuration.environment_revision = Some(environment);
    Ok(())
}

fn logical_location(root: &Path, path: &Path) -> (bool, std::path::PathBuf) {
    match path.strip_prefix(root) {
        Ok(relative) => (true, relative.into()),
        Err(_) => (false, path.into()),
    }
}
