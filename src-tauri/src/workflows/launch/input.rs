use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::conversations::MessageRevision;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStartSource {
    Message,
    History,
}

impl ConversationStartSource {
    fn value_type(self) -> WorkflowStartValueType {
        match self {
            Self::Message => WorkflowStartValueType::Text,
            Self::History => WorkflowStartValueType::Array,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStartField {
    name: String,
    value_type: WorkflowStartValueType,
    required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conversation_source: Option<ConversationStartSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TypedStartConfiguration {
    #[serde(default)]
    fields: Vec<WorkflowStartField>,
    #[serde(rename = "promptOrMarkdown", default, skip_serializing_if = "is_false")]
    prompt_or_markdown: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) conversation: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl TypedStartConfiguration {
    pub(crate) fn has_fields(&self) -> bool {
        !self.fields.is_empty()
    }

    pub(crate) fn accepts_automation(&self) -> bool {
        !self.prompt_or_markdown && self.fields.iter().all(|field| !field.required)
    }

    pub(crate) fn prompt_or_markdown(&self) -> bool {
        self.prompt_or_markdown
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
            ensure!(
                !self.prompt_or_markdown || !matches!(field.name.as_str(), "prompt" | "request"),
                "The Start request reserves the prompt and request field names"
            );
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
            if let Some(source) = field.conversation_source {
                ensure!(
                    field.value_type == source.value_type(),
                    "Conversation-bound Start input {:?} must be {}",
                    field.name,
                    source.value_type().label()
                );
            }
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
                self.fields.iter().any(|field| &field.name == name)
                    || self.prompt_or_markdown && name == "prompt",
                "Start input {name:?} is not declared by this revision"
            );
        }
        Ok(())
    }

    pub(crate) fn launch_values(
        &self,
        submitted: &Map<String, Value>,
        markdown: Option<&str>,
    ) -> Result<Map<String, Value>> {
        self.validate_values(submitted)?;
        let mut values = submitted.clone();
        if self.prompt_or_markdown {
            let prompt = values.remove("prompt");
            let request = match (prompt.as_ref(), markdown) {
                (Some(Value::String(prompt)), None) => prompt.as_str(),
                (None, Some(markdown)) => markdown,
                _ => anyhow::bail!("Provide either a prompt or a saved Markdown revision"),
            };
            ensure!(!request.trim().is_empty(), "The Start request is empty");
            ensure!(
                request.len() <= MAX_START_BYTES,
                "The Start request exceeds the workflow input size limit"
            );
            values.insert("request".into(), Value::String(request.into()));
        }
        Ok(values)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStartSchema {
    #[serde(flatten)]
    configuration: TypedStartConfiguration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) json_schema: Option<Value>,
    pub(crate) requires_document: bool,
    pub(crate) requires_work: bool,
    pub(crate) requires_event: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) requires_backlog_plan: bool,
}

impl WorkflowStartSchema {
    pub(crate) fn for_definition(definition: &WorkflowDefinition) -> Option<Self> {
        let (node, start) =
            definition
                .nodes
                .iter()
                .find_map(|node| match &node.instance.configuration {
                    NodeConfiguration::TypedStart(start) => Some((node, start)),
                    _ => None,
                })?;
        let wired = |port: &str| {
            definition
                .wires
                .iter()
                .any(|wire| wire.from.node == node.instance.id && wire.from.port == port)
        };
        let json_schema = node.definition.output_schemas.get("inputs").cloned();
        let mut configuration = start.clone();
        if let Some(schema) = &json_schema
            && let Some(properties) = schema["properties"].as_object()
        {
            configuration.fields = properties
                .iter()
                .filter_map(|(name, property)| {
                    let value_type = match property["type"].as_str()? {
                        "string" => WorkflowStartValueType::Text,
                        "number" | "integer" => WorkflowStartValueType::Number,
                        "boolean" => WorkflowStartValueType::Boolean,
                        "null" => WorkflowStartValueType::Null,
                        "object" => WorkflowStartValueType::Object,
                        "array" => WorkflowStartValueType::Array,
                        _ => return None,
                    };
                    let conversation_source = match (start.conversation, name.as_str(), value_type)
                    {
                        (true, "message", WorkflowStartValueType::Text) => {
                            Some(ConversationStartSource::Message)
                        }
                        (true, "history", WorkflowStartValueType::Array) => {
                            Some(ConversationStartSource::History)
                        }
                        _ => None,
                    };
                    Some(WorkflowStartField {
                        name: name.clone(),
                        required: schema["required"].as_array().is_some_and(|fields| {
                            fields.iter().any(|field| field.as_str() == Some(name))
                        }),
                        value_type,
                        conversation_source,
                    })
                })
                .collect();
        }
        Some(Self {
            configuration,
            json_schema,
            requires_document: wired("document"),
            requires_work: wired("task"),
            requires_event: wired("event"),
            requires_backlog_plan: wired("plan"),
        })
    }

    pub(crate) fn conversation_enabled(&self) -> bool {
        self.configuration.conversation
            && self
                .configuration
                .fields
                .iter()
                .any(|field| field.conversation_source == Some(ConversationStartSource::Message))
    }

    pub(crate) fn launch_values(
        &self,
        submitted: &Map<String, Value>,
        markdown: Option<&str>,
    ) -> Result<Map<String, Value>> {
        if let Some(schema) = &self.json_schema {
            ensure!(
                serde_json::to_vec(submitted)?.len() <= MAX_START_BYTES,
                "The submitted inputs exceed the workflow input size limit"
            );
            crate::workflows::schema::validate(schema, &Value::Object(submitted.clone()))?;
            return Ok(submitted.clone());
        }
        self.configuration.launch_values(submitted, markdown)
    }

    pub(crate) fn accepts_automation(&self) -> bool {
        !self.requires_backlog_plan && self.configuration.accepts_automation()
    }

    pub(crate) fn validate_backlog_child(&self) -> Result<()> {
        ensure!(
            !self.requires_document
                && !self.requires_event
                && !self.configuration.prompt_or_markdown,
            "Backlog children start with the dispatched task and cannot require another launch source"
        );
        ensure!(
            self.requires_backlog_plan && self.requires_work,
            "Connect the child's Start plan and task outputs to its delivery graph"
        );
        self.configuration.validate_values(&Map::from_iter([(
            "backlog".into(),
            Value::Object(Map::new()),
        )]))
    }

    pub(crate) fn prompt_or_markdown(&self) -> bool {
        self.configuration.prompt_or_markdown()
    }

    pub(crate) fn conversation_inputs(
        &self,
        mut values: Map<String, Value>,
        message: &str,
        history: &[MessageRevision],
    ) -> Result<Map<String, Value>> {
        let mut history_fields = Vec::new();
        for field in &self.configuration.fields {
            let Some(source) = field.conversation_source else {
                continue;
            };
            ensure!(
                !values.contains_key(&field.name),
                "Start input {:?} is supplied by the conversation",
                field.name
            );
            let value = match source {
                ConversationStartSource::Message => Value::String(message.into()),
                ConversationStartSource::History => {
                    history_fields.push(field.name.clone());
                    Value::Array(Vec::new())
                }
            };
            values.insert(field.name.clone(), value);
        }
        if self.json_schema.is_some() {
            ensure!(
                serde_json::to_vec(&values)?.len() <= MAX_START_BYTES,
                "The submitted inputs exceed the workflow input size limit"
            );
        } else {
            self.configuration.validate_values(&values)?;
        }
        if !history_fields.is_empty() {
            // Keep complete recent messages, counting separators as well as escaped JSON.
            let fields_budget =
                (MAX_START_BYTES - serde_json::to_vec(&values)?.len()) / history_fields.len();
            let mut used = 0;
            let mut recent = Vec::new();
            for entry in history.iter().rev() {
                let remaining = fields_budget - used;
                if entry.text.len() > remaining {
                    break;
                }
                let item = json!({"role": entry.role, "text": entry.text});
                let bytes = serde_json::to_vec(&item)?.len() + usize::from(!recent.is_empty());
                if bytes > remaining {
                    break;
                }
                used += bytes;
                recent.push(item);
            }
            recent.reverse();
            for field in history_fields {
                values.insert(field, Value::Array(recent.clone()));
            }
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::MessageRole;

    fn schema(fields: Value) -> WorkflowStartSchema {
        WorkflowStartSchema {
            configuration: serde_json::from_value(json!({ "fields": fields })).unwrap(),
            json_schema: None,
            requires_document: false,
            requires_work: false,
            requires_event: false,
            requires_backlog_plan: false,
        }
    }

    fn message(role: MessageRole, text: &str) -> MessageRevision {
        MessageRevision {
            message_id: String::new(),
            revision_id: String::new(),
            role,
            text: text.into(),
        }
    }

    #[test]
    fn conversation_sources_are_additive_and_require_compatible_types() {
        let original = json!([{ "name": "attempts", "valueType": "number", "required": true }]);
        let manual = schema(original.clone());
        assert!(manual.configuration.valid());
        assert_eq!(serde_json::to_value(&manual).unwrap()["fields"], original);

        for (source, invalid_type) in [("message", "number"), ("history", "text")] {
            let invalid = schema(json!([{
                "name": "context", "valueType": invalid_type, "required": true, "conversationSource": source
            }]));
            assert!(!invalid.configuration.valid());
        }

        let bound = schema(json!([{
            "name": "prompt", "valueType": "text", "required": true, "conversationSource": "message"
        }]));
        let manual_values = Map::from_iter([("prompt".into(), json!("manual input"))]);
        assert!(bound.configuration.validate_values(&manual_values).is_ok());
        assert!(
            bound
                .conversation_inputs(manual_values, "actual", &[])
                .is_err()
        );
    }

    #[test]
    fn conversation_bound_inputs_use_the_message_and_complete_recent_history() -> Result<()> {
        let binding = schema(json!([
            { "name": "prompt", "valueType": "text", "required": true, "conversationSource": "message" },
            { "name": "history", "valueType": "array", "required": true, "conversationSource": "history" },
            { "name": "history_copy", "valueType": "array", "required": false, "conversationSource": "history" },
            { "name": "attempts", "valueType": "number", "required": true }
        ]));
        let history = [
            message(MessageRole::User, &"\"".repeat(MAX_START_BYTES / 3)),
            message(MessageRole::User, "Earlier task"),
            message(MessageRole::Assistant, "Earlier result"),
        ];
        let manual_values = Map::from_iter([("attempts".into(), json!(2))]);
        let resolved = binding.conversation_inputs(manual_values, "Current request", &history)?;
        assert_eq!(resolved["prompt"], json!("Current request"));
        assert_eq!(resolved["attempts"], json!(2));
        assert_eq!(
            resolved["history"],
            json!([
                { "role": "user", "text": "Earlier task" },
                { "role": "assistant", "text": "Earlier result" },
            ])
        );
        assert_eq!(resolved["history_copy"], resolved["history"]);
        assert!(
            binding
                .conversation_inputs(Map::new(), "Current request", &history)
                .is_err()
        );
        Ok(())
    }
}
