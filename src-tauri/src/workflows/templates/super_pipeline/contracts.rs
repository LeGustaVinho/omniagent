use serde_json::{Map, Value, json};

pub(super) const OPTIONAL_STRATEGY_CHECKS: &[&str] =
    &["Pipeline e2e", "Pipeline mutation", "Pipeline performance"];

fn object(fields: &[(&str, Value)]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": fields.iter().cloned().map(|(name, schema)| (name.to_owned(), schema)).collect::<Map<_, _>>(),
        "required": fields.iter().map(|(name, _)| *name).collect::<Vec<_>>()
    })
}

fn text(max_length: usize) -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": max_length })
}

fn list(items: Value, min: usize, max: usize) -> Value {
    json!({ "type": "array", "items": items, "minItems": min, "maxItems": max })
}

pub(super) fn documentation(key: &str) -> Value {
    object(&[(key, text(9000))])
}

pub(super) fn test_strategy() -> Value {
    let mut required = list(
        json!({ "enum": OPTIONAL_STRATEGY_CHECKS }),
        0,
        OPTIONAL_STRATEGY_CHECKS.len(),
    );
    required["uniqueItems"] = json!(true);
    object(&[("testStrategy", text(9000)), ("requiredChecks", required)])
}

pub(super) fn documentation_review() -> Value {
    let mut schema = object(&[
        ("documentationAndCleanup", text(9000)),
        ("needsChanges", json!({ "type": "boolean" })),
        (
            "findings",
            list(
                object(&[
                    ("path", text(300)),
                    ("change", text(400)),
                    ("verification", text(400)),
                    ("references", list(text(100), 0, 20)),
                ]),
                0,
                10,
            ),
        ),
    ]);
    schema["if"] = json!({ "properties": { "needsChanges": { "const": true } } });
    schema["then"] = json!({ "properties": { "findings": { "minItems": 1 } } });
    schema["else"] = json!({ "properties": { "findings": { "maxItems": 0 } } });
    schema
}

pub(super) fn public_query() -> Value {
    object(&[("query", text(180))])
}

pub(super) fn risk() -> Value {
    object(&[("highRisk", json!({ "type": "boolean" }))])
}

pub(super) fn red_tests() -> Value {
    object(&[
        ("paths", list(text(500), 1, 32)),
        (
            "tests",
            list(object(&[("id", text(250)), ("reason", text(500))]), 1, 24),
        ),
    ])
}

pub(super) fn requirements() -> Value {
    let mut schema = object(&[
        ("requirements", text(6500)),
        ("needsClarification", json!({ "type": "boolean" })),
        ("questions", list(text(250), 0, 10)),
    ]);
    schema["if"] = json!({ "properties": { "needsClarification": { "const": true } } });
    schema["then"] = json!({ "properties": { "questions": { "minItems": 1 } } });
    schema
}

pub(super) fn master_plan() -> Value {
    documentation("masterPlan")
}

pub(super) fn sdd() -> Value {
    object(&[
        ("overview", text(6500)),
        (
            "requirements",
            list(
                object(&[
                    ("id", text(100)),
                    (
                        "kind",
                        json!({ "enum": ["functional", "non_functional", "constraint"] }),
                    ),
                    ("description", text(1500)),
                    ("acceptanceCriteria", list(text(500), 1, 20)),
                ]),
                1,
                128,
            ),
        ),
    ])
}

pub(super) fn bdd() -> Value {
    let categories = ["positive", "negative", "boundary"];
    let mut scenarios = list(
        object(&[
            ("id", text(100)),
            ("type", json!({ "enum": categories })),
            ("requirements", list(text(100), 1, 32)),
            ("given", text(500)),
            ("when", text(500)),
            ("then", text(500)),
        ]),
        categories.len(),
        256,
    );
    scenarios["allOf"] = Value::Array(
        categories
            .into_iter()
            .map(|kind| json!({ "contains": { "properties": { "type": { "const": kind } } } }))
            .collect(),
    );
    object(&[("scenarios", scenarios)])
}

pub(super) fn backlog() -> Value {
    let mut key = text(100);
    key["pattern"] = json!("^[A-Za-z0-9_.-]+$");
    object(&[
        ("epicTitle", text(200)),
        (
            "tasks",
            list(
                object(&[
                    ("key", key.clone()),
                    ("title", text(200)),
                    ("description", text(2500)),
                    (
                        "priority",
                        json!({ "type": "integer", "minimum": 0, "maximum": 4 }),
                    ),
                    ("dependsOn", list(key, 0, 256)),
                    ("acceptanceCriteria", list(text(500), 1, 20)),
                    ("sddRequirements", list(text(100), 1, 64)),
                    ("bddScenarios", list(text(100), 1, 64)),
                ]),
                1,
                256,
            ),
        ),
    ])
}

pub(super) fn review() -> Value {
    let mut schema = object(&[
        ("approved", json!({ "type": "boolean" })),
        ("summary", text(400)),
        (
            "findings",
            list(
                object(&[
                    ("subject", text(120)),
                    ("problem", text(350)),
                    ("revision", text(350)),
                ]),
                0,
                8,
            ),
        ),
    ]);
    schema["if"] = json!({ "properties": { "approved": { "const": true } } });
    schema["then"] = json!({ "properties": { "findings": { "maxItems": 0 } } });
    schema["else"] = json!({ "properties": { "findings": { "minItems": 1 } } });
    schema
}

pub(super) fn planning_review(subjects: &[&str]) -> Value {
    let mut schema = review();
    schema["properties"]["findings"]["items"]["properties"]["subject"] =
        json!({ "enum": subjects });
    schema
}
