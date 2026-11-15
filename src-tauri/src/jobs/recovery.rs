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
    history::{effective_release_blocker, uses_recorded_checkpoint},
    load_job,
    pipeline::load_attempt_job,
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
    pub recorded: bool,
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
    Side {
        run_id: String,
    },
    Command {
        run_id: String,
        launch_requested: bool,
    },
}

impl RecoveryCheckpoint {
    pub(super) fn run_id(&self) -> &str {
        match self {
            Self::Agent(source) => &source.source_run_id,
            Self::Command { run_id, .. } | Self::Side { run_id } => run_id,
        }
    }

    pub(super) fn context_id(&self) -> Option<&str> {
        match self {
            Self::Agent(source) => Some(&source.result_context_id),
            Self::Command { .. } | Self::Side { .. } => None,
        }
    }

    pub(super) fn actions(&self, mode: SessionMode) -> Vec<RecoveryAction> {
        match self {
            Self::Agent(source) => source.actions(mode),
            Self::Side { .. }
            | Self::Command {
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
    load_recovery_source(db, job, None)
}

pub(super) fn recovery_source_for_goal_resume(
    db: &Connection,
    job: &Job,
    revision: &str,
) -> Result<Option<RecoveryCheckpoint>> {
    load_recovery_source(db, job, Some(revision))
}

fn load_recovery_source(
    db: &Connection,
    job: &Job,
    goal_revision: Option<&str>,
) -> Result<Option<RecoveryCheckpoint>> {
    let Some(attempt) = job.latest_attempt.as_ref() else {
        return Ok(None);
    };
    let unsettled: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM unconfirmed_managed_goal_stops WHERE run_id = ?1)",
        [&attempt.run_id],
        |row| row.get(0),
    )?;
    if unsettled {
        return Ok(None);
    }
    let native_executor: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_goal_executors WHERE run_id = ?1)",
        [&attempt.run_id],
        |row| row.get(0),
    )?;
    if native_executor
        && goal_revision.is_none()
        && !pending_recovery(db, job)?.is_some_and(|(run, _)| run == attempt.run_id)
    {
        return Ok(None);
    }
    match attempt.status {
        RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted => {}
        RunStatus::Succeeded
            if pending_recovery(db, job)?.is_some_and(|(run, _)| run == attempt.run_id) => {}
        RunStatus::Succeeded
            if let Some(revision) = goal_revision
                && super::goal_control::confirmed_goal_pause(
                    db,
                    job,
                    revision,
                    &attempt.run_id,
                )? => {}
        _ => return Ok(None),
    }
    if super::instruction_delivery::adopted_run(db, &job.id)?.is_some()
        || super::instruction_delivery::unconfirmed_adoption(db, &attempt.run_id)?
        || super::question_waits::has_open_questions(db, &attempt.run_id)?
    {
        return Ok(None);
    }
    if job.kind == JobKind::Side {
        return Ok(Some(RecoveryCheckpoint::Side {
            run_id: attempt.run_id.clone(),
        }));
    }
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
    let recorded = uses_recorded_checkpoint(db, job, &attempt.run_id, &conversation_id)?;
    let current_result = if recorded {
        match sealed {
            Some(id) => Some(id),
            None => pending_recovery(db, job)?
                .and_then(|(run, context)| (run == attempt.run_id).then_some(context).flatten()),
        }
    } else {
        conversations::recovery_checkpoint(
            db,
            &conversation_id,
            &attempt.run_id,
            &input_context_id,
            sealed.as_deref(),
        )?
    };
    let Some(result_context_id) = current_result else {
        return Ok(None);
    };
    Ok(Some(RecoveryCheckpoint::Agent(Box::new(RecoverySource {
        source_run_id: attempt.run_id.clone(),
        conversation_id,
        input_context_id,
        result_context_id,
        recorded,
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
        let goal_executor: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM native_goal_executors WHERE run_id = ?1)",
            [&self.source_run_id],
            |row| row.get(0),
        )?;
        if goal_executor {
            return Ok(None);
        }
        // Replay attachments from their stored message revisions. The old
        // provider session may hold paths belonging to a retired attempt.
        let checkpoint_has_attachments: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation_contexts ctx, json_each(ctx.entries) entry
             JOIN message_attachments a ON a.message_revision_id = entry.value
             WHERE ctx.id = ?1 AND ctx.conversation_id = ?2)",
            params![self.input_context_id, self.conversation_id],
            |row| row.get(0),
        )?;
        if checkpoint_has_attachments {
            return Ok(None);
        }
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
           AND (r.kind = 'command' OR attempt.result_context_id = attempt.context_id
                OR EXISTS (SELECT 1 FROM job_side_requests WHERE job_id = j.id))
           AND NOT EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                           WHERE s.run_id = a.run_id)
         ON CONFLICT(job_id) DO NOTHING",
        [run_id],
    )?;
    if queued != 0 {
        let job = load_attempt_job(db, run_id)?.context("Recovery job disappeared")?;
        db.execute(
            "INSERT INTO goal_writer_recoveries(job_id, checkpoint_run_id)
             SELECT ?1, checkpoint_run_id FROM goal_writer_attempts WHERE run_id = ?2",
            params![job.id, run_id],
        )?;
        reconcile_approval(db, &job)?;
    }
    Ok(())
}

