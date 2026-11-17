use std::sync::LazyLock;

use regex::Regex;

const PREVIEW_CHARS: usize = 768;

static CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?ix) (?: \b(?:proxy[-_\x20]?)?authorization[\x22\x27]?\s*[:=]\s* "#,
        r#"(?: \x22(?:[^\x22\\\r\n]|\\[^\r\n])*\x22 | \x27(?:[^\x27\\\r\n]|\\[^\r\n])*\x27 "#,
        r#"| digest[\t\x20]+[^\r\n]* | (?:[a-z][a-z0-9+._-]*[\t\x20]+)?[^\s,;]+ ) "#,
        r#"| \b (?: (?:[\w.-]+[-_])?(?:api[-_\x20]?key|password|secret) | [\w.-]*token ) "#,
        r#"[\x22\x27]?\s*[:=]\s*(?:bearer\s+)?(?:\x22[^\x22]*\x22|\x27[^\x27]*\x27|[^\s,;]+) "#,
        r#"| \bbearer\s+[a-z0-9._~+/=-]+ "#,
        r#"| \b(?:sk-(?:proj-|ant-)?|gh[pousr]_|github_pat_|xox[baprs]-)[a-z0-9_-]{8,} "#,
        r#"| \beyJ[a-z0-9_-]+\.[a-z0-9_-]+\.[a-z0-9_-]+ )"#,
    ))
    .expect("credential pattern is valid")
});

pub(crate) fn detected_credentials(text: &str) -> impl Iterator<Item = &str> {
    CREDENTIALS.find_iter(text).map(|found| found.as_str())
}

pub fn preview(text: &str) -> String {
    let redacted = CREDENTIALS.replace_all(text, "[redacted]");
    let text = redacted.trim_end_matches(['\r', '\n']);
    let Some((boundary, _)) = text.char_indices().nth(PREVIEW_CHARS) else {
        return text.to_owned();
    };
    format!("{}…", &text[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_redacts_before_truncating_unicode() {
        let text = format!(
            r#"{{"api_key":"private-value"}} Authorization: Bearer test-credential {}"#,
            "é".repeat(800)
        );
        let preview = preview(&text);
        assert!(!preview.contains("private-value"));
        assert!(!preview.contains("test-credential"));
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
    }

    #[test]
    fn preview_redacts_authorization_schemes_and_quoted_headers() {
        for scheme in ["Basic", "Bearer", "Negotiate", "NTLM", "Custom"] {
            let text = format!("Request failed Authorization: {scheme} Y2FuYXJ5OmZpeHR1cmU= retry");
            assert_eq!(preview(&text), "Request failed [redacted] retry");
        }
        assert_eq!(
            preview("Proxy-Authorization: Basic Y2FuYXJ5OmZpeHR1cmU="),
            "[redacted]"
        );
        assert_eq!(
            preview(
                "Authorization: Digest username=\"private-user\", response=\"fixture-only\"\nRetry request"
            ),
            "[redacted]\nRetry request"
        );
        let quoted = preview(
            r#"{"Authorization":"Digest username=\"private-user\", response=\"fixture-only\"","status":401}"#,
        );
        assert!(!quoted.contains("private-user") && !quoted.contains("fixture-only"));
        assert!(quoted.contains(r#""status":401"#));
    }
}
