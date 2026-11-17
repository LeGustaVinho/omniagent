use std::collections::HashSet;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::{GitCommit, GithubCli, GithubTransport, PullRequest, RepositorySlug, validate_sha};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequiredCheck {
    pub context: String,
    pub app_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiState {
    Pending,
    Passed,
    Failed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckEvidence {
    pub context: String,
    pub app_id: Option<u64>,
    pub state: CiState,
    pub source: String,
    pub id: u64,
}

impl CheckEvidence {
    pub(crate) fn matches_context(&self, context: &str) -> bool {
        if self.source == "commit_status" {
            self.context.to_lowercase() == context.to_lowercase()
        } else {
            self.context == context
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiSnapshot {
    pub pull_number: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub tested_sha: String,
    pub tested_tree: Option<String>,
    pub state: CiState,
    pub checks: Vec<CheckEvidence>,
    pub required_checks: Vec<RequiredCheck>,
    pub reason: Option<String>,
}

impl CiSnapshot {
    fn new(pull: &PullRequest, required: &[RequiredCheck]) -> Self {
        Self {
            pull_number: pull.number,
            head_sha: pull.head_sha.clone(),
            base_sha: pull.base_sha.clone(),
            tested_sha: String::new(),
            tested_tree: None,
            state: CiState::Pending,
            checks: Vec::new(),
            required_checks: required.to_vec(),
            reason: None,
        }
    }

    fn set(&mut self, state: CiState, reason: &'static str) {
        self.state = state;
        self.reason = Some(reason.into());
    }

    pub(super) fn verify_for(&self, pull: &PullRequest) -> Result<&str> {
        ensure!(
            self.state == CiState::Passed
                && self.pull_number == pull.number
                && self.head_sha == pull.head_sha
                && self.base_sha == pull.base_sha,
            "A passing CI snapshot bound to this pull request, head and base is required"
        );
        validate_required_checks(&self.required_checks)?;
        ensure!(
            evaluate_checks(&self.required_checks, &self.checks) == CiState::Passed,
            "The recorded CI evidence does not satisfy all required GitHub checks"
        );
        validate_sha(&self.tested_sha)?;
        let tree = self.tested_tree.as_deref().unwrap_or_default();
        validate_sha(tree)?;
        Ok(tree)
    }
}

pub(super) fn validate_required_checks(required: &[RequiredCheck]) -> Result<()> {
    let mut unique = HashSet::new();
    ensure!(
        !required.is_empty()
            && required.len() <= 100
            && required.iter().all(|check| !check.context.trim().is_empty()
                && check.context == check.context.trim()
                && check.context.len() <= 255
                && !check.context.chars().any(char::is_control)
                && check.app_id != Some(0)
                && unique.insert((&check.context, check.app_id))),
        "A nonempty, unambiguous list of required GitHub checks is needed"
    );
    Ok(())
}

#[derive(Deserialize)]
struct CheckRuns {
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CheckRun {
    id: u64,
    name: String,
    head_sha: String,
    status: String,
    conclusion: Option<String>,
    app: Option<CheckApp>,
}

#[derive(Deserialize)]
struct CheckApp {
    id: u64,
}

#[derive(Deserialize)]
struct CommitStatus {
    id: u64,
    context: String,
    state: String,
}

impl<T: GithubTransport> GithubCli<T> {
    pub(crate) async fn inspect_ci(
        &self,
        slug: &RepositorySlug,
        pull: &PullRequest,
        required: &[RequiredCheck],
    ) -> Result<CiSnapshot> {
        validate_required_checks(required)?;
        validate_sha(&pull.head_sha)?;
        validate_sha(&pull.base_sha)?;
        let mut snapshot = CiSnapshot::new(pull, required);
        if self.load_ci(slug, pull, &mut snapshot).await.is_err() {
            snapshot.set(
                CiState::Unavailable,
                "GitHub could not supply complete CI evidence for this exact pull request revision",
            );
        }
        Ok(snapshot)
    }

    async fn load_ci(
        &self,
        slug: &RepositorySlug,
        expected: &PullRequest,
        snapshot: &mut CiSnapshot,
    ) -> Result<()> {
        let response = self.read_pull(slug, expected.number).await?;
        let current = response.publishable(slug)?;
        if !current.open
            || current.draft
            || !same_revision(&current, expected)
            || self.branch_head(slug, &current.head_branch).await? != expected.head_sha
            || self.branch_head(slug, &current.base_branch).await? != expected.base_sha
        {
            snapshot.set(
                CiState::Unavailable,
                "The pull request is not open for the recorded head and destination revisions",
            );
            return Ok(());
        }
        match response.mergeable {
            None => {
                snapshot.set(
                    CiState::Pending,
                    "GitHub is computing the test merge for these revisions",
                );
                return Ok(());
            }
            Some(false) => {
                snapshot.set(
                    CiState::Failed,
                    "GitHub found a conflict between the recorded head and destination revisions",
                );
                return Ok(());
            }
            Some(true) => {}
        }
        let Some(test_sha) = current.merge_commit_sha else {
            snapshot.set(
                CiState::Pending,
                "GitHub has not published the test merge SHA",
            );
            return Ok(());
        };
        let test = self.commit(slug, &test_sha).await?;
        if !commit_parents_match(&test, expected) {
            snapshot.set(
                CiState::Pending,
                "The GitHub test merge was computed for a different head or destination revision",
            );
            return Ok(());
        }
        let mut evaluated_sha = test_sha;
        let mut evidence = self.checks_at(slug, &evaluated_sha).await?;
        // GitHub gives precedence to checks on the test merge when it has them.
        if evidence.is_empty() {
            let head = self.commit(slug, &expected.head_sha).await?;
            if head.tree.sha != test.tree.sha {
                snapshot.set(
                    CiState::Pending,
                    "CI must test the integrated tree for the recorded destination revision",
                );
                return Ok(());
            }
            evaluated_sha = expected.head_sha.clone();
            evidence = self.checks_at(slug, &evaluated_sha).await?;
        }
        let state = evaluate_checks(&snapshot.required_checks, &evidence);
        if matches!(state, CiState::Passed | CiState::Failed)
            && (self.branch_head(slug, &current.head_branch).await? != expected.head_sha
                || self.branch_head(slug, &current.base_branch).await? != expected.base_sha)
        {
            snapshot.set(
                CiState::Unavailable,
                "The GitHub head or destination changed while collecting CI evidence",
            );
            return Ok(());
        }
        snapshot.tested_sha = evaluated_sha;
        snapshot.tested_tree = Some(test.tree.sha);
        snapshot.checks = evidence;
        match state {
            CiState::Passed => snapshot.state = state,
            CiState::Failed => snapshot.set(
                state,
                "A required GitHub check failed on the recorded tested SHA",
            ),
            _ => snapshot.set(
                state,
                "Required checks are missing or pending on the recorded tested SHA",
            ),
        }
        Ok(())
    }

    async fn checks_at(&self, slug: &RepositorySlug, sha: &str) -> Result<Vec<CheckEvidence>> {
        validate_sha(sha)?;
        let runs = self
            .pages(&slug.endpoint(&format!("/commits/{sha}/check-runs?filter=latest"))?)
            .await?;
        let statuses = self
            .pages(&slug.endpoint(&format!("/commits/{sha}/statuses"))?)
            .await?;
        let mut checks = Vec::new();
        for page in runs {
            let runs = serde_json::from_value::<CheckRuns>(page)?;
            for run in runs.check_runs {
                ensure!(
                    run.head_sha == sha && run.id != 0,
                    "GitHub returned a check run for another revision"
                );
                let state = if run.status != "completed" {
                    CiState::Pending
                } else if matches!(
                    run.conclusion.as_deref(),
                    Some("success" | "skipped" | "neutral")
                ) {
                    CiState::Passed
                } else {
                    CiState::Failed
                };
                checks.push(CheckEvidence {
                    context: run.name,
                    app_id: run.app.map(|app| app.id),
                    state,
                    source: "check_run".into(),
                    id: run.id,
                });
            }
        }
        let mut seen_statuses = HashSet::new();
        for page in statuses {
            for status in serde_json::from_value::<Vec<CommitStatus>>(page)? {
                ensure!(status.id != 0, "GitHub returned an invalid commit status");
                if !seen_statuses.insert(status.context.to_lowercase()) {
                    continue;
                }
                let state = match status.state.as_str() {
                    "success" => CiState::Passed,
                    "pending" => CiState::Pending,
                    _ => CiState::Failed,
                };
                checks.push(CheckEvidence {
                    context: status.context,
                    app_id: None,
                    state,
                    source: "commit_status".into(),
                    id: status.id,
                });
            }
        }
        checks.sort_unstable_by(|left, right| {
            left.source.cmp(&right.source).then(left.id.cmp(&right.id))
        });
        Ok(checks)
    }
}

pub(super) fn same_revision(current: &PullRequest, expected: &PullRequest) -> bool {
    current.repository_id == expected.repository_id
        && current.number == expected.number
        && current.id == expected.id
        && current.head_branch == expected.head_branch
        && current.base_branch == expected.base_branch
        && current.head_sha == expected.head_sha
        && current.base_sha == expected.base_sha
}

pub(super) fn commit_parents_match(commit: &GitCommit, pull: &PullRequest) -> bool {
    match commit.parents.as_slice() {
        [base, head] => base.sha == pull.base_sha && head.sha == pull.head_sha,
        _ => false,
    }
}

fn evaluate_checks(required: &[RequiredCheck], evidence: &[CheckEvidence]) -> CiState {
    let mut pending = false;
    for required in required {
        let mut has_matching_check_run = false;
        let mut has_matching_evidence = false;
        for check in evidence
            .iter()
            .filter(|check| check.matches_context(&required.context))
        {
            if check.source == "check_run" {
                if required.app_id.is_some() && check.app_id != required.app_id {
                    continue;
                }
                has_matching_check_run = true;
            }
            has_matching_evidence = true;
            match check.state {
                CiState::Failed => return CiState::Failed,
                CiState::Pending | CiState::Unavailable => pending = true,
                CiState::Passed => {}
            }
        }
        pending |= !has_matching_evidence || (required.app_id.is_some() && !has_matching_check_run);
    }
    if pending {
        CiState::Pending
    } else {
        CiState::Passed
    }
}
