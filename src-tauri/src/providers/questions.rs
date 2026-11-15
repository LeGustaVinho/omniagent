use std::{cmp::Ordering, collections::BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use rmcp::model::StringFormat;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use super::{
    AdapterUpdate, ProviderEvent, ProviderEventData, ProviderLocation, RequestId, WireMessage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionForm {
    pub message: Option<String>,
    pub fields: Vec<QuestionField>,
    pub actions: Vec<QuestionAction>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionRoute {
    #[default]
    Native,
    StructuredTurn,
}

impl QuestionRoute {
    pub(crate) fn is_native(&self) -> bool {
        *self == Self::Native
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionField {
    pub id: String,
    pub label: String,
    pub header: Option<String>,
    pub description: Option<String>,
    pub secret: bool,
    pub required: bool,
    pub default: Option<Value>,
    pub input: QuestionInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum QuestionInput {
    Text {
        min_length: Option<u64>,
        max_length: Option<u64>,
        format: Option<StringFormat>,
    },
    Boolean,
    Number {
        integer: bool,
        minimum: Option<serde_json::Number>,
        maximum: Option<serde_json::Number>,
    },
    Choice {
        options: Vec<QuestionOption>,
        multiple: bool,
        min_selections: u64,
        max_selections: u64,
        allow_custom: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionReply {
    pub action: QuestionAction,
    #[serde(default)]
    pub values: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOrigin {
    pub run_id: String,
    pub request_id: RequestId,
}

pub(crate) fn encoded_reply(
    id: &RequestId,
    reply: QuestionReply,
    frame: WireMessage,
) -> AdapterUpdate {
    let event = ProviderEvent {
        location: ProviderLocation::default(),
        data: ProviderEventData::QuestionReplyPrepared {
            request_id: id.clone(),
            reply,
            frame: frame.as_bytes().to_vec(),
        },
    };
    AdapterUpdate {
        outbound: vec![frame],
        events: vec![event],
    }
}

impl QuestionForm {
    pub(crate) fn codex(raw: &Value) -> Result<Self> {
        Self::tool(raw, false)
    }

    pub(crate) fn claude(raw: &Value) -> Result<Self> {
        Self::tool(raw, true)
    }

    fn tool(raw: &Value, claude: bool) -> Result<Self> {
        let fields = raw
            .as_array()
            .context("missing questions array")?
            .iter()
            .map(|question| {
                let label = required_string(question, "question")?;
                let id = if claude {
                    label.clone()
                } else {
                    required_string(question, "id")?
                };
                let options = if question["options"].is_null() {
                    Vec::new()
                } else {
                    question["options"]
                        .as_array()
                        .context("question options must be an array")?
                        .iter()
                        .map(|option| {
                            let label = required_string(option, "label")?;
                            Ok(QuestionOption {
                                id: label.clone(),
                                label,
                                description: optional_string(option, "description")?,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                let multiple = claude && optional_bool(question, "multiSelect")?.unwrap_or(false);
                let allow_custom = claude || optional_bool(question, "isOther")?.unwrap_or(false);
                let secret = !claude && optional_bool(question, "isSecret")?.unwrap_or(false);
                let input = if options.is_empty() {
                    QuestionInput::Text {
                        min_length: Some(1),
                        max_length: None,
                        format: None,
                    }
                } else {
                    let max_selections = if multiple { options.len() as u64 } else { 1 };
                    QuestionInput::Choice {
                        options,
                        multiple,
                        min_selections: 1,
                        max_selections,
                        allow_custom,
                    }
                };
                Ok(QuestionField {
                    id,
                    label,
                    header: optional_string(question, "header")?,
                    description: None,
                    secret,
                    required: true,
                    default: question.get("default").cloned(),
                    input,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let actions = if claude {
            vec![
                QuestionAction::Accept,
                QuestionAction::Decline,
                QuestionAction::Cancel,
            ]
        } else {
            vec![QuestionAction::Accept, QuestionAction::Cancel]
        };
        ensure!(
            !fields.is_empty(),
            "provider requested an empty question list"
        );
        Self::new(None, fields, actions)
    }

    pub(crate) fn acp(params: &Value) -> Result<Self> {
        ensure!(
            params["mode"] == "form",
            "ACP only supports forms in a conversation"
        );
        let message = required_string(params, "message")?;
        Self::schema(message, &params["requestedSchema"])
    }

    pub(crate) fn schema(message: String, schema: &Value) -> Result<Self> {
        ensure!(!message.trim().is_empty(), "question message is empty");
        check_keys(
            schema,
            &[
                "type",
                "properties",
                "required",
                "title",
                "description",
                "additionalProperties",
            ],
        )?;
        ensure!(schema["type"] == "object", "ACP requires an object form");
        ensure!(
            schema
                .get("additionalProperties")
                .is_none_or(|setting| setting.is_boolean()),
            "ACP additional property schemas are unsupported"
        );
        let props = schema["properties"]
            .as_object()
            .context("missing ACP form fields")?;
        let required = if let Some(required) = schema.get("required") {
            let values = required
                .as_array()
                .context("required ACP fields must be an array")?;
            let names = values
                .iter()
                .map(|name| {
                    let id = name.as_str().context("invalid required ACP field")?;
                    ensure!(props.contains_key(id), "ACP form requires an unknown field");
                    Ok(id)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            ensure!(
                names.len() == values.len(),
                "ACP form repeats a required field"
            );
            names
        } else {
            BTreeSet::new()
        };
        let fields = props
            .iter()
            .map(|(id, property)| {
                Ok(QuestionField {
                    id: id.clone(),
                    label: optional_string(property, "title")?.unwrap_or_else(|| id.clone()),
                    header: None,
                    description: optional_string(property, "description")?,
                    secret: false,
                    required: required.contains(id.as_str()),
                    default: property.get("default").cloned(),
                    input: acp_input(property)?,
                })
            })
            .collect::<Result<_>>()?;
        Self::new(
            Some(message),
            fields,
            vec![
                QuestionAction::Accept,
                QuestionAction::Decline,
                QuestionAction::Cancel,
            ],
        )
    }

    fn new(
        message: Option<String>,
        fields: Vec<QuestionField>,
        actions: Vec<QuestionAction>,
    ) -> Result<Self> {
        let mut ids = BTreeSet::new();
        for field in &fields {
            ensure!(
                !field.id.trim().is_empty() && ids.insert(&field.id),
                "question field ID is empty or repeated"
            );
            if let QuestionInput::Choice { options, .. } = &field.input {
                let mut values = BTreeSet::new();
                ensure!(
                    options.iter().all(|option| values.insert(&option.id)),
                    "question has duplicate option values"
                );
            }
            if let Some(default) = &field.default {
                field
                    .input
                    .validate(default)
                    .with_context(|| format!("invalid default for {}", field.id))?;
            }
        }
        Ok(Self {
            message,
            fields,
            actions,
        })
    }

    pub fn validate(&self, reply: &QuestionReply) -> Result<()> {
        ensure!(
            self.actions.contains(&reply.action),
            "question action is unavailable"
        );
        if reply.action != QuestionAction::Accept {
            ensure!(
                reply.values.is_empty(),
                "only an accepted response can contain field values"
            );
            return Ok(());
        }
        for (id, value) in &reply.values {
            let field = self
                .fields
                .iter()
                .find(|field| field.id == *id)
                .with_context(|| format!("unknown question field {id}"))?;
            field
                .input
                .validate(value)
                .with_context(|| format!("invalid answer for {id}"))?;
        }
        for field in &self.fields {
            ensure!(
                !field.required || reply.values.contains_key(&field.id),
                "missing required answer for {}",
                field.id
            );
        }
        Ok(())
    }
}

impl QuestionInput {
    fn validate(&self, value: &Value) -> Result<()> {
        match self {
            Self::Text {
                min_length,
                max_length,
                format,
            } => {
                let text = value.as_str().context("expected text")?;
                let len = text.chars().count() as u64;
                ensure!(
                    min_length.is_none_or(|min| len >= min)
                        && max_length.is_none_or(|max| len <= max),
                    "text length is outside the supported range"
                );
                if let Some(format) = format {
                    ensure!(
                        valid_format(text, *format),
                        "text does not match the requested format"
                    );
                }
            }
            Self::Boolean => ensure!(value.is_boolean(), "expected a Boolean"),
            Self::Number {
                integer,
                minimum,
                maximum,
            } => {
                let number = value.as_number().context("expected a number")?;
                ensure!(
                    !*integer || number.as_f64().is_some_and(|value| value.fract() == 0.0),
                    "expected an integer"
                );
                ensure!(
                    minimum.as_ref().is_none_or(|bound| compare_numbers(number, bound) != Ordering::Less)
                        && maximum.as_ref().is_none_or(|bound| compare_numbers(number, bound) != Ordering::Greater),
                    "number is outside the supported range"
                );
            }
            Self::Choice {
                options,
                multiple,
                min_selections,
                max_selections,
                allow_custom,
            } => {
                if *multiple
                    && *allow_custom
                    && let Some(custom) = value.as_str()
                {
                    ensure!(!custom.trim().is_empty(), "custom answer is empty");
                    return Ok(());
                }
                let values = if *multiple {
                    value
                        .as_array()
                        .context("expected a list of selected options")?
                        .as_slice()
                } else {
                    std::slice::from_ref(value)
                };
                let count = values.len() as u64;
                ensure!(
                    count >= *min_selections && count <= *max_selections,
                    "number of selections is outside the supported range"
                );
                let mut selected = BTreeSet::new();
                for value in values {
                    let id = value.as_str().context("expected a choice ID")?;
                    ensure!(selected.insert(id), "selected choice was repeated");
                    ensure!(
                        options.iter().any(|option| option.id == id)
                            || (*allow_custom && !*multiple && !id.trim().is_empty()),
                        "answer contains an unavailable choice"
                    );
                }
            }
        }
        Ok(())
    }
}

fn acp_input(property: &Value) -> Result<QuestionInput> {
    let mut keywords = vec!["type", "title", "description", "default"];
    let input = match property["type"].as_str() {
        Some("string") if property.get("enum").is_some() || property.get("oneOf").is_some() => {
            keywords.extend(["enum", "oneOf", "enumNames"]);
            QuestionInput::Choice {
                options: acp_options(property, "oneOf")?,
                multiple: false,
                min_selections: 1,
                max_selections: 1,
                allow_custom: false,
            }
        }
        Some("string") => {
            keywords.extend(["minLength", "maxLength", "format"]);
            let (min_length, max_length) = count_bounds(property, "minLength", "maxLength")?;
            QuestionInput::Text {
                min_length,
                max_length,
                format: property
                    .get("format")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?,
            }
        }
        Some("boolean") => QuestionInput::Boolean,
        Some(kind @ ("number" | "integer")) => {
            keywords.extend(["minimum", "maximum"]);
            let number = |key: &str| {
                property
                    .get(key)
                    .map(|value| value.as_number().cloned().context("invalid numeric bound"))
                    .transpose()
            };
            let minimum = number("minimum")?;
            let maximum = number("maximum")?;
            if let (Some(min), Some(max)) = (&minimum, &maximum) {
                ensure!(
                    compare_numbers(min, max) != Ordering::Greater,
                    "minimum exceeds maximum"
                );
            }
            QuestionInput::Number {
                integer: kind == "integer",
                minimum,
                maximum,
            }
        }
        Some("array") => {
            keywords.extend(["items", "minItems", "maxItems", "uniqueItems"]);
            let items = &property["items"];
            check_keys(items, &["type", "enum", "anyOf"])?;
            ensure!(
                items.get("type").is_none_or(|kind| kind == "string"),
                "ACP multi-select requires string option values"
            );
            let options = acp_options(items, "anyOf")?;
            let (min, max) = count_bounds(property, "minItems", "maxItems")?;
            optional_bool(property, "uniqueItems")?;
            let max_selections = max
                .unwrap_or(options.len() as u64)
                .min(options.len() as u64);
            let min_selections = min.unwrap_or(0);
            ensure!(
                min_selections <= max_selections,
                "form requires more choices than it offers"
            );
            QuestionInput::Choice {
                options,
                multiple: true,
                min_selections,
                max_selections,
                allow_custom: false,
            }
        }
        _ => bail!("unsupported ACP field type"),
    };
    check_keys(property, &keywords)?;
    Ok(input)
}

fn acp_options(property: &Value, titled: &str) -> Result<Vec<QuestionOption>> {
    let enumerated = property.get("enum");
    let choices = property.get(titled);
    ensure!(
        enumerated.is_some() != choices.is_some(),
        "expected one ACP option list"
    );
    let options = if let Some(choices) = choices {
        ensure!(
            property.get("enumNames").is_none(),
            "unexpected enumNames with titled options"
        );
        choices
            .as_array()
            .context("invalid titled ACP options")?
            .iter()
            .map(|option| {
                check_keys(option, &["const", "title", "description"])?;
                Ok(QuestionOption {
                    id: optional_string(option, "const")?.context("missing ACP choice value")?,
                    label: required_string(option, "title")?,
                    description: optional_string(option, "description")?,
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        let values = enumerated
            .and_then(Value::as_array)
            .context("invalid ACP enum")?;
        let names = property
            .get("enumNames")
            .map(|names| names.as_array().context("invalid enumNames"))
            .transpose()?;
        ensure!(
            names.is_none_or(|names| names.len() == values.len()),
            "enumNames must match the enum"
        );
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let id = value.as_str().context("invalid ACP enum value")?.to_owned();
                let label = match names {
                    Some(names) => names[index]
                        .as_str()
                        .context("invalid enum label")?
                        .to_owned(),
                    None => id.clone(),
                };
                Ok(QuestionOption {
                    id,
                    label,
                    description: None,
                })
            })
            .collect::<Result<Vec<_>>>()?
    };
    ensure!(!options.is_empty(), "ACP option list is empty");
    Ok(options)
}

fn count_bounds(schema: &Value, min: &str, max: &str) -> Result<(Option<u64>, Option<u64>)> {
    let get = |key: &str| {
        schema
            .get(key)
            .map(|value| value.as_u64().context("invalid count bound"))
            .transpose()
    };
    let (lower, upper) = (get(min)?, get(max)?);
    ensure!(
        lower.zip(upper).is_none_or(|(min, max)| min <= max),
        "minimum exceeds maximum"
    );
    Ok((lower, upper))
}

fn valid_format(text: &str, format: StringFormat) -> bool {
    match format {
        StringFormat::Date => time::Date::parse(
            text,
            time::macros::format_description!("[year]-[month]-[day]"),
        )
        .is_ok(),
        StringFormat::DateTime => {
            time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
                .is_ok()
        }
        StringFormat::Uri => {
            tauri::Url::parse(text).is_ok() && !text.chars().any(char::is_whitespace)
        }
        StringFormat::Email => text.rsplit_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && !local.contains('@')
                && !domain.is_empty()
                && !text.chars().any(char::is_whitespace)
                && tauri::Url::parse(&format!("https://{domain}")).is_ok_and(|url| {
                    url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.port().is_none()
                        && url.path() == "/"
                        && url.query().is_none()
                        && url.fragment().is_none()
                })
        }),
        _ => false,
    }
}

fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    let integer = |number: &Number| {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    };
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .expect("JSON numbers are finite"),
    }
}

fn check_keys(value: &Value, allowed: &[&str]) -> Result<()> {
    for key in value
        .as_object()
        .context("expected a schema object")?
        .keys()
    {
        ensure!(
            allowed.contains(&key.as_str()),
            "unsupported schema keyword {key}"
        );
    }
    Ok(())
}

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .with_context(|| format!("{key} must be Boolean"))
        })
        .transpose()
}

fn optional_string(value: &Value, key: &str) -> Result<Option<String>> {
    value
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("{key} must be text"))
        })
        .transpose()
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key)?
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing {key}"))
}