pub(crate) fn requeue_rejected_native_goal(db: &Connection, run: &str) -> Result<()> {
    use super::{DispatchRejection, policy::dispatch_rejection};
    if dispatch_rejection(db, run)? != Some(DispatchRejection::NativeGoalUnverified) {
        return Ok(());
    }
    let Some(job) = load_attempt_job(db, run)? else {
        return Ok(());
    };
    let writer = job.mode == SessionMode::ApprovedWrite;
    if writer && !unchanged_native_preflight(db, run)? {
        return Ok(());
    }
    let queued = db.execute(
        "INSERT INTO job_recoveries(job_id, source_run_id, action, context_id)
         SELECT j.id, attempt.run_id, CASE WHEN ?2 THEN 'resume' ELSE 'retry' END, a.result_context_id
         FROM jobs j JOIN job_attempts attempt ON attempt.job_id = j.id
         JOIN agent_attempts a ON a.run_id = attempt.run_id
         JOIN goal_revisions r ON r.initial_job_id = j.id
         JOIN active_goals g ON g.revision_id = r.id
         JOIN goal_revision_controls control ON control.revision_id = r.id
         WHERE a.run_id = ?1 AND NOT attempt.cancel_requested
           AND (attempt.ordinal = 1 OR EXISTS (
                SELECT 1 FROM native_goal_resume_checkpoints resume
                JOIN native_goal_executors previous ON previous.run_id = resume.source_run_id
                WHERE resume.job_id = j.id AND previous.revision_id = r.id AND previous.stop_confirmed
           ))
           AND r.continuation_owner = 'managed' AND control.state = 'active'
           AND j.queue_state IN ('queued', 'paused') AND a.result_context_id = a.context_id
           AND (NOT EXISTS (SELECT 1 FROM context_snapshots snap WHERE snap.run_id = a.run_id)
                OR EXISTS (SELECT 1 FROM native_goal_context_receipts seed WHERE seed.run_id = a.run_id))
           AND NOT EXISTS (SELECT 1 FROM native_goal_executors x WHERE x.run_id = a.run_id)
         ON CONFLICT(job_id) DO NOTHING",
        params![run, writer],
    )?;
    if queued != 0 {
        if writer {
            db.execute(
                "INSERT INTO goal_writer_recoveries VALUES (?1, ?2)",
                params![job.id, run],
            )?;
        }
        reconcile_approval(db, &job)?;
    }
    Ok(())
}

fn unchanged_native_preflight(db: &Connection, run: &str) -> Result<bool> {
    use crate::repository::AttemptWorktreeEvidence;
    let saved: Option<Vec<u8>> = db
        .query_row(
            "SELECT evidence FROM job_worktree_candidates WHERE run_id = ?1 AND capture_confirmed",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    let Some(saved) = saved else { return Ok(false) };
    let AttemptWorktreeEvidence::Saved { artifact } =
        serde_json::from_slice(&protection::unprotect(&saved)?)?
    else {
        return Ok(false);
    };
    let before = &artifact.start.starting;
    let after = &artifact.candidate;
    Ok(before.head == after.head
        && before.status == after.status
        && before.index_digest == after.index_digest
        && before.staged_diff_digest == after.staged_diff_digest
        && before.unstaged_diff_digest == after.unstaged_diff_digest
        && before
            .changes
            .as_ref()
            .zip(after.changes.as_ref())
            .is_some_and(|(before, after)| {
                before
                    .untracked
                    .iter()
                    .map(|file| (&file.path, &file.sha256))
                    .eq(after
                        .untracked
                        .iter()
                        .map(|file| (&file.path, &file.sha256)))
            }))
}

pub(super) fn pending_writer_checkpoint(db: &Connection, job: &Job) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT checkpoint_run_id FROM goal_writer_recoveries WHERE job_id = ?1
             UNION ALL
             SELECT p.source_run_id FROM native_goal_resume_checkpoints p
             JOIN job_worktree_candidates w ON w.run_id = p.source_run_id AND w.capture_confirmed
             WHERE p.job_id = ?1 LIMIT 1",
            [&job.id],
            |row| row.get(0),
        )
        .optional()?)
}

