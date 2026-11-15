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
