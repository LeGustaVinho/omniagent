use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::{SessionScope, consume_matching_draft, load_conversation},
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, TurnOutcome, goal::NativeGoalObservation,
        questions::QuestionRoute,
    },
    repository::writers::writer_owner,
    store::Store,
};

use super::{
    GoalControlState, Job, JobContext, JobOrigin, JobRequest, PipelineDispatch,
    history::{bind_pipeline_context, load_binding},
    insert_job, now_ms,
    pipeline::resolve_node_checkout,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalDefinition {
    pub objective: String,
    pub criteria: Vec<String>,
    pub max_turns: Option<u32>,
    pub max_seconds: Option<u32>,
    pub token_budget: Option<u64>,
}

impl GoalDefinition {
    fn validate(&self) -> Result<()> {
        ensure!(!self.objective.trim().is_empty(), "Enter a goal objective");
        ensure!(
            self.criteria
                .iter()
                .all(|criterion| !criterion.trim().is_empty()),
            "Goal criteria must be nonempty"
        );
        ensure!(
            self.max_turns != Some(0)
                && self.max_seconds != Some(0)
                && self.token_budget != Some(0),
            "Goal limits must be positive"
        );
        Ok(())
    }

    pub(super) fn instructions(&self) -> String {
        let mut prompt = format!(
            "Goal objective\n{}\n\nTake one bounded turn toward this goal. Before ending the turn, use report_goal_progress to record evidence and select the next action.",
            self.objective.trim()
        );
        if !self.criteria.is_empty() {
            prompt.push_str("\n\nAcceptance criteria\n");
            for criterion in &self.criteria {
                prompt.push_str("\n- ");
                prompt.push_str(criterion.trim());
            }
        }
        prompt
    }

    pub(super) fn native_objective(&self) -> String {
        let mut objective = self.objective.trim().to_owned();
        if !self.criteria.is_empty() {
            objective.push_str("\n\nAcceptance criteria\n");
            for criterion in &self.criteria {
                objective.push_str("\n- ");
                objective.push_str(criterion.trim());
            }
        }
        objective
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalContinuationOwner {
    Managed,
    Native,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalTurnAction {
    Continue,
    Review,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalTurnReport {
    pub evidence: String,
    pub action: GoalTurnAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCheckpoint {
    pub ordinal: u32,
    pub run_id: String,
    pub context_id: String,
    pub next_action: String,
    pub report: Option<GoalTurnReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRevision {
    pub goal_id: String,
    pub id: String,
    pub definition: GoalDefinition,
    pub continuation_owner: GoalContinuationOwner,
    pub control_state: GoalControlState,
    pub provider: Provider,
    pub model: Option<String>,
    pub job: Job,
    pub checkpoint: Option<GoalCheckpoint>,
    pub provider_session_id: Option<String>,
    pub provider_goal_id: Option<String>,
}

impl Store {
    pub async fn start_conversation_goal(
        &self,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
        submitted_draft: Option<String>,
    ) -> Result<GoalRevision> {
        self.start_goal(context, scope, task_id, definition, submitted_draft, None)
            .await
    }

    pub async fn start_pipeline_goal(
        &self,
        stage: PipelineDispatch,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
    ) -> Result<GoalRevision> {
        self.start_goal(context, scope, task_id, definition, None, Some(stage))
            .await
    }

    async fn start_goal(
        &self,
        context: JobContext,
        scope: SessionScope,
        task_id: Option<String>,
        definition: GoalDefinition,
        submitted_draft: Option<String>,
        stage: Option<PipelineDispatch>,
    ) -> Result<GoalRevision> {
        definition.validate()?;
        ensure!(
            scope.executable.is_absolute() && !scope.runner.is_empty(),
            "Invalid CLI profile"
        );
        let goal = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = load_conversation(&tx, &context.conversation_id)?;
            let (project, root) = if let Some(stage) = &stage {
                tx.query_row(
                    "SELECT id FROM conversation_contexts WHERE conversation_id = ?1 AND id = ?2",
                    params![conversation.id, context.context_id], |row| row.get::<_, String>(0),
                ).context("The stage's goal context is unavailable")?;
                resolve_node_checkout(&tx, stage, &conversation.checkout_id)?
            } else {
                ensure!(conversation.context_id == context.context_id, "The goal's context changed");
                tx.query_row(
                    "SELECT project_id, root FROM checkouts WHERE id = ?1",
                    [&conversation.checkout_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
            };
            ensure!(scope.checkout == PathBuf::from(root), "The goal's checkout changed");
            ensure!(
                !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1)",
                    [&conversation.id],
                    |row| row.get::<_, bool>(0),
                )?,
                "Wait until the conversation reaches a turn boundary before starting a goal"
            );
            ensure!(
                active_revision(&tx, &conversation.id, stage.as_ref())?.is_none(),
                "This conversation or stage already has an active goal"
            );
            ensure!(
                super::instructions::parked_owner(&tx, &conversation.id, &conversation.context_id)?.is_none(),
                "This context already has an owning continuation"
            );
            if let Some(draft) = submitted_draft {
                consume_matching_draft(&tx, &conversation.id, &draft)?;
            }
            let request = JobRequest {
                checkout_id: Some(conversation.checkout_id),
                task_id,
                mode: scope.mode,
                instructions: definition.instructions(),
                dependencies: Vec::new(),
                requires_bridge: true,
            };
            let origin = if stage.is_some() { JobOrigin::Pipeline } else { JobOrigin::Operator };
            let job = insert_job(
                &tx,
                project,
                Some(context.clone()),
                origin,
                request,
                0,
                stage.as_ref(),
            )?;
            if stage.is_some() {
                bind_pipeline_context(&tx, &job.id, &context.context_id, QuestionRoute::Native)?;
            }
            tx.execute(
                "UPDATE jobs SET operator_scope = ?1 WHERE id = ?2",
                params![protection::protect(&serde_json::to_vec(&scope)?)?, job.id],
            )?;
            let goal_id = match &stage {
                Some(stage) => tx.query_row(
                    "INSERT INTO goals(id, project_id, conversation_id, pipeline_run_id, node_instance_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(pipeline_run_id, node_instance_id) WHERE pipeline_run_id IS NOT NULL
                     DO UPDATE SET conversation_id = excluded.conversation_id RETURNING id",
                    params![Uuid::new_v4().to_string(), job.project_id, conversation.id, stage.pipeline_run_id, stage.node_instance_id],
                    |row| row.get::<_, String>(0),
                )?,
                None => tx.query_row(
                    "INSERT INTO goals(id, project_id, conversation_id) VALUES (?1, ?2, ?3)
                     ON CONFLICT(conversation_id) WHERE pipeline_run_id IS NULL
                     DO UPDATE SET id = goals.id RETURNING id",
                    params![Uuid::new_v4().to_string(), job.project_id, conversation.id],
                    |row| row.get::<_, String>(0),
                )?,
            };
            let id = Uuid::new_v4().to_string();
            let started = now_ms();
            let deadline = definition.max_seconds.map(|seconds| started + i64::from(seconds) * 1000);
            tx.execute(
                "INSERT INTO goal_revisions(id, goal_id, initial_job_id, definition, continuation_owner,
                    created_ms, deadline_ms, token_budget_requested)
                 VALUES (?1, ?2, ?3, ?4, 'managed', ?5, ?6, ?7)",
                params![id, goal_id, job.id, protection::protect(&serde_json::to_vec(&definition)?)?,
                    started, deadline, definition.token_budget.is_some()],
            )?;
            tx.execute("INSERT INTO active_goals VALUES (?1, ?2)", params![goal_id, id])?;
            tx.execute("INSERT INTO goal_revision_controls(revision_id, state) VALUES (?1, 'active')", [&id])?;
            tx.execute("INSERT INTO goal_continuations VALUES (?1, 0, ?2)", params![id, job.id])?;
            tx.commit()?;
            Ok(GoalRevision {
                goal_id, id, definition, continuation_owner: GoalContinuationOwner::Managed,
                control_state: GoalControlState::Active,
                provider: scope.provider, model: scope.model,
                job, checkpoint: None, provider_session_id: None, provider_goal_id: None,
            })
        }).await?;
        self.job_signal.send_replace(());
        Ok(goal)
    }

    pub async fn active_conversation_goal(
        &self,
        conversation: String,
    ) -> Result<Option<GoalRevision>> {
        self.execute(move |db| {
            active_revision(db, &conversation, None)?
                .map(|id| load_revision(db, id))
                .transpose()
        })
        .await
    }

    pub async fn active_pipeline_goal(
        &self,
        project: String,
        pipeline_run: String,
        node_instance: String,
    ) -> Result<Option<GoalRevision>> {
        self.execute(move |db| {
            let id = db
                .query_row(
                    "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
                 WHERE g.project_id = ?1 AND g.pipeline_run_id = ?2 AND g.node_instance_id = ?3",
                    params![project, pipeline_run, node_instance],
                    |row| row.get(0),
                )
                .optional()?;
            id.map(|id| load_revision(db, id)).transpose()
        })
        .await
    }

    pub(crate) async fn record_goal_turn_report(
        &self,
        run: String,
        report: GoalTurnReport,
    ) -> Result<()> {
        ensure!(
            !report.evidence.trim().is_empty() && report.evidence.len() <= 8192,
            "Invalid goal evidence"
        );
        let payload = protection::protect(&serde_json::to_vec(&report)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM active_agent_attempts a JOIN job_attempts j ON j.run_id = a.run_id
                    JOIN goal_continuations c ON c.job_id = j.job_id
                    JOIN active_goals g ON g.revision_id = c.revision_id
                    JOIN goal_revisions r ON r.id = g.revision_id
                    WHERE a.run_id = ?1 AND r.continuation_owner = 'managed' AND j.cancel_requested = 0
                )",
                [&run], |row| row.get(0),
            )?;
            ensure!(active, "The run has no active managed goal");
            let old = tx.query_row("SELECT payload FROM goal_turn_reports WHERE run_id = ?1", [&run], |row| row.get(0)).optional()?;
            if let Some(old) = decode_report(old)? {
                ensure!(old == report, "This turn already reported a different checkpoint");
            } else {
                tx.execute("INSERT INTO goal_turn_reports VALUES (?1, ?2)", params![run, payload])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
}

fn load_revision(db: &Connection, id: String) -> Result<GoalRevision> {
    let (goal_id, project, contents, owner, profile): (String, String, Vec<u8>, String, Vec<u8>) =
        db.query_row(
            "SELECT g.id, g.project_id, r.definition, r.continuation_owner, j.operator_scope
                 FROM goal_revisions r JOIN goals g ON g.id = r.goal_id
                 JOIN jobs j ON j.id = r.initial_job_id WHERE r.id = ?1",
            [&id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let profile: SessionScope = serde_json::from_slice(&protection::unprotect(&profile)?)?;
    let job_id: String = db.query_row(
                "SELECT job_id FROM goal_continuations WHERE revision_id = ?1 ORDER BY checkpoint_ordinal DESC LIMIT 1",
                [&id], |row| row.get(0),
            )?;
    let job = super::load_job(db, &project, &job_id)?.context("The goal's job is unavailable")?;
    let (provider_session_id, provider_goal_id) = db.query_row(
                "SELECT coalesce(n.provider_session_id, s.provider_session_id), n.provider_goal_id
                 FROM goal_continuations c JOIN job_attempts j ON j.job_id = c.job_id
                 JOIN agent_attempts a ON a.run_id = j.run_id JOIN agent_sessions s ON s.id = a.agent_session_id
                 LEFT JOIN native_goal_observations n ON n.run_id = a.run_id
                 WHERE c.revision_id = ?1 ORDER BY c.checkpoint_ordinal DESC, j.ordinal DESC LIMIT 1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.unwrap_or_default();
    let checkpoint = db.query_row(
                "SELECT c.ordinal, c.run_id, c.context_id, c.next_action, r.payload FROM goal_checkpoints c
                 LEFT JOIN goal_turn_reports r ON r.run_id = c.run_id
                 WHERE c.revision_id = ?1 ORDER BY c.ordinal DESC LIMIT 1",
                [&id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Option<Vec<u8>>>(4)?)),
            ).optional()?.map(|(ordinal, run_id, context_id, next_action, report)| -> Result<_> {
                Ok(GoalCheckpoint { ordinal, run_id, context_id, next_action, report: decode_report(report)? })
            }).transpose()?;
    let control_state: String = db.query_row(
        "SELECT state FROM goal_revision_controls WHERE revision_id = ?1",
        [&id],
        |row| row.get(0),
    )?;
    Ok(GoalRevision {
        goal_id,
        id,
        job,
        checkpoint,
        provider_session_id,
        provider_goal_id,
        provider: profile.provider,
        model: profile.model,
        definition: serde_json::from_slice(&protection::unprotect(&contents)?)?,
        continuation_owner: serde_json::from_value(serde_json::Value::String(owner))?,
        control_state: GoalControlState::from_key(&control_state)?,
    })
}

pub(super) fn limit_blocker(
    db: &Connection,
    job: &str,
    native_candidate: bool,
) -> Result<Option<&'static str>> {
    let limits: Option<(Option<i64>, bool, String)> = db
        .query_row(
            "SELECT r.deadline_ms, r.continuation_owner = 'managed' AND r.token_budget_requested, controls.state
         FROM goal_continuations c JOIN goal_revisions r ON r.id = c.revision_id
         JOIN goal_revision_controls controls ON controls.revision_id = r.id
         WHERE c.job_id = ?1",
            [job],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((deadline, unsupported_budget, state)) = limits else {
        return Ok(None);
    };
    if let Some(reason) = GoalControlState::from_key(&state)?.blocker() {
        return Ok(Some(reason));
    }
    Ok(match (deadline, unsupported_budget) {
        (Some(deadline), _) if now_ms() >= deadline => Some("The goal reached its time limit"),
        (_, true) if !native_candidate => {
            Some("The managed goal route cannot enforce this token budget")
        }
        _ => None,
    })
}

pub(super) fn deactivate_cancelled_job(db: &Connection, job: &str) -> Result<()> {
    let cancelled: Option<String> = db.query_row(
        "DELETE FROM active_goals
         WHERE revision_id IN (
             SELECT c.revision_id FROM goal_continuations c
             JOIN goal_revisions r ON r.id = c.revision_id
             JOIN goal_revision_controls controls ON controls.revision_id = c.revision_id
             WHERE c.job_id = ?1 AND r.continuation_owner = 'managed'
               AND controls.state IN ('active','paused')
               AND NOT EXISTS (SELECT 1 FROM goal_continuations next
                               WHERE next.revision_id = c.revision_id AND next.checkpoint_ordinal > c.checkpoint_ordinal)
               AND NOT EXISTS (
                   SELECT 1 FROM goal_continuations member JOIN job_attempts a ON a.job_id = member.job_id
                   WHERE member.revision_id = c.revision_id AND (
                       EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
                       OR EXISTS (SELECT 1 FROM job_writer_reservations WHERE run_id = a.run_id)
                       OR EXISTS (SELECT 1 FROM unconfirmed_question_stops WHERE run_id = a.run_id)
                   )
               )
         ) RETURNING revision_id",
        [job],
        |row| row.get(0),
    ).optional()?;
    if let Some(revision) = cancelled {
        db.execute(
            "UPDATE goal_revision_controls SET state = 'cancelled', stop_action = 'cancel', confirmed_by = 'recorded_idle',
                    requested_ms = ?2, settled_ms = ?2 WHERE revision_id = ?1",
            params![revision, now_ms()],
        )?;
    }
    Ok(())
}

pub(crate) struct GoalExecution {
    deadline_ms: Option<i64>,
}

impl GoalExecution {
    pub(crate) fn time_limit_reached(&self) -> bool {
        self.deadline_ms
            .is_some_and(|deadline| now_ms() >= deadline)
    }
}

impl Store {
    pub(crate) async fn goal_execution_for_run(
        &self,
        run: String,
    ) -> Result<Option<GoalExecution>> {
        self.execute(move |db| {
            Ok(db.query_row(
                "SELECT r.deadline_ms FROM goal_continuations c JOIN goal_revisions r ON r.id = c.revision_id
                 JOIN job_attempts a ON a.job_id = c.job_id WHERE a.run_id = ?1",
                [&run], |row| Ok(GoalExecution { deadline_ms: row.get(0)? }),
            ).optional()?)
        }).await
    }

    pub(crate) async fn activate_native_goal_executor(
        &self,
        run: String,
        evidence: String,
    ) -> Result<bool> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let execution: Option<(String, String)> = tx.query_row(
                "SELECT r.id, s.provider_session_id FROM active_agent_attempts active
                 JOIN job_attempts j ON j.run_id = active.run_id
                 JOIN goal_revisions r ON r.initial_job_id = j.job_id
                 JOIN active_goals current ON current.revision_id = r.id
                 JOIN goal_revision_controls controls ON controls.revision_id = r.id
                 JOIN agent_attempts a ON a.run_id = active.run_id
                 JOIN agent_sessions s ON s.id = a.agent_session_id
                 WHERE active.run_id = ?1 AND r.continuation_owner = 'managed'
                   AND controls.state = 'active' AND NOT j.cancel_requested
                   AND (r.deadline_ms IS NULL OR r.deadline_ms > ?2)
                   AND EXISTS (SELECT 1 FROM native_goal_context_receipts c
                     WHERE c.run_id = ?1 AND c.provider_session_id = s.provider_session_id)
                   AND EXISTS (SELECT 1 FROM native_goal_qualifications q WHERE q.evidence_id = ?3)",
                params![run, now_ms(), evidence], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((revision, session)) = execution else {
                return Ok(false);
            };
            if !super::validate_attempt_policy(&tx, &run)? {
                tx.commit()?;
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO native_goal_executors(run_id, revision_id, provider_session_id, evidence_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![run, revision, session, evidence],
            )?;
            tx.execute("UPDATE goal_revisions SET continuation_owner = 'native' WHERE id = ?1", [&revision])?;
            tx.commit()?;
            Ok(true)
        }).await
    }

    pub(crate) async fn record_native_goal_result(
        &self,
        run: String,
        result: &'static str,
    ) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute(
                "UPDATE native_goal_executors SET provider_result = ?2 WHERE run_id = ?1 AND provider_result IS NULL",
                params![run, result],
            )?;
            ensure!(changed == 1, "The native executor has no current result boundary");
            Ok(())
        }).await
    }
}

pub(crate) fn settle_native_goal_executor(
    db: &Connection,
    run: &str,
    confirmed: bool,
) -> Result<()> {
    if confirmed {
        db.execute(
            "UPDATE native_goal_executors SET stop_confirmed = 1 WHERE run_id = ?1",
            [run],
        )?;
    }
    Ok(())
}

pub(crate) fn record_native_goal(
    db: &Connection,
    run: &str,
    event_seq: i64,
    observed: &NativeGoalObservation,
) -> Result<()> {
    db.execute(
        "INSERT INTO native_goal_observations(run_id, event_seq, provider_goal_id, provider_session_id)
         SELECT j.run_id, ?2, ?3, ?4 FROM job_attempts j
         JOIN goal_continuations c ON c.job_id = j.job_id JOIN goal_revisions r ON r.id = c.revision_id
         JOIN agent_attempts a ON a.run_id = j.run_id JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE j.run_id = ?1 AND r.continuation_owner = 'native' AND s.provider_session_id = ?4
         ON CONFLICT(run_id) DO UPDATE SET event_seq = excluded.event_seq,
           provider_goal_id = coalesce(excluded.provider_goal_id, native_goal_observations.provider_goal_id)",
        params![run, event_seq, observed.provider_goal_id, observed.provider_session_id],
    )?;
    Ok(())
}

pub(crate) fn is_native_goal_run(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM job_attempts j
           JOIN goal_continuations c ON c.job_id = j.job_id
           JOIN goal_revisions r ON r.id = c.revision_id
           WHERE j.run_id = ?1 AND r.continuation_owner = 'native')",
        [run],
        |row| row.get(0),
    )?)
}

