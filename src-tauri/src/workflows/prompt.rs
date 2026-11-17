use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ArtifactKind, ArtifactValue, BuiltinNode, NodeDefinition, Port, PortType};

const MAX_CONTRACT_ENTRIES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
const MAX_DETAIL_BYTES: usize = 4 * 1024;

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptDecisionConfiguration {
    pub routes: Vec<String>,
    pub delegates: BTreeMap<String, bool>,
    pub output_schema: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    route: String,
    reason: String,
    outputs: Map<String, Value>,
    delegations: Vec<Delegation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Delegation {
    role: String,
    focus: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptDecision {
    pub route: String,
    pub reason: String,
    pub outputs: Map<String, Value>,
    pub selected: BTreeMap<String, String>,
}

impl PromptDecisionConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=MAX_CONTRACT_ENTRIES).contains(&self.routes.len())
            && self
                .routes
                .iter()
                .all(|route| valid_identifier(route) && route != "falhou")
            && self.routes.iter().collect::<BTreeSet<_>>().len() == self.routes.len()
            && self.delegates.len() <= MAX_CONTRACT_ENTRIES
            && self.delegates.keys().all(|alias| valid_identifier(alias))
            && super::data_nodes::ValidateOutputConfiguration {
                schema: self.output_schema.clone(),
            }
            .validate()
            .is_ok()
    }

    pub(crate) fn decide(&self, value: Value) -> Result<PromptDecision> {
        ensure!(self.valid(), "The published Prompt contract is invalid");
        ensure!(
            serde_json::to_vec(&value)?.len() <= MAX_RESPONSE_BYTES,
            "The Prompt response exceeds the data limit"
        );
        let response: Response = serde_json::from_value(value)
            .context("Expected a Prompt decision with route, reason, outputs, and delegations")?;
        ensure!(
            self.routes.contains(&response.route),
            "The response selected an unknown Prompt route"
        );
        ensure!(
            valid_detail(&response.reason),
            "The Prompt decision needs a bounded justification"
        );
        ensure!(
            response.delegations.len() <= self.delegates.len(),
            "Too many Prompt calls were selected"
        );
        let outputs = super::data_nodes::ValidateOutputConfiguration {
            schema: self.output_schema.clone(),
        }
        .evaluate(Value::Object(response.outputs))?;
        let mut selected = BTreeMap::new();
        for Delegation { role, focus } in response.delegations {
            ensure!(
                self.delegates.contains_key(&role),
                "The response selected an unknown Prompt role"
            );
            ensure!(
                valid_detail(&focus),
                "A selected Prompt role needs a bounded focus"
            );
            ensure!(
                selected.insert(role, focus).is_none(),
                "A Prompt role was selected more than once"
            );
        }
        for (alias, required) in &self.delegates {
            if *required {
                selected.entry(alias.clone()).or_insert_with(|| {
                    "Review the supplied subject using the required role's responsibilities.".into()
                });
            }
        }
        Ok(PromptDecision {
            route: response.route,
            reason: response.reason,
            outputs,
            selected,
        })
    }
}

