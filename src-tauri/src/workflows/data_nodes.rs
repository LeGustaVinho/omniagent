use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ConditionPredicate, Endpoint};

pub(super) const MAX_DATA_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetFieldsMode {
    Build,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldValue {
    Literal { value: Value },
    Input { pointer: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldOperation {
    Set { path: String, value: FieldValue },
    Rename { from: String, path: String },
    ChooseObject { pointers: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetFieldsConfiguration {
    mode: SetFieldsMode,
    operations: Vec<FieldOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMapping {
    from: String,
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilterMapListConfiguration {
    #[serde(default)]
    predicate: Option<ConditionPredicate>,
    #[serde(default)]
    mappings: Vec<FieldMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTextConfiguration {
    template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEvidence {
    pub source: Endpoint,
    #[serde(flatten)]
    pub outcome: EvidenceOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum EvidenceOutcome {
    Completed { artifact_id: String, value: Value },
    Failed,
    Skipped,
}

pub(super) fn evidence_text(evidence: &[WorkflowEvidence]) -> Result<String> {
    let text = serde_json::to_string(evidence)?;
    ensure!(
        text.len() <= MAX_DATA_BYTES,
        "The evidence set exceeds the node data size limit"
    );
    Ok(text)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityGateConfiguration {
    pub rules: std::collections::BTreeMap<String, ConditionPredicate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidateOutputConfiguration {
    pub schema: Value,
}

struct InlineSchema;

impl jsonschema::Retrieve for InlineSchema {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> std::result::Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("Schemas must bundle their references".into())
    }
}

impl ValidateOutputConfiguration {
    pub(super) fn validate(&self) -> Result<jsonschema::Validator> {
        bounded_data(&self.schema)?;
        ensure!(self.schema.is_object(), "Set an output JSON Schema object");
        jsonschema::options()
            .with_retriever(InlineSchema)
            .build(&self.schema)
            .map_err(|_| {
                anyhow::anyhow!("The output JSON Schema is invalid or has an external reference")
            })
    }

    pub(super) fn parse(&self, text: &str) -> Result<Map<String, Value>> {
        ensure!(
            text.len() <= MAX_DATA_BYTES,
            "The output exceeds the node data size limit"
        );
        let value = serde_json::from_str(text)
            .map_err(|_| anyhow::anyhow!("The output must be one complete JSON object"))?;
        self.evaluate(value)
    }

    pub(super) fn evaluate(&self, value: Value) -> Result<Map<String, Value>> {
        bounded_data(&value)?;
        ensure!(
            value.is_object(),
            "The output must be one complete JSON object"
        );
        let policy = self.validate()?;
        ensure!(
            policy.is_valid(&value),
            "The output did not match the pinned JSON Schema"
        );
        let Value::Object(object) = value else {
            unreachable!("the output object was checked")
        };
        Ok(object)
    }
}

fn bounded_data(value: &impl Serialize) -> Result<()> {
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_DATA_BYTES,
        "The node data exceeds {MAX_DATA_BYTES} UTF-8 bytes"
    );
    Ok(())
}
