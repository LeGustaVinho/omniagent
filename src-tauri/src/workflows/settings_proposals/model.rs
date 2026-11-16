use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsField {
    LiveQuestionTimeout,
    TemplateDisplayLabel,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsAutoGrant {
    pub field: SettingsField,
    pub checkout_id: Option<String>,
    pub template_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatchAllowlist {
    pub revision: i64,
    pub grants: BTreeSet<SettingsAutoGrant>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionedSetting<T> {
    pub revision: i64,
    pub value: T,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateDisplayLabel {
    pub template_id: String,
    pub template_revision: String,
    pub display_label: RevisionedSetting<Option<String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SettingsPatchCatalogQuery {
    #[serde(default)]
    pub template_offset: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsPatchCatalog {
    pub live_question_timeout: RevisionedSetting<u32>,
    pub allowlist_revision: i64,
    pub templates: Vec<SettingsPatchTemplate>,
    pub next_template_offset: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsPatchTemplate {
    pub name: String,
    #[serde(flatten)]
    pub label: TemplateDisplayLabel,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub request_id: String,
    pub changes: Vec<SettingsChange>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    tag = "field",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SettingsChange {
    LiveQuestionTimeout {
        expected_revision: i64,
        minutes: u32,
    },
    TemplateDisplayLabel {
        template_id: String,
        template_revision: String,
        expected_revision: i64,
        #[serde(deserialize_with = "Option::deserialize")]
        label: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    tag = "field",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SettingsFieldDiff {
    LiveQuestionTimeout {
        before: RevisionedSetting<u32>,
        after: u32,
    },
    TemplateDisplayLabel {
        template_id: String,
        template_revision: String,
        before: RevisionedSetting<Option<String>>,
        after: Option<String>,
    },
}

impl SettingsFieldDiff {
    pub(super) fn field(&self) -> SettingsField {
        match self {
            Self::LiveQuestionTimeout { .. } => SettingsField::LiveQuestionTimeout,
            Self::TemplateDisplayLabel { .. } => SettingsField::TemplateDisplayLabel,
        }
    }

    pub(super) fn template_id(&self) -> Option<&str> {
        match self {
            Self::LiveQuestionTimeout { .. } => None,
            Self::TemplateDisplayLabel { template_id, .. } => Some(template_id),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatchActor {
    pub run_id: String,
    pub project_id: String,
    pub checkout_id: String,
    pub turn_id: String,
    pub dispatch_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsDecisionSource {
    UserAllowlist,
    DesktopUser,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsAppliedRevision {
    pub field: SettingsField,
    pub template_id: Option<String>,
    pub before_revision: i64,
    pub after_revision: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SettingsProposalDecision {
    Pending,
    Applied {
        source: SettingsDecisionSource,
        allowlist_revision: i64,
        at: i64,
        revisions: Vec<SettingsAppliedRevision>,
    },
    Rejected {
        source: SettingsDecisionSource,
        allowlist_revision: i64,
        at: i64,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsProposal {
    pub id: String,
    pub actor: SettingsPatchActor,
    pub request_id: String,
    pub review_revision: String,
    pub proposed_at: i64,
    pub allowlist_revision: i64,
    pub diffs: Vec<SettingsFieldDiff>,
    pub decision: SettingsProposalDecision,
}