fn valid_detail(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_DETAIL_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptOutcomeConfiguration {
    pub outputs: BTreeMap<String, ArtifactKind>,
}

impl PromptOutcomeConfiguration {
    pub(super) fn valid(&self) -> bool {
        self.outputs.len() <= MAX_CONTRACT_ENTRIES
            && self
                .outputs
                .iter()
                .all(|(name, kind)| valid_identifier(name) && structured_kind(*kind))
    }

    pub(crate) fn definition(&self, base: &NodeDefinition) -> Result<NodeDefinition> {
        ensure!(
            base.kind == BuiltinNode::PromptOutcome && self.valid(),
            "Invalid Prompt outputs"
        );
        let mut definition = base.clone();
        definition
            .outputs
            .extend(self.outputs.iter().map(|(name, kind)| Port {
                name: format!("value-{name}"),
                value: PortType::Data(*kind),
            }));
        Ok(definition)
    }

    pub(crate) fn project(
        &self,
        decision: &PromptDecision,
        source: &str,
    ) -> Result<Vec<(String, ArtifactValue)>> {
        ensure!(self.valid(), "Invalid Prompt outputs");
        ensure!(
            decision.outputs.len() == self.outputs.len(),
            "The Prompt response does not match its published outputs"
        );
        self.outputs
            .iter()
            .map(|(name, kind)| {
                let value = decision
                    .outputs
                    .get(name)
                    .with_context(|| format!("Prompt output {name} is missing"))?;
                let artifact = match (kind, value) {
                    (ArtifactKind::JsonObject, Value::Object(value)) => {
                        ArtifactValue::JsonObject(value.clone())
                    }
                    (ArtifactKind::JsonList, Value::Array(values)) => {
                        ArtifactValue::JsonList(super::iteration::WorkflowJsonList::new(
                            values.clone(),
                            Some(source.into()),
                        )?)
                    }
                    (ArtifactKind::RenderedText, Value::String(text)) => {
                        ArtifactValue::RenderedText(text.clone())
                    }
                    _ => anyhow::bail!("Prompt output {name} has the wrong data type"),
                };
                Ok((format!("value-{name}"), artifact))
            })
            .collect()
    }
}

pub(crate) fn structured_kind(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::JsonObject | ArtifactKind::JsonList | ArtifactKind::RenderedText
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptCallStatus {
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptCall {
    pub role: String,
    pub focus: Option<String>,
    pub status: PromptCallStatus,
    pub artifact_id: Option<String>,
    pub stage_id: String,
    pub attempt_id: Option<String>,
    pub approved: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PromptOutcome {
    pub route: String,
    pub reason: String,
    pub outputs: Map<String, Value>,
    pub selected: BTreeMap<String, String>,
    pub calls: Vec<PromptCall>,
    pub diagnostic: Option<String>,
    pub approved: bool,
}

impl PromptOutcome {
    pub(crate) fn assemble(decision: PromptDecision, calls: Vec<PromptCall>) -> Self {
        let missing = decision.selected.keys().any(|role| {
            !calls
                .iter()
                .any(|call| call.role == *role && call.status == PromptCallStatus::Completed)
        });
        let unexpected = calls.iter().any(|call| {
            !decision.selected.contains_key(&call.role) && call.status != PromptCallStatus::Skipped
        });
        let diagnostic = if missing {
            Some("A selected Prompt role did not produce a result".into())
        } else if unexpected {
            Some("A Prompt role ran outside the recorded selection".into())
        } else {
            None
        };
        let approved = diagnostic.is_none()
            && !decision.selected.is_empty()
            && calls
                .iter()
                .filter(|call| decision.selected.contains_key(&call.role))
                .all(|call| call.approved == Some(true));
        Self {
            route: if diagnostic.is_some() {
                "falhou".into()
            } else {
                decision.route
            },
            reason: decision.reason,
            outputs: decision.outputs,
            selected: decision.selected,
            calls,
            diagnostic,
            approved,
        }
    }
}

pub(crate) fn stage_alias(node: &str) -> Option<&str> {
    node.rsplit("::")
        .next()?
        .strip_prefix("delegate-")
        .filter(|alias| valid_identifier(alias))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn contract() -> PromptDecisionConfiguration {
        PromptDecisionConfiguration {
            routes: vec!["entregar".into(), "corrigir".into()],
            delegates: BTreeMap::from([
                ("correctness".into(), true),
                ("performance".into(), false),
                ("accessibility".into(), false),
            ]),
            output_schema: json!({
                "type": "object", "properties": { "summary": {"type": "string"} },
                "required": ["summary"], "additionalProperties": false,
            }),
        }
    }

    #[test]
    fn validates_routes_outputs_and_roles_before_any_specialist_is_selected() {
        let base = json!({
            "route": "entregar", "reason": "The candidate meets the requested changes.",
            "outputs": {"summary":"Candidate ready."}, "delegations": [],
        });
        let safe = contract().decide(base.clone()).unwrap();
        assert_eq!(safe.selected.keys().collect::<Vec<_>>(), ["correctness"]);

        let mut risky = base.clone();
        risky["delegations"] = json!([{"role": "performance", "focus": "Inspect query cost."}]);
        let risky = contract().decide(risky).unwrap();
        assert_eq!(
            risky.selected.keys().collect::<Vec<_>>(),
            ["correctness", "performance"]
        );

        for (key, value) in [
            ("route", json!("unknown")),
            ("route", json!("falhou")),
            ("outputs", json!({})),
            ("outputs", json!({"summary":3})),
            (
                "delegations",
                json!([{"role":"invented","focus":"anything"}]),
            ),
            ("delegations", json!([{"role":"performance","focus":"  "}])),
            (
                "delegations",
                json!([{"role":"performance","focus":"One"},{"role":"performance","focus":"Two"}]),
            ),
        ] {
            let mut response = base.clone();
            response[key] = value;
            assert!(
                contract().decide(response).is_err(),
                "{key} must be validated"
            );
        }
    }

    #[test]
    fn generated_ports_are_typed_and_cannot_emit_native_provenance() {
        let kind = BuiltinNode::PromptOutcome;
        let base = super::super::builtin_registry()
            .into_iter()
            .find(|entry| entry.kind == kind)
            .unwrap();
        let outputs = PromptOutcomeConfiguration {
            outputs: BTreeMap::from([("summary".into(), ArtifactKind::RenderedText)]),
        };
        let pinned = outputs.definition(&base).unwrap();
        assert!(super::super::registry::supports_pinned_execution(&pinned));
        assert!(
            pinned
                .outputs
                .iter()
                .any(|port| port.name == "value-summary"
                    && port.value == PortType::Data(ArtifactKind::RenderedText))
        );
        let forbidden = PromptOutcomeConfiguration {
            outputs: BTreeMap::from([("candidate".into(), ArtifactKind::Candidate)]),
        };
        assert!(forbidden.definition(&base).is_err());
    }
}
