use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    conversations::{self, NativeJobRecovery, SessionScope},
    dispatch::ContextSnapshot,
    protection,
    providers::{
        Provider, SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    repository,
    runs::RunStatus,
    store::Store,
};

use super::{
    Job, JobKind, JobOrigin, QueueState,
    admission::recovery_scope,
    command::CommandInvocation,
    load_job,
    pipeline::{load_attempt_job, release_blocker},
    policy::reconcile_approval,
};

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

#[derive(PartialEq, Eq)]
pub(super) enum RecoveryProfile {
    Agent(SessionScope),
    Command(CommandInvocation),
}

impl RecoveryProfile {
    fn checkout(&self) -> &Path {
        match self {
            Self::Agent(scope) => &scope.checkout,
            Self::Command(invocation) => &invocation.checkout,
        }
    }

    fn executable(&self) -> &Path {
        match self {
            Self::Agent(scope) => &scope.executable,
            Self::Command(invocation) => &invocation.executable,
        }
    }
}

pub(super) enum RecoveryCheckpoint {
    Agent(Box<RecoverySource>),
    Command {
        run_id: String,
        launch_requested: bool,
    },
}

impl RecoveryCheckpoint {
    pub(super) fn run_id(&self) -> &str {
        match self {
            Self::Agent(source) => &source.source_run_id,
            Self::Command { run_id, .. } => run_id,
        }
    }

    pub(super) fn context_id(&self) -> Option<&str> {
        match self {
            Self::Agent(source) => Some(&source.result_context_id),
            Self::Command { .. } => None,
        }
    }

    pub(super) fn actions(&self, mode: SessionMode) -> Vec<RecoveryAction> {
        match self {
            Self::Agent(source) => source.actions(mode),
            Self::Command {
                launch_requested: false,
                ..
            } => vec![RecoveryAction::Retry],
            Self::Command {
                launch_requested: true,
                ..
            } => Vec::new(),
        }
    }
}

pub(super) fn recovery_source(db: &Connection, job: &Job) -> Result<Option<RecoveryCheckpoint>> {
    let Some(attempt) = job.latest_attempt.as_ref().filter(|attempt| {
        matches!(
            attempt.status,
            RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted
        )
    }) else {
        return Ok(None);
    };
    if job.kind == JobKind::Command {
        let launch_requested = db.query_row(
            "SELECT process_launch_requested FROM job_attempts WHERE run_id = ?1",
            [&attempt.run_id],
            |row| row.get(0),
        )?;
        return Ok(Some(RecoveryCheckpoint::Command {
            run_id: attempt.run_id.clone(),
            launch_requested,
        }));
    }
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
    Ok(Some(RecoveryCheckpoint::Agent(Box::new(RecoverySource {
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
    }))))
}

