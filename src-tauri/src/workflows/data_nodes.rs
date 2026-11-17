use std::collections::BTreeSet;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ConditionPredicate, iteration::WorkflowJsonList};

const MAX_DATA_BYTES: usize = 192 * 1024;
const MAX_POINTER_BYTES: usize = 2048;

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
}

impl FieldOperation {
    fn destination(&self) -> &str {
        match self {
            Self::Set { path, .. } | Self::Rename { path, .. } => path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetFieldsConfiguration {
    mode: SetFieldsMode,
    operations: Vec<FieldOperation>,
}

impl SetFieldsConfiguration {
    pub(super) fn requires_source(&self) -> bool {
        self.mode == SetFieldsMode::Update
            || self.operations.iter().any(|operation| {
                matches!(
                    operation,
                    FieldOperation::Set {
                        value: FieldValue::Input { .. },
                        ..
                    }
                )
            })
    }

    pub(super) fn validate(&self) -> Result<()> {
        bounded_data(self)?;
        validate_destinations(self.operations.iter().map(FieldOperation::destination))?;
        for operation in &self.operations {
            match operation {
                FieldOperation::Set {
                    value: FieldValue::Input { pointer },
                    ..
                } => validate_pointer(pointer)?,
                FieldOperation::Rename { from, path } => {
                    validate_destination(from)?;
                    ensure!(
                        !overlaps(from, path),
                        "Rename requires separate source and destination fields"
                    );
                }
                FieldOperation::Set {
                    value: FieldValue::Literal { .. },
                    ..
                } => {}
            }
        }
        Ok(())
    }

    pub(super) fn evaluate(&self, source: Option<&Value>) -> Result<Map<String, Value>> {
        self.validate()?;
        ensure!(
            source.is_some() || !self.requires_source(),
            "Set fields requires a source value"
        );
        let mut object = match self.mode {
            SetFieldsMode::Build => Map::new(),
            SetFieldsMode::Update => {
                let object = source
                    .and_then(Value::as_object)
                    .context("Set fields can update only an object")?;
                bounded_data(object)?;
                object.clone()
            }
        };
        for operation in &self.operations {
            let value = match operation {
                FieldOperation::Set {
                    value: FieldValue::Literal { value },
                    ..
                } => value.clone(),
                FieldOperation::Set {
                    value: FieldValue::Input { pointer },
                    ..
                } => {
                    let value = source
                        .and_then(|input| input.pointer(pointer))
                        .with_context(|| format!("Source field {pointer:?} is missing"))?;
                    bounded_data(value)?;
                    value.clone()
                }
                FieldOperation::Rename { from, .. } => remove_field(&mut object, from)
                    .with_context(|| format!("Field {from:?} is missing from the object"))?,
            };
            set_field(&mut object, operation.destination(), value)?;
            bounded_data(&object)?;
        }
        Ok(object)
    }
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

impl FilterMapListConfiguration {
    pub(super) fn validate(&self) -> Result<()> {
        bounded_data(self)?;
        if let Some(predicate) = &self.predicate {
            ensure!(
                !matches!(predicate, ConditionPredicate::CandidateChecksPassed)
                    && predicate.valid(),
                "A list predicate needs a field and an existence or scalar equality comparison"
            );
        }
        validate_destinations(self.mappings.iter().map(|mapping| mapping.path.as_str()))?;
        for mapping in &self.mappings {
            validate_pointer(&mapping.from)?;
        }
        Ok(())
    }

    pub(super) fn evaluate(
        &self,
        list: &WorkflowJsonList,
        source_artifact_id: &str,
    ) -> Result<WorkflowJsonList> {
        self.validate()?;
        list.validate()?;
        let mut values = Vec::new();
        for (index, item) in list.values.iter().enumerate() {
            if self
                .predicate
                .as_ref()
                .is_some_and(|predicate| !predicate.test(item))
            {
                continue;
            }
            if self.mappings.is_empty() {
                values.push(item.clone());
                continue;
            }
            let mut object = Map::new();
            for mapping in &self.mappings {
                let value = item.pointer(&mapping.from).with_context(|| {
                    format!("List item {} has no field {:?}", index + 1, mapping.from)
                })?;
                set_field(&mut object, &mapping.path, value.clone())?;
                bounded_data(&object)?;
            }
            values.push(Value::Object(object));
        }
        WorkflowJsonList::new(values, Some(source_artifact_id.into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTextConfiguration {
    template: String,
}

impl BuildTextConfiguration {
    pub(super) fn validate(&self) -> Result<()> {
        bounded_data(self)?;
        parse_template(&self.template).map(|_| ())
    }

    pub(super) fn requires_values(&self) -> bool {
        parse_template(&self.template)
            .is_ok_and(|parts| parts.iter().any(|part| matches!(part, TextPart::Field(_))))
    }

    pub(super) fn evaluate(&self, values: Option<&Map<String, Value>>) -> Result<String> {
        bounded_data(self)?;
        let mut text = String::new();
        for part in parse_template(&self.template)? {
            match part {
                TextPart::Literal(value) => append_text(&mut text, value)?,
                TextPart::Field(name) => {
                    let value = values
                        .and_then(|fields| fields.get(name))
                        .with_context(|| format!("Template field {name:?} is missing"))?;
                    match value {
                        Value::String(value) => append_text(&mut text, value)?,
                        Value::Bool(value) => {
                            append_text(&mut text, if *value { "true" } else { "false" })?;
                        }
                        Value::Number(value) => append_text(&mut text, &value.to_string())?,
                        Value::Null => {}
                        Value::Array(_) | Value::Object(_) => {
                            bail!("Template field {name:?} must be a scalar value");
                        }
                    }
                }
            }
        }
        Ok(text)
    }
}

pub(super) fn data_view(artifact: &Value) -> &Value {
    let pointer = match artifact.get("kind").and_then(Value::as_str) {
        Some("json_object" | "rendered_text") => "/evidence",
        Some("json_list") => "/evidence/values",
        Some("json_item") => "/evidence/value",
        _ => return artifact,
    };
    artifact.pointer(pointer).unwrap_or(artifact)
}

fn bounded_data(value: &impl Serialize) -> Result<()> {
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_DATA_BYTES,
        "The node data exceeds {MAX_DATA_BYTES} UTF-8 bytes"
    );
    Ok(())
}

fn validate_pointer(pointer: &str) -> Result<()> {
    ensure!(
        pointer.len() <= MAX_POINTER_BYTES
            && (pointer.is_empty() || pointer.starts_with('/'))
            && pointer.match_indices('~').all(|(index, _)| {
                matches!(pointer.as_bytes().get(index + 1), Some(b'0' | b'1'))
            }),
        "Enter a valid JSON Pointer for the field"
    );
    Ok(())
}

fn validate_destination(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty(),
        "Select an object field as the destination"
    );
    validate_pointer(path)
}

fn validate_destinations<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut destinations = BTreeSet::new();
    for path in paths {
        validate_destination(path)?;
        ensure!(
            destinations.insert(path),
            "Destination field {path:?} appears more than once"
        );
    }
    for path in &destinations {
        for (index, _) in path.match_indices('/').skip(1) {
            ensure!(
                !destinations.contains(&path[..index]),
                "Destination field {path:?} overlaps another destination"
            );
        }
    }
    Ok(())
}

fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

fn object_path(path: &str) -> Vec<String> {
    path.split('/')
        .skip(1)
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn set_field(object: &mut Map<String, Value>, path: &str, value: Value) -> Result<()> {
    let fields = object_path(path);
    let (field, parents) = fields.split_last().context("Missing destination field")?;
    let mut parent = object;
    for name in parents {
        let entry = parent
            .entry(name.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        parent = entry
            .as_object_mut()
            .with_context(|| format!("Destination {path:?} has a non-object parent"))?;
    }
    parent.insert(field.clone(), value);
    Ok(())
}

fn remove_field(object: &mut Map<String, Value>, path: &str) -> Option<Value> {
    let fields = object_path(path);
    let (field, parents) = fields.split_last()?;
    let mut parent = object;
    for name in parents {
        parent = parent.get_mut(name)?.as_object_mut()?;
    }
    parent.remove(field)
}

enum TextPart<'a> {
    Literal(&'a str),
    Field(&'a str),
}

fn parse_template(mut rest: &str) -> Result<Vec<TextPart<'_>>> {
    let mut parts = Vec::new();
    while let Some((before, opening)) = rest.split_once("{{") {
        if let Some(literal) = before.strip_suffix('\\') {
            parts.extend([TextPart::Literal(literal), TextPart::Literal("{{")]);
            rest = opening;
            continue;
        }
        let (field, after) = opening
            .split_once("}}")
            .context("Close the template field")?;
        let field = field.trim();
        ensure!(valid_field_name(field), "Enter a valid template field name");
        parts.extend([TextPart::Literal(before), TextPart::Field(field)]);
        rest = after;
    }
    parts.push(TextPart::Literal(rest));
    Ok(parts)
}

fn valid_field_name(field: &str) -> bool {
    let mut bytes = field.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn append_text(text: &mut String, part: &str) -> Result<()> {
    ensure!(
        part.len() <= MAX_DATA_BYTES - text.len(),
        "The rendered text exceeds {MAX_DATA_BYTES} UTF-8 bytes"
    );
    text.push_str(part);
    Ok(())
}

#[cfg(test)]
mod tests;
