use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::workflows::{
    ArtifactKind, BuiltinNode, CommandOutputFormat, Endpoint, NodeConfiguration, NodeDefinition,
    WorkflowDefinition, WorkflowGraph, role_templates::ConversationTemplate,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageArtifactKind {
    Schema,
    Pipeline,
    Role,
    Preset,
    Subflow,
    Command,
    McpTool,
}

impl PackageArtifactKind {
    pub(super) fn sql(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Pipeline => "pipeline",
            Self::Role => "role",
            Self::Preset => "preset",
            Self::Subflow => "subflow",
            Self::Command => "command",
            Self::McpTool => "mcp_tool",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageDestination {
    #[default]
    Project,
    Library,
}

impl PackageDestination {
    pub(super) fn key(self, project: &str) -> String {
        match self {
            Self::Project => format!("project:{project}"),
            Self::Library => "library".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageArtifactVersion {
    pub destination: PackageDestination,
    pub kind: PackageArtifactKind,
    pub id: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PackageReference {
    Draft {
        key: String,
    },
    Published {
        version: PackageArtifactVersion,
    },
    Parameter {
        name: String,
        kind: PackageArtifactKind,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PackageCommandBinding {
    Registered { id: String, revision: String },
    Required { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PackageArtifactBody {
    Schema {
        fields: Vec<crate::workflows::schema::SchemaField>,
    },
    Pipeline {
        graph: WorkflowGraph,
    },
    Role {
        template: ConversationTemplate,
    },
    Preset {
        node_kind: BuiltinNode,
        configuration: NodeConfiguration,
        #[serde(default)]
        editable_fields: Vec<String>,
    },
    Subflow {
        graph: WorkflowGraph,
        #[serde(default)]
        inputs: BTreeMap<String, Endpoint>,
        #[serde(default)]
        outputs: BTreeMap<String, Endpoint>,
    },
    Prompt {
        prompt: PromptCard,
    },
    Command {
        binding: PackageCommandBinding,
        arguments: Vec<String>,
        input: Map<String, Value>,
        output: CommandOutputFormat,
    },
    McpTool {
        binding: String,
        tool: String,
        input_schema: Map<String, Value>,
        output_schema: Option<Map<String, Value>>,
    },
}

impl PackageArtifactBody {
    pub fn kind(&self) -> PackageArtifactKind {
        match self {
            Self::Schema { .. } => PackageArtifactKind::Schema,
            Self::Pipeline { .. } => PackageArtifactKind::Pipeline,
            Self::Role { .. } => PackageArtifactKind::Role,
            Self::Preset { .. } => PackageArtifactKind::Preset,
            Self::Subflow { .. } | Self::Prompt { .. } => PackageArtifactKind::Subflow,
            Self::Command { .. } => PackageArtifactKind::Command,
            Self::McpTool { .. } => PackageArtifactKind::McpTool,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptCard {
    pub text: String,
    pub executor: BuiltinNode,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub schema_contract: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub node_prefab: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role_overrides: BTreeMap<String, Value>,
    #[serde(default)]
    pub inputs: BTreeMap<String, PromptInput>,
    #[serde(default)]
    pub outputs: BTreeMap<String, PromptOutput>,
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub delegates: Vec<PromptDelegate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<NodeConfiguration>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptInput {
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptOutput {
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptDelegate {
    pub alias: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptExpansion {
    pub workflow: WorkflowDefinition,
    pub inputs: BTreeMap<String, Endpoint>,
    pub outputs: BTreeMap<String, Endpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageMember {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub destination: PackageDestination,
    #[serde(default)]
    pub base: Option<PackageArtifactVersion>,
    pub body: PackageArtifactBody,
    #[serde(default)]
    pub references: BTreeMap<String, PackageReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPackage {
    pub name: String,
    pub members: Vec<PackageMember>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ResolvedPackageReference {
    Published {
        version: PackageArtifactVersion,
    },
    Parameter {
        name: String,
        kind: PackageArtifactKind,
    },
}

impl From<ResolvedPackageReference> for PackageReference {
    fn from(reference: ResolvedPackageReference) -> Self {
        match reference {
            ResolvedPackageReference::Published { version } => Self::Published { version },
            ResolvedPackageReference::Parameter { name, kind } => Self::Parameter { name, kind },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPackageArtifact {
    pub version: PackageArtifactVersion,
    pub name: String,
    pub body: PackageArtifactBody,
    pub references: BTreeMap<String, ResolvedPackageReference>,
    pub definitions: BTreeMap<String, NodeDefinition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<String, super::WorkflowNodePosition>,
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_expansion: Option<PromptExpansion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedPackageMember {
    pub key: String,
    pub artifact: PublishedPackageArtifact,
    pub previous: Option<PackageArtifactVersion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackagePublication {
    pub id: String,
    pub draft_id: String,
    pub reviewed_revision: String,
    pub originating_request_id: Option<String>,
    pub originating_run_id: Option<String>,
    pub decision_source: String,
    pub confirmed_at: i64,
    pub members: Vec<PublishedPackageMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkflowPackagePublishOutcome {
    Published {
        publication: WorkflowPackagePublication,
    },
    StaleBase {
        member: String,
        current: PublishedPackageArtifact,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackageDraft {
    pub id: String,
    pub conversation_id: Option<String>,
    pub revision: String,
    pub parent_revision: Option<String>,
    pub package: WorkflowPackage,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, BTreeMap<String, super::WorkflowNodePosition>>,
    pub publication: Option<WorkflowPackagePublication>,
    pub request: Option<WorkflowPackageRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackageCatalogEntry {
    pub version: PackageArtifactVersion,
    pub name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub prompt: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub typed_prompt: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub node_prefab: bool,
    pub blockers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackageRequest {
    pub id: String,
    pub job_id: String,
    pub run_id: Option<String>,
    pub state: String,
    pub diagnostic: Option<String>,
}

impl WorkflowPackageRequest {
    pub(super) fn pending(&self) -> bool {
        matches!(
            self.state.as_str(),
            "queued" | "starting" | "running" | "waiting_for_input" | "paused"
        )
    }
}
