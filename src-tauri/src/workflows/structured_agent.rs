use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    conversations::SessionScope,
    providers::{Provider, SessionMode},
    store::Store,
};

use super::{
    ArtifactValue, NodeConfiguration, PinnedNode, WorkflowArtifact,
    data_nodes::{MAX_DATA_BYTES, ValidateOutputConfiguration},
    role_templates::{ConversationTemplate, TemplateReviewPolicy},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredAgentConfiguration {
    pub prompt: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub effective_role: ConversationTemplate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_role: Option<ConversationTemplate>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_verdict: bool,
}

impl StructuredAgentConfiguration {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.prompt.trim().is_empty() && self.prompt.len() <= MAX_DATA_BYTES,
            "Enter a Prompt within the workflow data size limit"
        );
        for schema in [&self.input_schema, &self.output_schema] {
            ensure!(
                schema["type"] == "object",
                "Choose an object Schema for the Prompt"
            );
            ValidateOutputConfiguration {
                schema: schema.clone(),
            }
            .validate()?;
        }
        let mut effective_role = self.effective_role.clone();
        super::role_templates::validate_conversation_template(&mut effective_role)?;
        ensure!(
            effective_role.requirements.is_valid(),
            "The effective Role requirements are invalid"
        );
        match (&self.effective_role.review_policy, &self.reviewer_role) {
            (TemplateReviewPolicy::CrossReview, Some(reviewer)) => {
                super::role_templates::validate_conversation_template(&mut reviewer.clone())?;
                ensure!(
                    reviewer.tool_scope == SessionMode::ReadOnly
                        && reviewer.review_policy == TemplateReviewPolicy::Independent
                        && reviewer.requirements.is_valid(),
                    "Choose a read-only reviewer with an independent review policy"
                );
            }
            (TemplateReviewPolicy::CrossReview, None) => {
                anyhow::bail!("Choose a reviewer Role for cross-review")
            }
            (_, Some(_)) => anyhow::bail!("A reviewer Role requires cross-review"),
            _ => {}
        }
        self.parts().map(|_| ())
    }

    fn parts(&self) -> Result<Vec<Part<'_>>> {
        let mut parts = Vec::new();
        let mut remaining = self.prompt.as_str();
        while let Some((literal, placeholder)) = remaining.split_once("{{") {
            if let Some(literal) = literal.strip_suffix('\\') {
                parts.push(Part::Literal(literal));
                parts.push(Part::Literal("{{"));
                remaining = placeholder;
                continue;
            }
            parts.push(Part::Literal(literal));
            let (expression, next) = placeholder
                .split_once("}}")
                .context("Close the Prompt variable with }}")?;
            let expression = expression.trim();
            let mut path = expression.split('.');
            ensure!(
                path.next() == Some("input"),
                "Unknown Prompt variable {expression:?}"
            );
            let mut schema = &self.input_schema;
            let mut access = Vec::new();
            for segment in path {
                schema = schema
                    .get("properties")
                    .and_then(|fields| fields.get(segment))
                    .with_context(|| format!("Unknown Prompt variable {expression:?}"))?;
                access.push(segment);
            }
            parts.push(Part::Input(access));
            remaining = next;
        }
        parts.push(Part::Literal(remaining));
        Ok(parts)
    }

    pub(super) fn render(&self, artifact: Option<&WorkflowArtifact>) -> Result<String> {
        let Some(ArtifactValue::JsonObject(input)) = artifact.map(|artifact| &artifact.value)
        else {
            anyhow::bail!("The AI Node needs a JSON object on its input port")
        };
        self.render_json(&Value::Object(input.clone()))
    }

    fn render_json(&self, input: &Value) -> Result<String> {
        super::schema::validate(&self.input_schema, input).context("Invalid AI Node input")?;
        let mut output = String::with_capacity(self.prompt.len());
        for part in self.parts()? {
            match part {
                Part::Literal(text) => output.push_str(text),
                Part::Input(path) => {
                    let value = path
                        .into_iter()
                        .try_fold(input, |value, field| value.get(field))
                        .unwrap_or(&Value::Null);
                    if let Some(text) = value.as_str() {
                        output.push_str(text);
                    } else {
                        output.push_str(&serde_json::to_string(value)?);
                    }
                }
            }
            ensure!(
                output.len() <= MAX_DATA_BYTES,
                "The rendered Prompt exceeds the workflow data size limit"
            );
        }
        Ok(output)
    }

    pub(super) fn instructions(&self, prompt: &str) -> String {
        super::authoring::package::roles::role_instructions(&self.effective_role, prompt)
    }

    pub(super) fn require_scope(&self, scope: &SessionScope) -> Result<()> {
        let role = &self.effective_role;
        ensure!(
            scope.provider == role.model.provider
                && scope.profile_id == role.model.profile_id
                && scope.model.as_deref() == Some(role.model.model_id.as_str())
                && scope.effort == role.effort
                && scope.mode == role.tool_scope,
            "The execution profile differs from the effective AI Node Role"
        );
        Ok(())
    }

    pub(super) fn output(&self, text: &str) -> Result<Map<String, Value>> {
        ensure!(
            text.len() <= MAX_DATA_BYTES,
            "The AI Node result exceeds the data size limit"
        );
        let mut value: Value = serde_json::from_str(text)
            .map_err(|_| anyhow::anyhow!("The AI Node did not return one JSON object"))?;
        if self.effective_role.model.provider == Provider::Codex {
            normalize_optional(&self.output_schema, &mut value);
        }
        super::schema::validate(&self.output_schema, &value).context("Invalid AI Node result")?;
        let Value::Object(result) = value else {
            anyhow::bail!("The AI Node did not return a JSON object")
        };
        Ok(result)
    }
}

