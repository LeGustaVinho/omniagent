use std::sync::LazyLock;

use regex::Regex;

const PREVIEW_CHARS: usize = 768;

static CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?ix) (?: \b (?: authorization | api[-_\x20]?key | password | secret | [\w.-]*token ) "#,
        r#"[\x22\x27]?\s*[:=]\s*(?:bearer\s+)?(?:\x22[^\x22]*\x22|\x27[^\x27]*\x27|[^\s,;}]+) "#,
        r#"| \bbearer\s+[a-z0-9._~+/=-]+ "#,
        r#"| \b(?:sk-(?:proj-|ant-)?|gh[pousr]_|github_pat_|xox[baprs]-)[a-z0-9_-]{8,} "#,
        r#"| \beyJ[a-z0-9_-]+\.[a-z0-9_-]+\.[a-z0-9_-]+ )"#,
    ))
    .expect("credential pattern is valid")
});

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
}