fn writer_checkpoint_for_recovery(
    db: &Connection,
    job: &Job,
    run: &str,
    action: RecoveryAction,
    goal_revision: Option<&str>,
) -> Result<Option<String>> {
    if action == RecoveryAction::Resume
        && let Some(revision) = goal_revision
        && super::goal_control::writer_pause_checkpoint(db, job, revision, run)?
    {
        return Ok(Some(run.to_owned()));
    }
    Ok(db.query_row(
        "SELECT checkpoint.checkpoint_run_id FROM goal_writer_attempts checkpoint
         JOIN job_attempts attempt ON attempt.run_id = checkpoint.run_id
         WHERE checkpoint.run_id = ?1 AND attempt.job_id = ?2 AND attempt.process_launch_requested = 0",
        params![run, job.id], |row| row.get(0),
    ).optional()?)
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
        self.queue_job_recovery_with_goal(project_id, job_id, expected_run, action, None)
            .await
    }

    pub(super) async fn queue_job_recovery_with_goal(
        &self,
        project_id: String,
        job_id: String,
        expected_run: String,
        action: RecoveryAction,
        goal_revision: Option<String>,
    ) -> Result<Job> {
        let (checkout_id, owner, scope, refresh_desktop, writer_checkpoint) = self
            .execute({
                let (project, id) = (project_id.clone(), job_id.clone());
                let (run, revision) = (expected_run.clone(), goal_revision.clone());
                move |db| {
                    let job = load_job(db, &project, &id)?
                        .context("Job is unavailable in this project")?;
                    ensure!(job.downstream_wait.is_none(), "The job is waiting for downstream results");
                    ensure!(effective_release_blocker(db, &job)?.is_none(),
                        "The pipeline job needs an accepted release for the current plan and worktree");
                    let scope = recovery_scope(db, &job)?;
                    let refresh = (job.origin == JobOrigin::Operator || job.operator_requested == Some(true))
                        && matches!(&scope, RecoveryProfile::Agent(profile)
                            if profile.permission_revision == DESKTOP_PERMISSION_REVISION);
                    let writer_checkpoint = writer_checkpoint_for_recovery(db, &job, &run, action, revision.as_deref())?;
                    Ok((job.checkout_id, job.worktree_owner_task_id, scope, refresh, writer_checkpoint))
                }
            })
            .await?;
        if let Some(owner) = owner.as_ref() {
            if let Some(checkpoint) = &writer_checkpoint {
                repository::verify_managed_recovery_candidate(
                    self,
                    project_id.clone(),
                    owner.clone(),
                    checkout_id,
                    checkpoint.clone(),
                )
                .await?;
            } else {
                repository::verify_managed_recovery_checkout(
                    self,
                    project_id.clone(),
                    owner.clone(),
                    checkout_id,
                )
                .await?;
            }
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
            ensure!(
                scope.account_binding.is_none() || scope.account_binding == profile.account_binding,
                "The configured CLI account changed or its identity is unavailable"
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
                    job.downstream_wait.is_none(),
                    "The job is waiting for downstream results"
                );
                ensure!(
                    effective_release_blocker(&tx, &job)?.is_none(),
                    "The pipeline job needs an accepted release for the current plan and worktree"
                );
                let checkpoint = load_recovery_source(&tx, &job, goal_revision.as_deref())?
                    .context("The job no longer has a recoverable checkpoint")?;
                ensure!(
                    checkpoint.run_id() == expected_run,
                    "The job has a newer attempt"
                );
                let current_writer = writer_checkpoint_for_recovery(&tx, &job, &expected_run, action, goal_revision.as_deref())?;
                ensure!(current_writer == writer_checkpoint, "The goal's worktree checkpoint changed during recovery");
                ensure!(
                    checkpoint.actions(job.mode).contains(&action)
                        || (action == RecoveryAction::Resume && goal_revision.is_some() && writer_checkpoint.as_deref() == Some(&expected_run)),
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
                    if let Some(revision) = &goal_revision {
                        super::goal_control::activate_paused_goal(
                            &tx,
                            &project_id,
                            revision,
                            &job_id,
                        )?;
                    }
                    tx.commit()?;
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
                if let Some(writer_checkpoint) = writer_checkpoint {
                    tx.execute(
                        "INSERT INTO goal_writer_recoveries(job_id, checkpoint_run_id) VALUES (?1, ?2)",
                        params![job.id, writer_checkpoint],
                    )?;
                }
                if job.queue_state != QueueState::Paused {
                    tx.execute(
                        "UPDATE jobs SET queue_state = 'queued' WHERE id = ?1",
                        [&job.id],
                    )?;
                }
                let job =
                    load_job(&tx, &project_id, &job_id)?.context("Recovery job disappeared")?;
                reconcile_approval(&tx, &job)?;
                if let Some(revision) = &goal_revision {
                    super::goal_control::activate_paused_goal(&tx, &project_id, revision, &job_id)?;
                }
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }
}
