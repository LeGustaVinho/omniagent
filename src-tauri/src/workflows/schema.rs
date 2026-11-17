use std::collections::{BTreeMap, HashMap, VecDeque};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BuiltinNode, NodeConfiguration, NodeDefinition, Wire, WorkflowGraph};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaField {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    pub required: bool,
    #[serde(flatten)]
    shape: SchemaType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum SchemaType {
    String,
    Boolean,
    Integer,
    Number,
    Object { fields: Vec<SchemaField> },
    Array { items: Box<SchemaType> },
}

pub(super) fn object(fields: &[SchemaField]) -> Result<Value> {
    object_at(fields, 1, &mut 0)
}

fn object_at(fields: &[SchemaField], depth: usize, count: &mut usize) -> Result<Value> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    *count += fields.len();
    ensure!(*count <= 512, "A Schema can contain up to 512 fields");
    for field in fields {
        ensure!(
            super::prompt::valid_identifier(&field.name),
            "Invalid Schema field name {:?}",
            field.name
        );
        ensure!(
            field.description.len() <= 2048,
            "Schema field description is too long"
        );
        let mut property = field.shape.json(depth + 1, count)?;
        if !field.description.is_empty() {
            property["description"] = json!(field.description);
        }
        ensure!(
            properties.insert(field.name.clone(), property).is_none(),
            "Repeated Schema field {:?}",
            field.name
        );
        if field.required {
            required.push(&field.name);
        }
    }
    Ok(json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    }))
}

impl SchemaType {
    fn json(&self, depth: usize, count: &mut usize) -> Result<Value> {
        ensure!(depth <= 10, "A Schema can have up to 10 nested levels");
        Ok(match self {
            Self::String => json!({"type": "string"}),
            Self::Boolean => json!({"type": "boolean"}),
            Self::Integer => json!({"type": "integer"}),
            Self::Number => json!({"type": "number"}),
            Self::Object { fields } => object_at(fields, depth, count)?,
            Self::Array { items } => {
                json!({"type": "array", "items": items.json(depth + 1, count)?})
            }
        })
    }
}

pub(crate) fn validate(schema: &Value, value: &Value) -> Result<()> {
    let validator = jsonschema::validator_for(schema).context("Invalid workflow Schema")?;
    if let Err(error) = validator.validate(value) {
        // Keep the path and violation category without leaking the rejected value into diagnostics.
        anyhow::bail!(
            "Data did not match the Schema at {} ({})",
            error.instance_path(),
            error.schema_path(),
        );
    }
    Ok(())
}

pub(super) fn compatible(source: &Value, target: &Value) -> Result<()> {
    check(source, target, "input")
}

fn check(source: &Value, target: &Value, path: &str) -> Result<()> {
    let source_type = source.get("type").and_then(Value::as_str);
    let target_type = target.get("type").and_then(Value::as_str);
    if target_type.is_none() {
        return Ok(());
    }
    ensure!(
        source_type == target_type
            || (source_type == Some("integer") && target_type == Some("number")),
        "Incompatible Schema type at {path}"
    );
    match target_type {
        Some("object") => {
            let is_required = |schema: &Value, name: &str| {
                schema["required"]
                    .as_array()
                    .is_some_and(|names| names.iter().any(|value| value.as_str() == Some(name)))
            };
            if let Some(required) = target["required"].as_array() {
                for name in required.iter().filter_map(Value::as_str) {
                    ensure!(
                        is_required(source, name),
                        "Source must require {path}.{name}"
                    );
                }
            }
            let closed = target["additionalProperties"] == false;
            ensure!(
                !closed || source["additionalProperties"] == false,
                "Source must declare every field at {path}"
            );
            if let Some(properties) = source["properties"].as_object() {
                for (name, property) in properties {
                    if let Some(expected) = target["properties"].get(name) {
                        check(property, expected, &format!("{path}.{name}"))?;
                    } else {
                        ensure!(!closed, "Destination has no field {path}.{name}");
                    }
                }
            }
        }
        Some("array") => check(&source["items"], &target["items"], &format!("{path}[]"))?,
        _ => {}
    }
    Ok(())
}

