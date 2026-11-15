use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Provider, capabilities::revision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Filesystem,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRequest {
    pub name: String,
    pub matching_ids: Vec<String>,
}

pub(crate) fn explicit_skill_request<'a>(
    provider: Provider,
    prompt: &str,
    skills: impl IntoIterator<Item = &'a SkillEntry>,
) -> Option<SkillRequest> {
    let prefix = if provider == Provider::Codex {
        '$'
    } else {
        '/'
    };
    let name = prompt.split_whitespace().next()?.strip_prefix(prefix)?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return None;
    }
    let mut matching_ids = skills
        .into_iter()
        .filter(|skill| {
            skill.name == name
                || name.split_once(':').is_some_and(|(namespace, local)| {
                    skill.name == local && skill.plugin_id.as_deref() == Some(namespace)
                })
        })
        .map(|skill| skill.id.clone())
        .collect::<Vec<_>>();
    matching_ids.sort_unstable();
    matching_ids.dedup();
    // Slash commands also include built-ins such as /plan. A name known only
    // to the provider can still be reported as an available command separately.
    if matching_ids.is_empty() && prefix == '/' {
        return None;
    }
    Some(SkillRequest {
        name: name.into(),
        matching_ids,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub id: String,
    pub provider: Provider,
    pub source: SkillSource,
    pub name: String,
    pub path: PathBuf,
    pub scope: Option<String>,
    pub plugin_id: Option<String>,
    pub enabled: Option<bool>,
    pub revision: Option<String>,
}

impl SkillEntry {
    pub(crate) fn identity(provider: Provider, path: &Path) -> anyhow::Result<String> {
        let normalized: PathBuf = dunce::simplified(path).components().collect();
        revision(&(provider, normalized))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalog {
    pub entries: Vec<SkillEntry>,
    pub issues: Vec<SkillCatalogIssue>,
    pub reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogIssue {
    pub path: Option<PathBuf>,
    pub message: String,
}

impl SkillCatalog {
    pub(crate) fn unavailable(message: &str) -> Self {
        Self {
            entries: Vec::new(),
            issues: vec![SkillCatalogIssue {
                path: None,
                message: message.into(),
            }],
            reported: false,
        }
    }
}
