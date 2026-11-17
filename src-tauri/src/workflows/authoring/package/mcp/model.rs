use serde::{Deserialize, Serialize};

use crate::workflows::{NodeInstance, Wire};

use super::*;

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "view",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum ReadCatalog {
    Draft {
        #[serde(default)]
        offset: usize,
    },
    DraftNodeDefinitions {
        #[serde(default)]
        offset: usize,
    },
    Published {
        #[serde(default)]
        offset: usize,
    },
    DraftMember {
        key: String,
        expected_revision: String,
        #[serde(default)]
        offset: usize,
    },
    PublishedMember {
        version: PackageArtifactVersion,
        #[serde(default)]
        offset: usize,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ApplyOperations {
    pub request_id: String,
    pub expected_revision: String,
    pub operations: Vec<DraftOperation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubmitForReview {
    pub request_id: String,
    pub expected_revision: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum DraftOperation {
    RenamePackage {
        name: String,
    },
    PutMember {
        member: Box<PackageMember>,
    },
    RemoveMember {
        key: String,
    },
    PutNode {
        member_key: String,
        node: NodeInstance,
    },
    RemoveNode {
        member_key: String,
        node_id: String,
    },
    PutWire {
        member_key: String,
        wire: Wire,
    },
    RemoveWire {
        member_key: String,
        wire: Wire,
    },
    SetReference {
        member_key: String,
        slot: String,
        #[serde(deserialize_with = "Option::deserialize")]
        reference: Option<PackageReference>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum AuthoringOutcome {
    Applied { revision: String },
    Submitted { revision: String },
    RevisionConflict { current_revision: String },
    RequestConflict,
    Rejected { message: String },
}