pub(super) fn infer_ports(
    graph: &WorkflowGraph,
    definitions: &mut BTreeMap<String, NodeDefinition>,
) {
    let mut incoming = HashMap::<&str, Vec<&Wire>>::new();
    let mut edges = HashMap::<&str, Vec<&str>>::new();
    let mut remaining: HashMap<_, usize> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), 0))
        .collect();
    for wire in &graph.wires {
        incoming.entry(&wire.to.node).or_default().push(wire);
        edges
            .entry(&wire.from.node)
            .or_default()
            .push(&wire.to.node);
        if let Some(count) = remaining.get_mut(wire.to.node.as_str()) {
            *count += 1;
        }
    }
    let mut ready: VecDeque<_> = graph
        .nodes
        .iter()
        .filter(|node| remaining[node.id.as_str()] == 0)
        .map(|node| node.id.as_str())
        .collect();
    let nodes: HashMap<_, _> = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    while let Some(id) = ready.pop_front() {
        let node = nodes[id];
        let wires = incoming.get(id).map(Vec::as_slice).unwrap_or_default();
        let contract = |name: &str| {
            let source = &wires.iter().find(|wire| wire.to.port == name)?.from;
            definitions
                .get(&source.node)?
                .output_schemas
                .get(&source.port)
                .cloned()
        };
        let input = match node.kind {
            BuiltinNode::End => contract("result"),
            BuiltinNode::Fork | BuiltinNode::Wait | BuiltinNode::Human => contract("input"),
            _ => None,
        };
        let derived = match &node.configuration {
            NodeConfiguration::Human(config) => {
                Some(("response", config.response_schema(input.as_ref())))
            }
            NodeConfiguration::JoinResults(config) => {
                let branches = config.branches.iter().map(|name| {
                    let mut properties = Map::from_iter([
                        ("status".into(), json!({"type":"string"})),
                        ("error".into(), json!({"type":"string"})),
                    ]);
                    let result = format!("result:{name}");
                    if wires.iter().any(|wire| wire.to.port == result) {
                        properties.insert("result".into(), contract(&result).unwrap_or_else(|| json!({"type":"object"})));
                    }
                    (name.clone(), json!({"type":"object", "properties":properties, "required":["status"], "additionalProperties":false}))
                }).collect::<Map<_, _>>();
                Some((
                    "results",
                    json!({
                        "type":"object", "properties":{"branches":{"type":"object","properties":branches,"additionalProperties":false}},
                        "required":["branches"],"additionalProperties":false
                    }),
                ))
            }
            _ => None,
        };
        if let Some(definition) = definitions.get_mut(id) {
            if let Some((name, derived)) = derived {
                definition.output_schemas.insert(name.into(), derived);
            }
            if let Some(input) = input {
                let port = if node.kind == BuiltinNode::End {
                    "result"
                } else {
                    "input"
                };
                definition
                    .input_schemas
                    .entry(port.into())
                    .or_insert_with(|| input.clone());
                for output in &definition.outputs {
                    if (node.kind == BuiltinNode::Fork && output.name.starts_with("context:"))
                        || (matches!(node.kind, BuiltinNode::End | BuiltinNode::Wait)
                            && output.name == "result")
                        || (node.kind == BuiltinNode::Human && output.name == "approved_result")
                    {
                        definition
                            .output_schemas
                            .entry(output.name.clone())
                            .or_insert_with(|| input.clone());
                    }
                }
            }
        }
        for target in edges.get(id).into_iter().flatten() {
            if let Some(count) = remaining.get_mut(target) {
                *count -= 1;
                if *count == 0 {
                    ready.push_back(target);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_validate_values_and_check_directional_compatibility() -> Result<()> {
        let make = |field_type: &str, required: bool| -> Result<Value> {
            let fields = serde_json::from_value::<Vec<SchemaField>>(json!([{
                "name": "score", "description": "Review score", "type": field_type, "required": required
            }]))?;
            object(&fields)
        };
        let source = make("integer", true)?;
        let target = make("number", false)?;
        compatible(&source, &target)?;
        assert!(compatible(&target, &source).is_err());
        validate(&source, &json!({"score": 7}))?;
        assert!(validate(&source, &json!({})).is_err());
        assert!(validate(&target, &json!({"score": 7, "other": 1})).is_err());
        Ok(())
    }
}
