use std::sync::LazyLock;

use anyhow::{Result, ensure};
use regex::Regex;
use serde_json::Value;
use tauri::Url;

use crate::diagnostics;

const BLOCKED: &str = "A workflow export contains a credential. Replace literals with local bindings or environment references before exporting.";

static WEB_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:https?|wss?)://[^\s<>"'`]+"#).expect("valid URL pattern")
});

pub(super) fn check(document: &Value) -> Result<()> {
    ensure!(!contains_credentials(document), BLOCKED);
    Ok(())
}

fn contains_credentials(value: &Value) -> bool {
    match value {
        Value::String(value) => contains_credential_text(value),
        Value::Array(items) => items.iter().any(contains_credentials),
        Value::Object(fields) => fields.iter().any(|(name, value)| {
            contains_credential_text(name)
                || (sensitive_name(name) && has_credential_value(name, value))
                || contains_credentials(value)
        }),
        _ => false,
    }
}

fn has_credential_value(name: &str, value: &Value) -> bool {
    match value {
        Value::String(value) => literal(name, value),
        Value::Number(_) => true,
        Value::Array(items) => items.iter().any(|item| has_credential_value(name, item)),
        // A schema may declare a field like apiKey without putting credentials
        // in the exchange. Its examples and defaults may carry literal values.
        Value::Object(fields) => ["default", "examples", "example", "const", "enum"]
            .into_iter()
            .filter_map(|field| fields.get(field))
            .any(|value| has_credential_value(name, value)),
        _ => false,
    }
}

fn contains_credential_text(text: &str) -> bool {
    let mut start = 0;
    for found in WEB_URL.find_iter(text) {
        if contains_credential_fragment(&text[start..found.start()])
            || contains_url_credential(found.as_str())
        {
            return true;
        }
        start = found.end();
    }
    contains_credential_fragment(&text[start..])
}

fn contains_credential_fragment(text: &str) -> bool {
    diagnostics::detected_credentials(text).any(|found| {
        if let Some((scheme, token)) = found.split_once(char::is_whitespace)
            && scheme.eq_ignore_ascii_case("bearer")
        {
            // An unlabeled reference to "Bearer authentication" in prose
            // has no credential value. Header values are checked separately.
            return token
                .trim_end_matches('.')
                .bytes()
                .any(|byte| !byte.is_ascii_alphabetic());
        }
        found
            .split_once(['=', ':'])
            .is_none_or(|(name, value)| literal(name, value))
    })
}

fn contains_url_credential(url: &str) -> bool {
    let Ok(url) = Url::parse(url.trim_end_matches([')', ']', '.', ',', ';'])) else {
        return contains_credential_fragment(url);
    };
    url.password()
        .is_some_and(|value| literal("password", value))
        || contains_credential_fragment(url.username())
        || url.query_pairs().any(|(key, value)| {
            (sensitive_name(&key) && literal(&key, &value)) || contains_credential_fragment(&value)
        })
        || contains_credential_fragment(url.path())
        || url.fragment().is_some_and(contains_credential_fragment)
}

fn literal(name: &str, value: &str) -> bool {
    let value = value.trim().trim_matches(['\'', '"', '`']);
    let name = name.trim_end_matches(['\'', '"']).to_ascii_lowercase();
    let value = if name.ends_with("authorization") {
        value
            .split_once(char::is_whitespace)
            .map_or(value, |(_, token)| token.trim())
    } else {
        value
    };
    let reference = value
        .trim_end_matches(['.', ')'])
        .trim_matches(['\'', '"', '`']);
    !value.is_empty() && !is_reference(reference)
}

fn sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "authorization",
        "proxy-authorization",
        "proxy_authorization",
        "apikey",
        "x-api-key",
        "api_key",
        "api-key",
        "password",
        "passwd",
        "credential",
        "secret",
        "clientsecret",
        "client-secret",
        "privatekey",
        "accesstoken",
        "refreshtoken",
        "token",
    ]
    .contains(&lower.as_str())
        || [
            "_api_key",
            "_secret",
            "_password",
            "_token",
            "_private_key",
            "-token",
            "-api-key",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

fn is_reference(value: &str) -> bool {
    if value == "[redacted]" {
        return true;
    }
    [
        ("${env:", "}"),
        ("${", "}"),
        ("$", ""),
        ("%", "%"),
        ("{{", "}}"),
        ("<", ">"),
    ]
    .into_iter()
    .filter_map(|(open, close)| value.strip_prefix(open)?.strip_suffix(close))
    .any(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn distinguishes_portable_schemas_and_references_from_credential_literals() -> Result<()> {
        check(&json!({
            "properties": {"apiKey": {"type": "string", "description": "Use a local binding"}},
            "required": ["apiKey"],
            "instructions": "Set OPENAI_API_KEY=`${env:OPENAI_API_KEY}`.",
            "completionContract": "The documented endpoint uses Bearer authentication.",
            "example": "https://example.invalid/lookup?apiKey=${API_KEY}&key=article-slug",
            "headers": {"Authorization": "Bearer '${access_token}'", "X-API-Key": "[redacted]"},
        }))?;
        for data in [
            json!({"instructions": "Set OPENAI_API_KEY=fixture-only-secret"}),
            json!({"instructions": "Authorization: Basic Zml4dHVyZTpzZWNyZXQ="}),
            json!({"instructions": "Bearer Y2FuYXJ5OmZpeHR1cmU="}),
            json!({"instructions": "https://user:fixture-only-secret@example.invalid"}),
            json!({"instructions": "https://ghp_fixture_canary_only@example.invalid"}),
            json!({"instructions": "https://example.invalid/lookup?api%5Fkey=fixture-only-secret"}),
            json!({"properties": {"apiKey": {"type": "string", "default": "fixture-only-secret"}}}),
            json!({"headers": {"Authorization": "Basic Zml4dHVyZTpzZWNyZXQ="}}),
            json!({"environment": {"OPENAI_API_KEY": "fixture-only-secret"}}),
        ] {
            assert_eq!(check(&data).unwrap_err().to_string(), BLOCKED);
        }
        Ok(())
    }
}