pub(crate) fn record_native_goal_turn(
    db: &Connection,
    run: &str,
    seq: i64,
    event: &ProviderEvent,
) -> Result<()> {
    if matches!(&event.data, ProviderEventData::GoalObserved { .. }) {
        // Goal notifications can refer to a turn that has already ended.
        return Ok(());
    }
    let turn = event.location.turn_id.as_deref();
    match &event.data {
        ProviderEventData::TurnStarted => {
            let turn = turn.context("Native goal turn has no turn ID")?;
            ensure!(
                event.location.session_id.is_some(),
                "Native goal turn has no session ID"
            );
            let inserted = db.execute(
                "INSERT INTO native_goal_turns(run_id, provider_turn_id, started_event_seq)
                 VALUES (?1, ?2, ?3) ON CONFLICT(run_id, provider_turn_id) DO NOTHING",
                params![run, turn, seq],
            )?;
            if inserted == 1 {
                db.execute(
                    "UPDATE agent_attempts SET provider_turn_id = ?2, provider_terminal = NULL
                     WHERE run_id = ?1",
                    params![run, turn],
                )?;
            }
        }
        ProviderEventData::TurnFinished { outcome } => {
            let turn = turn.context("Native goal turn has no turn ID")?;
            ensure!(
                event.location.session_id.is_some(),
                "Native goal turn has no session ID"
            );
            let state = match outcome {
                TurnOutcome::Completed => "completed",
                TurnOutcome::Failed => "failed",
                TurnOutcome::Interrupted => "interrupted",
            };
            let changed = db.execute(
                "UPDATE native_goal_turns SET finished_event_seq = ?3, outcome = ?4
                 WHERE run_id = ?1 AND provider_turn_id = ?2 AND outcome IS NULL",
                params![run, turn, seq, state],
            )?;
            if changed == 1 {
                db.execute(
                    "UPDATE agent_attempts SET provider_terminal = ?3
                     WHERE run_id = ?1 AND provider_turn_id = ?2",
                    params![run, turn, state],
                )?;
            } else {
                let known: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM native_goal_turns WHERE run_id = ?1 AND provider_turn_id = ?2)",
                    params![run, turn],
                    |row| row.get(0),
                )?;
                ensure!(known, "Native goal terminal event has no observed start");
            }
        }
        _ if let Some(turn) = turn => {
            let open: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM native_goal_turns WHERE run_id = ?1 AND provider_turn_id = ?2 AND outcome IS NULL)",
                params![run, turn],
                |row| row.get(0),
            )?;
            ensure!(
                open,
                "Provider event does not match the active native goal turn"
            );
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn accept_goal_checkpoint(db: &Connection, run: &str, context: &str) -> Result<()> {
    let source = db.query_row(
        "SELECT r.id, c.checkpoint_ordinal + 1, r.definition, r.deadline_ms, j.job_id,
                g.project_id, a.conversation_id, report.payload, r.continuation_owner,
                (SELECT provider_result FROM native_goal_executors WHERE run_id = j.run_id AND stop_confirmed)
         FROM job_attempts j JOIN goal_continuations c ON c.job_id = j.job_id
         JOIN goal_revisions r ON r.id = c.revision_id JOIN active_goals active ON active.revision_id = r.id
         JOIN goals g ON g.id = r.goal_id JOIN agent_attempts a ON a.run_id = j.run_id
         LEFT JOIN goal_turn_reports report ON report.run_id = j.run_id
         WHERE j.run_id = ?1 AND j.cancel_requested = 0 AND a.provider_terminal = 'completed'
           AND EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                       WHERE s.run_id = ?1 AND d.ordinal = 1 AND d.delivery = 'submitted')",
        [run], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Option<i64>>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            row.get::<_, String>(6)?, row.get::<_, Option<Vec<u8>>>(7)?, row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?)),
    ).optional()?;
    let Some((
        revision,
        ordinal,
        definition,
        deadline,
        source_job,
        project,
        conversation,
        report,
        owner,
        native_result,
    )) = source
    else {
        return Ok(());
    };
    let definition: GoalDefinition = serde_json::from_slice(&protection::unprotect(&definition)?)?;
    let report = decode_report(report)?;
    let mut next = if owner == "native" {
        match native_result.as_deref() {
            Some("blocked") => "blocked",
            Some("limit_reached") => "limit_reached",
            Some("completed") => "review",
            _ => return Ok(()),
        }
    } else {
        match report.as_ref().map(|report| report.action) {
            Some(GoalTurnAction::Blocked) => "blocked",
            Some(GoalTurnAction::Continue) if owner == "managed" => {
                if definition.max_turns.is_some_and(|max| ordinal >= max)
                    || deadline.is_some_and(|deadline| now_ms() >= deadline)
                    || definition.token_budget.is_some()
                {
                    "limit_reached"
                } else {
                    "continue"
                }
            }
            _ => "review",
        }
    };
    let mut continuation = None;
    if next == "continue" {
        let prior =
            super::load_job(db, &project, &source_job)?.context("The goal job is unavailable")?;
        let permitted_scope = if let Some(expected) = &prior.worktree_owner_task_id {
            let task = prior
                .task_id
                .as_deref()
                .context("The goal writer has no task")?;
            writer_owner(db, &project, task, &prior.checkout_id)?.as_ref() == Some(expected)
        } else {
            true
        };
        if permitted_scope {
            continuation = Some(prior);
        } else {
            next = "blocked";
        }
    }
    let inserted = db.execute(
        "INSERT INTO goal_checkpoints(revision_id, ordinal, run_id, context_id, next_action)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
        params![revision, ordinal, run, context, next],
    )?;
    if inserted == 1
        && let Some(prior) = continuation
    {
        let request = JobRequest {
            checkout_id: Some(prior.checkout_id),
            task_id: prior.task_id,
            mode: prior.mode,
            instructions: definition.instructions(),
            dependencies: Vec::new(),
            requires_bridge: true,
        };
        let stage = prior.pipeline.as_ref().map(|origin| origin.binding());
        let target = insert_job(
            db,
            project,
            Some(JobContext {
                conversation_id: conversation,
                context_id: context.into(),
            }),
            prior.origin,
            request,
            prior.priority,
            stage.as_ref(),
        )?;
        db.execute(
            "UPDATE jobs SET operator_scope = (SELECT operator_scope FROM jobs WHERE id = ?1) WHERE id = ?2",
            params![source_job, target.id],
        )?;
        if load_binding(db, &source_job)?.is_some() {
            db.execute(
                "INSERT INTO job_history_bindings(job_id, context_id, pipeline_pin, structured_questions)
                 SELECT ?1, ?2, pipeline_pin, structured_questions FROM job_history_bindings WHERE job_id = ?3",
                params![target.id, context, source_job],
            )?;
        }
        db.execute(
            "INSERT INTO goal_continuations VALUES (?1, ?2, ?3)",
            params![revision, ordinal, target.id],
        )?;
    }
    Ok(())
}

