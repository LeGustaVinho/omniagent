use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub(super) const FORMAT: &str = "End the review with exactly one fenced `omniagent-review` JSON block. Use {\"verdict\":\"approved\",\"criteria\":[{\"index\":1,\"status\":\"met\"}],\"findings\":[]}. Use each 1-based index of the pinned unit's completionCriteria exactly once. Allowed statuses are met, unmet and unverified. Each finding needs severity (critical, high, medium, low or info), summary and evidence. Set verdict to changes_requested for unmet or unverified criteria or for critical or high findings. You may also request changes for medium or low findings. The evidence must cite the candidate tree or the checks you actually observed.";

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Approved,
    ChangesRequested,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Met,
    Unmet,
    Unverified,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    severity: Severity,
    summary: String,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Criterion {
    index: usize,
    status: Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewReport {
    verdict: Verdict,
    criteria: Vec<Criterion>,
    findings: Vec<Finding>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Provenance<'a> {
    pub job_id: &'a str,
    pub run_id: &'a str,
    pub dispatch_id: &'a str,
    pub response_revision_id: &'a str,
    pub candidate_id: &'a str,
    pub tree: &'a str,
}

pub(super) fn normalize(
    response: &str,
    expected: &[String],
    provenance: Provenance<'_>,
) -> Result<Map<String, Value>> {
    let marker = "```omniagent-review";
    let mut lines = response.lines();
    let response_is_json = response.trim().starts_with('{');
    let body = if response_is_json {
        response.trim().to_owned()
    } else {
        ensure!(
            lines.by_ref().any(|line| line.trim() == marker),
            "The review has no structured report"
        );
        let mut body = Vec::new();
        let mut fenced = false;
        for line in lines.by_ref() {
            if line.trim() == "```" {
                fenced = true;
                break;
            }
            body.push(line);
        }
        ensure!(
            fenced && lines.all(|line| line.trim().is_empty()),
            "The review must end in one complete structured report"
        );
        body.join("\n")
    };
    let mut saved: ReviewReport =
        serde_json::from_str(&body).context("The review report has invalid fields")?;
    saved.criteria.sort_by_key(|criterion| criterion.index);
    ensure!(
        saved.criteria.len() == expected.len()
            && saved
                .criteria
                .iter()
                .enumerate()
                .all(|(index, criterion)| criterion.index == index + 1),
        "The review must evaluate every pinned criterion exactly once"
    );
    ensure!(
        saved.findings.iter().all(
            |finding| !finding.summary.trim().is_empty() && !finding.evidence.trim().is_empty()
        ),
        "A review finding needs both a summary and concrete evidence"
    );
    let incomplete = saved
        .criteria
        .iter()
        .any(|criterion| criterion.status != Status::Met);
    let serious = saved
        .findings
        .iter()
        .any(|finding| matches!(finding.severity, Severity::Critical | Severity::High));
    ensure!(
        saved.verdict != Verdict::Approved || !(incomplete || serious),
        "The verdict must request changes for incomplete criteria or serious findings"
    );
    ensure!(
        saved.verdict != Verdict::ChangesRequested
            || incomplete
            || saved
                .findings
                .iter()
                .any(|finding| !matches!(finding.severity, Severity::Info)),
        "A request for changes needs an incomplete criterion or actionable finding"
    );
    let criteria: Vec<_> = saved
        .criteria
        .into_iter()
        .zip(expected)
        .map(|(criterion, text)| json!({"criterion": text, "status": criterion.status}))
        .collect();
    Ok(Map::from_iter([
        ("schemaVersion".into(), json!(1)),
        ("verdict".into(), json!(saved.verdict)),
        ("criteria".into(), json!(criteria)),
        ("findings".into(), json!(saved.findings)),
        ("provenance".into(), json!(provenance)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance() -> Provenance<'static> {
        Provenance {
            job_id: "job",
            run_id: "run",
            dispatch_id: "dispatch",
            response_revision_id: "response",
            candidate_id: "candidate",
            tree: "tree",
        }
    }

    #[test]
    fn retains_a_negative_review_with_exact_criterion_text_and_provenance() -> Result<()> {
        let response = r#"Reviewed the candidate.

```omniagent-review
{"verdict":"changes_requested","criteria":[{"index":2,"status":"unmet"},{"index":1,"status":"met"}],"findings":[{"severity":"high","summary":"Missing guard","evidence":"guard.rs:11"}]}
```"#;
        let report = normalize(
            response,
            &["Build succeeds".into(), "Guards malformed input".into()],
            provenance(),
        )?;
        assert_eq!(report["verdict"], "changes_requested");
        assert_eq!(report["criteria"][1]["criterion"], "Guards malformed input");
        assert_eq!(report["findings"][0]["evidence"], "guard.rs:11");
        assert_eq!(report["provenance"]["candidateId"], "candidate");
        Ok(())
    }

    #[test]
    fn rejects_omitted_criteria_and_false_approvals() {
        let expected = ["Checks pass".into()];
        for response in [
            r#"{"verdict":"approved","criteria":[],"findings":[]}"#,
            r#"{"verdict":"approved","criteria":[{"index":1,"status":"unverified"}],"findings":[]}"#,
            r#"{"verdict":"approved","criteria":[{"index":1,"status":"met"}],"findings":[{"severity":"critical","summary":"Crash","evidence":"run output"}]}"#,
        ] {
            assert!(normalize(response, &expected, provenance()).is_err());
        }
    }
}
