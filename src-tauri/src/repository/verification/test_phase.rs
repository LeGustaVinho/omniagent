use std::{collections::BTreeSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::diagnostics;

use super::{
    CandidateGateAction, CandidateGateOutcome, CandidateRevision, PinnedGate, SelectedCandidate,
    command, load_capture, load_policy,
    materialize::{ScratchGit, materialize_test_snapshot},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestPhase {
    Baseline,
    Red,
    Green,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestPhaseConfiguration {
    pub phase: TestPhase,
    pub check: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_test_paths: Vec<String>,
}

impl TestPhaseConfiguration {
    pub(crate) fn valid(&self) -> bool {
        !self.check.trim().is_empty()
            && self.check.len() <= 256
            && if self.phase == TestPhase::Red {
                !self.allowed_test_paths.is_empty()
                    && self.allowed_test_paths.len() <= 64
                    && self.allowed_test_paths.iter().all(|rule| {
                        if let Some(suffix) = rule.strip_prefix("*.") {
                            !suffix.is_empty() && relative_path(suffix) && !suffix.contains('/')
                        } else {
                            relative_path(rule.strip_suffix('/').unwrap_or(rule))
                        }
                    })
            } else {
                self.allowed_test_paths.is_empty()
            }
    }

    pub(crate) fn required_input(&self, name: &str) -> Option<bool> {
        match name {
            "candidate" | "prior" => Some(self.phase != TestPhase::Baseline),
            "tests" => Some(self.phase == TestPhase::Red),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestSpecification {
    pub paths: Vec<String>,
    pub tests: Vec<ExpectedFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedFailure {
    pub id: String,
    pub reason: String,
}

fn relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains(['\0', '\r', '\n', '\\', ':', '*', '?'])
        && path
            .split('/')
            .all(|part| !matches!(part, "" | "." | "..") && !part.eq_ignore_ascii_case(".git"))
}

impl TestSpecification {
    pub(crate) fn validate(&self, configuration: &TestPhaseConfiguration) -> Result<()> {
        ensure!(
            (1..=64).contains(&self.paths.len()),
            "Declare the complete patch of test paths"
        );
        let mut paths = BTreeSet::new();
        for path in &self.paths {
            ensure!(
                relative_path(path) && paths.insert(path),
                "Test paths must be distinct, relative Git paths"
            );
            ensure!(
                configuration.allowed_test_paths.iter().any(|rule| {
                    if let Some(suffix) = rule.strip_prefix('*') {
                        path.ends_with(suffix)
                    } else if rule.ends_with('/') {
                        path.starts_with(rule)
                    } else {
                        path == rule
                    }
                }),
                "A declared test path is outside the pinned test path rules"
            );
        }
        let mut ids = BTreeSet::new();
        ensure!(
            (1..=64).contains(&self.tests.len())
                && self.tests.iter().all(|item| !item.id.trim().is_empty()
                    && item.id.len() <= 256
                    && ids.insert(&item.id)
                    && !item.reason.trim().is_empty()
                    && item.reason.len() <= 512),
            "Declare each expected failing test ID and its exact failure reason"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    Verified,
    Failed,
    Blocked,
}

impl TestVerdict {
    pub(crate) fn port(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TestReport {
    status: SuiteStatus,
    #[serde(default)]
    tests: Vec<ReportedTest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SuiteStatus {
    Completed,
    EnvironmentError,
    BuildError,
    ToolError,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReportedTest {
    id: String,
    status: TestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TestStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCheckResult {
    pub verdict: TestVerdict,
    pub reason: Option<String>,
    pub exit_code: Option<u32>,
    pub report: Option<TestReport>,
    pub tree: Option<String>,
    pub test_patch_revision: Option<String>,
}

impl TestCheckResult {
    pub(crate) fn rejected(verdict: TestVerdict, reason: impl AsRef<str>) -> Self {
        Self {
            verdict,
            reason: Some(diagnostics::preview(reason.as_ref())),
            exit_code: None,
            report: None,
            tree: None,
            test_patch_revision: None,
        }
    }
}

pub(crate) struct PinnedTestGate {
    pub policy_revision: String,
    gate: Option<PinnedGate>,
}

impl PinnedTestGate {
    pub(crate) fn has_registered_command(&self) -> bool {
        self.gate.as_ref().is_some_and(|gate| {
            matches!(gate.gate.action, CandidateGateAction::Command { .. })
                && gate.executable_revision.is_some()
        })
    }
}

pub(crate) fn load_test_gate(db: &Connection, project: &str, name: &str) -> Result<PinnedTestGate> {
    let (policy_revision, gates) = load_policy(db, project)?;
    Ok(PinnedTestGate {
        policy_revision,
        gate: gates.into_iter().find(|gate| gate.gate.name == name),
    })
}

pub(crate) struct FrozenTestCandidate {
    pub selection: SelectedCandidate,
    capture: CandidateRevision,
}

pub(crate) fn freeze_test_candidate(
    db: &Connection,
    candidate: &SelectedCandidate,
) -> Result<FrozenTestCandidate> {
    super::require_current(db, candidate)?;
    let (capture, checkout) = load_capture(db, &candidate.target)?;
    ensure!(
        checkout == candidate.checkout_id,
        "The test candidate checkout changed"
    );
    Ok(FrozenTestCandidate {
        selection: candidate.clone(),
        capture,
    })
}

pub(crate) struct TestExecution {
    pub gate: PinnedTestGate,
    pub phase: TestPhase,
    pub common_dir: PathBuf,
    pub base: String,
    pub specification: Option<TestSpecification>,
    pub candidate: Option<FrozenTestCandidate>,
    pub expected_patch: Option<String>,
}

impl TestExecution {
    pub(crate) fn run(self) -> TestCheckResult {
        match self.run_snapshot() {
            Ok(result) => result,
            Err(error) => TestCheckResult::rejected(TestVerdict::Blocked, error.to_string()),
        }
    }

    fn run_snapshot(&self) -> Result<TestCheckResult> {
        let gate = self
            .gate
            .gate
            .as_ref()
            .context("Configure the named test command in the project check policy")?;
        let (checkout, tree, patch) = match &self.candidate {
            Some(candidate) => {
                let specification = self
                    .specification
                    .as_ref()
                    .context("Missing pinned test specification")?;
                let snapshot = materialize_test_snapshot(
                    &candidate.capture,
                    &candidate.selection,
                    &specification.paths,
                    self.phase == TestPhase::Red,
                    self.expected_patch.as_deref(),
                );
                let (checkout, patch) = match snapshot? {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        return Ok(TestCheckResult::rejected(TestVerdict::Failed, error));
                    }
                };
                (checkout, candidate.selection.tree.clone(), Some(patch))
            }
            None => {
                let (checkout, tree) = ScratchGit::new(&self.common_dir, &self.base)?
                    .materialize_commit(&self.base)?;
                (checkout, tree, None)
            }
        };
        let outcome = command::run_in_checkout(
            checkout.path(),
            &gate.gate.action,
            gate.executable_revision.as_deref(),
        );
        let mut check = verify_report(self.phase, self.specification.as_ref(), outcome);
        check.tree = Some(tree);
        check.test_patch_revision = patch;
        Ok(check)
    }
}

fn verify_report(
    phase: TestPhase,
    specified: Option<&TestSpecification>,
    outcome: CandidateGateOutcome,
) -> TestCheckResult {
    let (exit, stdout, truncated) = match outcome {
        CandidateGateOutcome::Passed {
            stdout, truncated, ..
        } => (0, stdout, truncated),
        CandidateGateOutcome::Failed {
            exit_code,
            stdout,
            truncated,
            ..
        } => (exit_code, stdout, truncated),
        CandidateGateOutcome::CouldNotRun { reason }
        | CandidateGateOutcome::Interrupted { reason, .. } => {
            return TestCheckResult::rejected(TestVerdict::Blocked, reason);
        }
    };
    let mut result = TestCheckResult::rejected(
        TestVerdict::Blocked,
        "The test command did not emit a complete structured report",
    );
    result.exit_code = Some(exit);
    if truncated || stdout.len() > 64 * 1024 {
        return result;
    }
    let Ok(report) = serde_json::from_str::<TestReport>(&stdout) else {
        return result;
    };
    let mut ids = BTreeSet::new();
    if !report
        .tests
        .iter()
        .all(|test| !test.id.trim().is_empty() && ids.insert(&test.id))
    {
        return TestCheckResult {
            reason: Some("The test report has missing or duplicate IDs".into()),
            ..result
        };
    }
    let (verdict, reason) = match report.status {
        SuiteStatus::EnvironmentError | SuiteStatus::BuildError | SuiteStatus::ToolError => (
            TestVerdict::Blocked,
            Some(
                report
                    .reason
                    .as_deref()
                    .unwrap_or("The suite could not execute the tests"),
            ),
        ),
        SuiteStatus::Completed => evaluate_completed(phase, specified, &report.tests, exit),
    };
    TestCheckResult {
        verdict,
        reason: reason.map(diagnostics::preview),
        report: Some(report),
        ..result
    }
}

fn evaluate_completed(
    phase: TestPhase,
    specified: Option<&TestSpecification>,
    reports: &[ReportedTest],
    exit: u32,
) -> (TestVerdict, Option<&'static str>) {
    let failure_count = reports
        .iter()
        .filter(|test| test.status == TestStatus::Failed)
        .count();
    if phase == TestPhase::Red {
        let matched = specified.is_some_and(|specified| {
            failure_count == specified.tests.len()
                && specified.tests.iter().all(|expected| {
                    reports.iter().any(|report| {
                        report.id == expected.id
                            && report.status == TestStatus::Failed
                            && report.reason.as_ref() == Some(&expected.reason)
                    })
                })
        });
        if exit != 0 && matched {
            return (TestVerdict::Verified, None);
        }
        return (
            TestVerdict::Failed,
            Some("Red requires every declared test to fail for its exact expected reason"),
        );
    }
    let expected_passed = specified.is_none_or(|specified| {
        specified.tests.iter().all(|expected| {
            reports
                .iter()
                .any(|report| report.id == expected.id && report.status == TestStatus::Passed)
        })
    });
    if exit == 0 && failure_count == 0 && expected_passed {
        (TestVerdict::Verified, None)
    } else {
        (
            TestVerdict::Failed,
            Some("The required tests did not all pass"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn red_requires_an_assertion_failure_for_every_exact_id_and_reason() {
        let specification = TestSpecification {
            paths: vec!["tests/submit.case".into()],
            tests: vec![ExpectedFailure {
                id: "BDD-Submit".into(),
                reason: "submit is unsupported".into(),
            }],
        };
        let result = |phase, exit, value: serde_json::Value| {
            let stdout = value.to_string();
            let outcome = if exit == 0 {
                CandidateGateOutcome::Passed {
                    stdout,
                    stderr: String::new(),
                    truncated: false,
                }
            } else {
                CandidateGateOutcome::Failed {
                    exit_code: exit,
                    stdout,
                    stderr: String::new(),
                    truncated: false,
                }
            };
            verify_report(phase, Some(&specification), outcome)
        };
        let correct = json!({"status":"completed","tests":[
            {"id":"BDD-Submit","status":"failed","reason":"submit is unsupported"}
        ]});
        assert_eq!(
            result(TestPhase::Red, 1, correct.clone()).verdict,
            TestVerdict::Verified
        );
        assert_eq!(
            result(TestPhase::Red, 0, correct).verdict,
            TestVerdict::Failed
        );
        let wrong = json!({"status":"completed","tests":[
            {"id":"BDD-Submit","status":"failed","reason":"wrong message"}
        ]});
        assert_eq!(
            result(TestPhase::Red, 1, wrong).verdict,
            TestVerdict::Failed
        );
        for status in ["build_error", "environment_error", "tool_error"] {
            let blocked = result(
                TestPhase::Red,
                1,
                json!({"status":status, "reason":"Fixture could not run"}),
            );
            assert_eq!(blocked.verdict, TestVerdict::Blocked);
            assert_eq!(blocked.reason.as_deref(), Some("Fixture could not run"));
        }
        assert_eq!(
            result(
                TestPhase::Green,
                0,
                json!({"status":"completed","tests":[]})
            )
            .verdict,
            TestVerdict::Failed
        );
        let pass = json!({"status":"completed","tests":[{"id":"BDD-Submit","status":"passed"}]});
        assert_eq!(
            result(TestPhase::Green, 0, pass).verdict,
            TestVerdict::Verified
        );
    }
}