fn decode_report(payload: Option<Vec<u8>>) -> Result<Option<GoalTurnReport>> {
    payload
        .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
        .transpose()
}

pub(super) struct GoalBoundary {
    pub run: String,
    pub conversation: String,
    pub context: String,
}

pub(super) fn managed_boundary(db: &Connection, job: &str) -> Result<Option<GoalBoundary>> {
    Ok(db
        .query_row(
            "SELECT checkpoint.run_id, a.conversation_id, checkpoint.context_id
         FROM goal_continuations c JOIN active_goals g ON g.revision_id = c.revision_id
         JOIN goal_checkpoints checkpoint ON checkpoint.revision_id = c.revision_id
            AND checkpoint.ordinal = c.checkpoint_ordinal
         JOIN agent_attempts a ON a.run_id = checkpoint.run_id
         WHERE c.job_id = ?1 AND checkpoint.next_action = 'continue'
            AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = c.job_id)",
            [job],
            |row| {
                Ok(GoalBoundary {
                    run: row.get(0)?,
                    conversation: row.get(1)?,
                    context: row.get(2)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn queued_boundary_owner(
    db: &Connection,
    conversation: &str,
    context: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT checkpoint.run_id
         FROM goal_continuations c JOIN active_goals g ON g.revision_id = c.revision_id
         JOIN goal_revisions revision ON revision.id = c.revision_id
         JOIN goal_checkpoints checkpoint ON checkpoint.revision_id = c.revision_id
            AND checkpoint.ordinal = c.checkpoint_ordinal
         JOIN jobs j ON j.id = c.job_id
         WHERE j.conversation_id = ?1 AND j.context_id = ?2 AND j.queue_state <> 'cancelled'
           AND (revision.deadline_ms IS NULL OR revision.deadline_ms > ?3)
           AND checkpoint.next_action = 'continue'
           AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = c.job_id)
         ORDER BY j.sequence LIMIT 1",
            params![conversation, context, now_ms()],
            |row| row.get(0),
        )
        .optional()?)
}

fn active_revision(
    db: &Connection,
    conversation: &str,
    stage: Option<&PipelineDispatch>,
) -> Result<Option<String>> {
    if let Some(stage) = stage {
        return Ok(db
            .query_row(
                "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
             WHERE g.pipeline_run_id = ?1 AND g.node_instance_id = ?2",
                params![stage.pipeline_run_id, stage.node_instance_id],
                |row| row.get(0),
            )
            .optional()?);
    }
    Ok(db
        .query_row(
            "SELECT a.revision_id FROM active_goals a JOIN goals g ON g.id = a.goal_id
         WHERE g.conversation_id = ?1 AND g.pipeline_run_id IS NULL",
            [conversation],
            |row| row.get(0),
        )
        .optional()?)
}
