use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::workflows::{
    BuiltinNode, CommandOutputFormat, Endpoint, NodeConfiguration, NodeDefinition, WorkflowGraph,
    role_templates::ConversationTemplate,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageArtifactKind {
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
            Self::Pipeline { .. } => PackageArtifactKind::Pipeline,
            Self::Role { .. } => PackageArtifactKind::Role,
            Self::Preset { .. } => PackageArtifactKind::Preset,
            Self::Subflow { .. } => PackageArtifactKind::Subflow,
            Self::Command { .. } => PackageArtifactKind::Command,
            Self::McpTool { .. } => PackageArtifactKind::McpTool,
        }
    }
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
