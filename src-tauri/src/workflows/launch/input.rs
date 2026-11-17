use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::workflows::{NodeConfiguration, WorkflowDefinition};

const MAX_START_BYTES: usize = 192 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStartValueType {
    Text,
    Number,
    Boolean,
    Null,
    Object,
    Array,
}

impl WorkflowStartValueType {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Text => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Null => value.is_null(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "a number",
            Self::Boolean => "a Boolean",
            Self::Null => "null",
            Self::Object => "an object",
            Self::Array => "a list",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStartField {
    name: String,
    value_type: WorkflowStartValueType,
    required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypedStartConfiguration {
    fields: Vec<WorkflowStartField>,
}

impl TypedStartConfiguration {
    pub(crate) fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    pub(crate) fn accepts_automation(&self) -> bool {
        self.fields.iter().all(|field| !field.required)
    }

    pub(crate) fn valid(&self) -> bool {
        self.validate().is_ok()
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            serde_json::to_vec(&self.fields)?.len() <= MAX_START_BYTES,
            "The Start fields exceed the workflow input size limit"
        );
        let mut names = BTreeSet::new();
        for field in &self.fields {
            let mut chars = field.name.bytes();
            ensure!(
                !field.name.is_empty()
                    && field.name.len() <= 160
                    && chars
                        .next()
                        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                    && chars
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
                "Start field names use letters, digits, underscores and hyphens"
            );
            ensure!(
                names.insert(&field.name),
                "Repeated Start field {:?}",
                field.name
            );
        }
        Ok(())
    }

    pub(crate) fn validate_values(&self, values: &Map<String, Value>) -> Result<()> {
        self.validate()?;
        ensure!(
            serde_json::to_vec(values)?.len() <= MAX_START_BYTES,
            "The submitted inputs exceed the workflow input size limit"
        );
        for field in &self.fields {
            match values.get(&field.name) {
                Some(value) => ensure!(
                    field.value_type.accepts(value),
                    "Start input {:?} must be {}",
                    field.name,
                    field.value_type.label()
                ),
                None => ensure!(!field.required, "Start input {:?} is required", field.name),
            }
        }
        for name in values.keys() {
            ensure!(
                self.fields.iter().any(|field| &field.name == name),
                "Start input {name:?} is not declared by this revision"
            );
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStartSchema {
    #[serde(flatten)]
    configuration: TypedStartConfiguration,
    pub(crate) requires_document: bool,
    pub(crate) requires_work: bool,
    pub(crate) requires_event: bool,
}

impl WorkflowStartSchema {
    pub(crate) fn for_definition(definition: &WorkflowDefinition) -> Option<Self> {
        let (id, start) =
            definition
                .nodes
                .iter()
                .find_map(|node| match &node.instance.configuration {
                    NodeConfiguration::TypedStart(start) => Some((&node.instance.id, start)),
                    _ => None,
                })?;
        let wired = |port: &str| {
            definition
                .wires
                .iter()
                .any(|wire| &wire.from.node == id && wire.from.port == port)
        };
        Some(Self {
            configuration: start.clone(),
            requires_document: wired("document"),
            requires_work: wired("task"),
            requires_event: wired("event"),
        })
    }

    pub(crate) fn accepts_automation(&self) -> bool {
        self.configuration.accepts_automation()
    }
}
