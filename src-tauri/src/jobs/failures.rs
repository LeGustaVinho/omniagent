use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{process::TerminalOutcome, runs::RunStatus, store::Store};

use super::{DispatchRejection, policy::dispatch_rejection};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum FailurePhase {
    #[default]
    Unknown,
    Preparation,
    Launch,
    Startup,
    Dispatch,
    Execution,
    ProcessStop,
    EvidenceCapture,
    ResultValidation,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum FailureCause {
    WorktreeUnavailable,
    LaunchRejected,
    StartupTimeout,
    GoalTimeLimit,
    GoalAuthorityRevoked,
    ProviderError,
    TransportClosed,
    ProcessExit,
    StopUnconfirmed,
    EvidenceUnavailable,
    InvalidResult,
    Shutdown,
    RuntimeRestart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ObservedProcessExit {
    pub code: u32,
    pub cancelled: bool,
}

impl From<TerminalOutcome> for ObservedProcessExit {
    fn from(value: TerminalOutcome) -> Self {
        match value {
            TerminalOutcome::Exited(code) => Self {
                code,
                cancelled: false,
            },
            TerminalOutcome::Cancelled(code) => Self {
                code,
                cancelled: true,
            },
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct FailureObservation {
    pub phase: FailurePhase,
    pub cause: Option<FailureCause>,
    pub process: Option<ObservedProcessExit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobFailure {
    pub outcome: RunStatus,
    pub checkout_id: String,
    pub checkout_root: String,
    pub observation: FailureObservation,
    pub provider_outcome: Option<String>,
    pub logs_available: bool,
    pub saved_diff_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dispatch_rejection: Option<DispatchRejection>,
}

pub(crate) fn record_job_failure(
    db: &Connection,
    run_id: &str,
    outcome: RunStatus,
    observation: Option<FailureObservation>,
) -> Result<()> {
    if matches!(outcome, RunStatus::Failed | RunStatus::Interrupted) {
        let report = observation
            .map(|details| serde_json::to_string(&details))
            .transpose()?;
        db.execute(
            "INSERT INTO job_failures (run_id, checkout_root, report)
             SELECT a.run_id, c.root, ?2 FROM job_attempts a JOIN runs r ON r.id = a.run_id
             JOIN checkouts c ON c.id = r.checkout_id WHERE a.run_id = ?1",
            params![run_id, report],
        )?;
    }
    Ok(())
}

impl Store {
    pub async fn job_failure(&self, project: String, run: String) -> Result<Option<JobFailure>> {
        self.execute(move |db| {
            let row = db.query_row(
                "SELECT r.status, r.checkout_id, f.checkout_root, f.report, a.provider_terminal,
                    (EXISTS(SELECT 1 FROM agent_events e WHERE e.run_id = r.id)
                        OR EXISTS(SELECT 1 FROM job_command_outputs o WHERE o.run_id = r.id)),
                    EXISTS(SELECT 1 FROM job_worktree_candidates d WHERE d.run_id = r.id AND d.capture_confirmed = 1)
                 FROM job_failures f JOIN runs r ON r.id = f.run_id
                 LEFT JOIN agent_attempts a ON a.run_id = f.run_id JOIN job_attempts ja ON ja.run_id = f.run_id
                 JOIN jobs j ON j.id = ja.job_id WHERE j.project_id = ?1 AND f.run_id = ?2",
                params![project, run],
                |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get::<_, Option<String>>(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
            ).optional()?;
            row.map(|(outcome, checkout_id, checkout_root, report, provider_outcome, logs_available, saved_diff_available)| Ok(JobFailure {
                outcome: RunStatus::from_sql(&outcome)?,
                checkout_id,
                checkout_root,
                observation: report.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default(),
                provider_outcome,
                logs_available,
                saved_diff_available,
                dispatch_rejection: dispatch_rejection(db, &run)?,
            })).transpose()
        }).await
    }
}
