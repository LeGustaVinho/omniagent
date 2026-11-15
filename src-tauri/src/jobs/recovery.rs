use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    conversations::{self, NativeJobRecovery, SessionScope},
    dispatch::ContextSnapshot,
    protection,
    providers::{Provider, SessionMode},
    runs::RunStatus,
    store::Store,
};

use super::{Job, QueueState, load_job};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum RecoveryAction {
    Retry,
    Resume,
}

impl RecoveryAction {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Resume => "resume",
        }
    }

    pub(super) fn from_key(value: &str) -> Result<Self> {
        match value {
            "retry" => Ok(Self::Retry),
            "resume" => Ok(Self::Resume),
            _ => anyhow::bail!("unknown job recovery action"),
        }
    }

    pub(super) fn from_attempt_kind(value: &str) -> Result<Option<Self>> {
        if value == "initial" {
            Ok(None)
        } else {
            Self::from_key(value).map(Some)
        }
    }
}

pub(super) struct RecoverySource {
    pub source_run_id: String,
    pub conversation_id: String,
    pub input_context_id: String,
    pub result_context_id: String,
    pub status: RunStatus,
    provider_terminal: Option<String>,
    session: Option<String>,
    provider_turn_id: Option<String>,
    dispatched: bool,
    launch_requested: bool,
}

pub(super) fn recovery_source(db: &Connection, job: &Job) -> Result<Option<RecoverySource>> {
    let Some(attempt) = job.latest_attempt.as_ref().filter(|attempt| {
        matches!(
            attempt.status,
            RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted
        )
    }) else {
        return Ok(None);
    };
    let (conversation_id, input_context_id, sealed, provider_terminal, session, provider_turn_id, dispatched, launch_requested):
        (String, String, Option<String>, Option<String>, Option<String>, Option<String>, bool, bool) = db.query_row(
        "SELECT a.conversation_id, a.context_id, a.result_context_id, a.provider_terminal,
                s.provider_session_id, a.provider_turn_id,
                EXISTS (SELECT 1 FROM context_snapshots x JOIN dispatches d ON d.snapshot_id = x.id WHERE x.run_id = a.run_id),
                j.process_launch_requested
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         JOIN job_attempts j ON j.run_id = a.run_id WHERE a.run_id = ?1",
        [&attempt.run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
    )?;
    let Some(result_context_id) = conversations::recovery_checkpoint(
        db,
        &conversation_id,
        &attempt.run_id,
        &input_context_id,
        sealed.as_deref(),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(RecoverySource {
        source_run_id: attempt.run_id.clone(),
        conversation_id,
        input_context_id,
        result_context_id,
        status: attempt.status,
        provider_terminal,
        session,
        provider_turn_id,
        dispatched,
        launch_requested,
    }))
}

impl RecoverySource {
    pub(super) fn actions(&self, mode: SessionMode) -> Vec<RecoveryAction> {
        // A write may already have affected the candidate even when delivery was uncertain.
        // Keep its recovery pending review until a reconciled worktree checkpoint is available.
        if mode == SessionMode::ApprovedWrite
            && (self.launch_requested || self.dispatched || self.status == RunStatus::Interrupted)
        {
            return Vec::new();
        }
        let mut actions = vec![RecoveryAction::Retry];
        if self.dispatched || self.result_context_id != self.input_context_id {
            actions.push(RecoveryAction::Resume);
        }
        actions
    }

    pub(super) fn native_resume(
        &self,
        db: &Connection,
        scope: &SessionScope,
    ) -> Result<Option<NativeJobRecovery>> {
        if self.status != RunStatus::Cancelled
            || self.provider_terminal.as_deref() != Some("interrupted")
            || scope
                .account_binding
                .as_ref()
                .is_none_or(|account| account.is_empty())
        {
            return Ok(None);
        }
        let Some(session) = &self.session else {
            return Ok(None);
        };
        let encrypted: Option<Vec<u8>> = db.query_row(
            "SELECT x.payload FROM context_snapshots x JOIN dispatches d ON d.snapshot_id = x.id
             WHERE x.run_id = ?1 AND d.ordinal = 1 AND d.delivery = 'submitted'",
            [&self.source_run_id], |row| row.get(0),
        ).optional()?;
        let Some(bytes) = encrypted else {
            return Ok(None);
        };
        let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&bytes)?)?;
        if snapshot.scope != *scope || snapshot.context_id != self.input_context_id {
            return Ok(None);
        }
        let (Some(assessment), Some(scoped_launch)) =
            (snapshot.capabilities, snapshot.scoped_launch)
        else {
            return Ok(None);
        };
        if scope.provider == Provider::Antigravity
            && !assessment.entries.iter().any(|entry| {
                use crate::providers::{
                    Capability,
                    capabilities::{Availability, CapabilitySubject},
                };
                entry.descriptor.subject
                    == (CapabilitySubject::Protocol {
                        capability: Capability::Resume,
                    })
                    && entry.descriptor.availability == Availability::Available
            })
        {
            return Ok(None);
        }
        Ok(assessment
            .scope
            .executable_revision
            .map(|executable_revision| NativeJobRecovery {
                source_run_id: self.source_run_id.clone(),
                provider_session_id: session.clone(),
                provider_turn_id: self.provider_turn_id.clone(),
                context_id: self.input_context_id.clone(),
                executable_revision,
                approved_customizations: scoped_launch.approved,
            }))
    }
}

pub(super) fn pending_recovery(db: &Connection, job: &Job) -> Result<Option<(String, String)>> {
    Ok(db
        .query_row(
            "SELECT source_run_id, context_id FROM job_recoveries WHERE job_id = ?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

impl Store {
    pub async fn queue_job_recovery(
        &self,
        project_id: String,
        job_id: String,
        expected_run: String,
        action: RecoveryAction,
    ) -> Result<Job> {
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let job = load_job(&tx, &project_id, &job_id)?
                    .context("Job is unavailable in this project")?;
                let checkpoint = recovery_source(&tx, &job)?
                    .context("The job no longer has a recoverable checkpoint")?;
                ensure!(
                    checkpoint.source_run_id == expected_run,
                    "The job has a newer attempt"
                );
                ensure!(
                    checkpoint.actions(job.mode).contains(&action),
                    "This recovery action needs a confirmed checkpoint"
                );
                if let Some(queued_action) = job.queued_recovery {
                    ensure!(
                        queued_action == action,
                        "A different recovery is already queued"
                    );
                    return Ok(job);
                }
                tx.execute(
                    "INSERT INTO job_recoveries VALUES (?1, ?2, ?3, ?4)",
                    params![
                        job.id,
                        checkpoint.source_run_id,
                        action.key(),
                        checkpoint.result_context_id
                    ],
                )?;
                if job.queue_state != QueueState::Paused {
                    tx.execute(
                        "UPDATE jobs SET queue_state = 'queued' WHERE id = ?1",
                        [&job.id],
                    )?;
                }
                let job =
                    load_job(&tx, &project_id, &job_id)?.context("Recovery job disappeared")?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }
}