enum Part<'a> {
    Literal(&'a str),
    Input(Vec<&'a str>),
}

fn required(schema: &Value, name: &str) -> bool {
    schema["required"]
        .as_array()
        .is_some_and(|required| required.iter().any(|value| value == name))
}

fn normalize_optional(schema: &Value, value: &mut Value) {
    if let (Some(properties), Some(fields)) =
        (schema["properties"].as_object(), value.as_object_mut())
    {
        for (name, child) in properties {
            if !required(schema, name) && fields.get(name).is_some_and(Value::is_null) {
                fields.remove(name);
            } else if let Some(value) = fields.get_mut(name) {
                normalize_optional(child, value);
            }
        }
    } else if let Some(items) = value.as_array_mut() {
        for item in items {
            normalize_optional(&schema["items"], item);
        }
    }
}

fn codex_schema(schema: &Value) -> Value {
    let mut strict = schema.clone();
    if let Some(properties) = schema["properties"].as_object() {
        let mut wire_fields = Map::new();
        for (name, child) in properties {
            let mut wire = codex_schema(child);
            if !required(schema, name) {
                wire["type"] = serde_json::json!([wire["type"], "null"]);
            }
            wire_fields.insert(name.clone(), wire);
        }
        strict["required"] = serde_json::json!(properties.keys().collect::<Vec<_>>());
        strict["properties"] = Value::Object(wire_fields);
    } else if let Some(item) = schema.get("items") {
        strict["items"] = codex_schema(item);
    }
    strict
}

pub(super) fn configuration(node: &PinnedNode) -> Option<&StructuredAgentConfiguration> {
    match &node.instance.configuration {
        NodeConfiguration::AgentRole { requirements, .. } => requirements.structured.as_ref(),
        _ => None,
    }
}

impl Store {
    pub(crate) async fn workflow_output_schema_for_attempt(
        &self,
        run: String,
    ) -> Result<Option<Value>> {
        self.execute(move |db| {
            let origin: Option<(String, String, String)> = db
                .query_row(
                    "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id
                 FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
                 JOIN workflow_job_inputs i ON i.job_id = o.job_id WHERE a.run_id = ?1",
                    params![run],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((project, workflow, node)) = origin else {
                return Ok(None);
            };
            let pinned = super::runtime::load_pinned_node(db, &project, &workflow, &node)?
                .context("The workflow attempt lost its pinned node")?;
            Ok(configuration(&pinned).map(|config| {
                if config.effective_role.model.provider == Provider::Codex {
                    codex_schema(&config.output_schema)
                } else {
                    config.output_schema.clone()
                }
            }))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_declared_input_and_normalizes_codex_optional_output() -> Result<()> {
        let nested = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "tag": {"type": "string"}
            },
            "required": ["name"],
            "additionalProperties": false
        });
        let schema = json!({
            "type": "object",
            "properties": {"item": nested},
            "required": ["item"],
            "additionalProperties": false
        });
        let mut config = StructuredAgentConfiguration {
            prompt: r"Summarize {{input.item.name}} ({{ input.item.tag }}) from {{input}}. Use \{{example}}."
                .into(),
            input_schema: schema.clone(),
            output_schema: schema.clone(),
            effective_role: serde_json::from_value(json!({
                "name": "Summarizer",
                "role": "agent",
                "model": {"provider": "codex", "profileId": "example", "modelId": "example-model"},
                "toolScope": "read_only",
                "reviewPolicy": "independent",
                "instructions": "Summarize the input.",
                "completionContract": "Return a structured result."
            }))?,
            reviewer_role: None,
            review_verdict: false,
        };
        config.validate()?;
        assert_eq!(
            config.render_json(&json!({"item":{"name":"Report"}}))?,
            r#"Summarize Report (null) from {"item":{"name":"Report"}}. Use {{example}}."#
        );
        assert!(config.render_json(&json!({"item":{"name":5}})).is_err());
        let wire = codex_schema(&schema);
        let item = &wire["properties"]["item"];
        assert_eq!(item["required"], json!(["name", "tag"]));
        assert_eq!(item["properties"]["tag"]["type"], json!(["string", "null"]));
        assert_eq!(
            Value::Object(config.output(r#"{"item":{"name":"Report","tag":null}}"#)?),
            json!({"item":{"name":"Report"}})
        );
        assert!(
            config
                .output(r#"{"item":{"name":null,"tag":null}}"#)
                .is_err()
        );
        assert!(
            config
                .output(r#"{"item":{"name":"Report","extra":5}}"#)
                .is_err()
        );
        config.prompt = "{{input.unknown}}".into();
        assert!(config.validate().is_err());
        Ok(())
    }
}