impl RecoverySource {
    pub(super) fn actions(&self, mode: SessionMode) -> Vec<RecoveryAction> {
        // A write may already have affected the candidate even when delivery was uncertain.
        // Keep its recovery pending review until a reconciled worktree checkpoint is available.
        if mode == SessionMode::ApprovedWrite && (self.launch_requested || self.dispatched) {
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

// Only work proven never launched can be retried automatically. Keep the
// previous attempt and its frozen context for the scheduler's normal checks.
pub(crate) fn requeue_unlaunched_job(db: &Connection, run_id: &str) -> Result<()> {
    let queued = db.execute(
        "INSERT INTO job_recoveries (job_id, source_run_id, action, context_id)
         SELECT j.id, a.run_id, 'retry', attempt.result_context_id
         FROM job_attempts a JOIN jobs j ON j.id = a.job_id
         JOIN runs r ON r.id = a.run_id
         LEFT JOIN agent_attempts attempt ON attempt.run_id = a.run_id
         WHERE a.run_id = ?1 AND a.process_launch_requested = 0 AND a.cancel_requested = 0
           AND j.queue_state IN ('queued', 'paused')
           AND (r.kind = 'command' OR attempt.result_context_id = attempt.context_id)
           AND NOT EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                           WHERE s.run_id = a.run_id)
         ON CONFLICT(job_id) DO NOTHING",
        [run_id],
    )?;
    if queued != 0 {
        let job = load_attempt_job(db, run_id)?.context("Recovery job disappeared")?;
        reconcile_approval(db, &job)?;
    }
    Ok(())
}

pub(super) fn pending_recovery(
    db: &Connection,
    job: &Job,
) -> Result<Option<(String, Option<String>)>> {
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
        let (checkout_id, owner, scope, refresh_desktop) = self
            .execute({
                let (project, id) = (project_id.clone(), job_id.clone());
                move |db| {
                    let job = load_job(db, &project, &id)?
                        .context("Job is unavailable in this project")?;
                    ensure!(release_blocker(db, &job)?.is_none(),
                        "The pipeline job needs an accepted release for the current plan and worktree");
                    let scope = recovery_scope(db, &job)?;
                    let refresh = job.origin == JobOrigin::Operator
                        && matches!(&scope, RecoveryProfile::Agent(profile)
                            if profile.permission_revision == DESKTOP_PERMISSION_REVISION && profile.account_binding.is_none());
                    Ok((job.checkout_id, job.worktree_owner_task_id, scope, refresh))
                }
            })
            .await?;
        if let Some(owner) = owner.as_ref() {
            repository::verify_managed_recovery_checkout(
                self,
                project_id.clone(),
                owner.clone(),
                checkout_id,
            )
            .await?;
        } else {
            repository::verify_checkout(self, checkout_id, scope.checkout().to_owned()).await?;
        }
        let refreshed = if refresh_desktop {
            let RecoveryProfile::Agent(scope) = &scope else {
                anyhow::bail!("The recovery has no desktop CLI profile");
            };
            let configured = self.configured_executable(scope.provider).await?;
            let configured_path = configured.executable.clone();
            let model = scope
                .model
                .clone()
                .context("The desktop job has no selected model")?;
            let antigravity_model =
                (scope.provider == Provider::Antigravity).then(|| model.clone());
            let profile =
                tokio::task::spawn_blocking(move || desktop_profile(configured, antigravity_model))
                    .await??;
            ensure!(
                profile.models.iter().any(|item| item.option.selectable
                    && item.option.selection.profile_id == scope.profile_id
                    && item.option.selection.model_id == model
                    && scope
                        .effort
                        .as_ref()
                        .is_none_or(|effort| item.efforts.contains(effort))),
                "The original job model or thinking level is unavailable"
            );
            let mut current = scope.clone();
            current.executable = profile.executable;
            current.cli_version = profile.version;
            Some((configured_path, current))
        } else {
            ensure!(
                scope.executable().is_file(),
                "The job's CLI executable is unavailable"
            );
            None
        };
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let job = load_job(&tx, &project_id, &job_id)?
                    .context("Job is unavailable in this project")?;
                ensure!(
                    release_blocker(&tx, &job)?.is_none(),
                    "The pipeline job needs an accepted release for the current plan and worktree"
                );
                let checkpoint = recovery_source(&tx, &job)?
                    .context("The job no longer has a recoverable checkpoint")?;
                ensure!(
                    checkpoint.run_id() == expected_run,
                    "The job has a newer attempt"
                );
                ensure!(
                    checkpoint.actions(job.mode).contains(&action),
                    "This recovery action needs a confirmed checkpoint"
                );
                ensure!(
                    recovery_scope(&tx, &job)? == scope && job.worktree_owner_task_id == owner,
                    "The effective job profile or worktree owner changed during recovery"
                );
                if let Some(queued_action) = job.queued_recovery {
                    ensure!(
                        queued_action == action,
                        "A different recovery is already queued"
                    );
                    return Ok(job);
                }
                if let Some((configured, current)) = refreshed {
                    let actual: Option<String> = tx
                        .query_row(
                            "SELECT executable FROM cli_executables WHERE provider = ?1",
                            [current.provider.as_str()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    ensure!(
                        actual == configured,
                        "The configured CLI changed during recovery"
                    );
                    tx.execute(
                        "UPDATE jobs SET operator_scope = ?1 WHERE id = ?2",
                        params![protection::protect(&serde_json::to_vec(&current)?)?, job.id],
                    )?;
                }
                tx.execute(
                    "INSERT INTO job_recoveries VALUES (?1, ?2, ?3, ?4)",
                    params![
                        job.id,
                        checkpoint.run_id(),
                        action.key(),
                        checkpoint.context_id()
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
                reconcile_approval(&tx, &job)?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }
}
