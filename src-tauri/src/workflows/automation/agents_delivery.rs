use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    jobs::{PipelineDispatch, QueueState, load_job},
    protection,
    repository::{
        AttemptWorktreeEvidence,
        verification::{
            CandidateSelection, CandidateTarget, SelectedCandidate, load_selected_candidate,
            require_candidate_review_dispatch, require_current,
        },
    },
    runs::RunStatus,
    store::Store,
};

use super::{
    super::{
        ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs, PinnedNode, WorkflowArtifact,
        WorkflowJobInput, WorkflowRun,
        artifacts::{insert_scoped_artifact, load_artifact},
        control::{self, ControlStageState},
        digest,
        generic_agent::{AgentStageOutcome, GoalResult, goal_result_run, load_response, stage_job},
        inspection::record_workflow_input_ids,
        job_inputs::load_attempt_input,
        runtime::{load_node_attempt, load_pinned_node, load_workflow_run},
        stage_goal::{StageGoalPin, require_stage_goal_result},
        units::{WorkflowDeliveryUnit, load_delivery_unit},
    },
    AutomaticAgentWait, enabled,
};

mod report;
#[cfg(test)]
mod tests;

pub(in crate::workflows) fn instructions(kind: BuiltinNode) -> String {
    match kind {
        BuiltinNode::Implement => {
            "Implement the pinned logical unit on the approved branch. Keep unrelated changes out of the unit. Leave the changes in the worktree for review, checks and commit approval."
        }
        BuiltinNode::ReviewCandidate => {
            "Review the selected candidate tree for correctness and evaluate the pinned unit criteria."
        }
        BuiltinNode::ReviewDiffCandidate => {
            "Review the frozen Git diff and report findings with evidence from the selected comparison."
        }
        _ => "",
    }.into()
}

pub(in crate::workflows) const REVIEW_FORMAT: &str = report::FORMAT;

pub(in crate::workflows) fn enabled_for(
    db: &Connection,
    run: &str,
    node: &PinnedNode,
) -> Result<bool> {
    Ok(matches!(
        node.definition.kind,
        BuiltinNode::Implement | BuiltinNode::ReviewCandidate
    ) && has_output(node, "failed")
        && (super::prompt_delegate(node) || enabled(db, run)?))
}

pub(crate) fn require_unique_admission(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    input: &WorkflowJobInput,
) -> Result<()> {
    let node = load_pinned_node(
        db,
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
    )?
    .context("The delivery agent stage is unavailable")?;
    if !super::prompt_delegate(&node) && !enabled(db, &binding.pipeline_run_id)? {
        return Ok(());
    }
    if matches!(
        node.definition.kind,
        BuiltinNode::Implement | BuiltinNode::ReviewCandidate
    ) && has_output(&node, "failed")
        || super::prompt_native_stage(&node)
    {
        ensure!(
            stage_job(
                db,
                project,
                &binding.pipeline_run_id,
                &binding.node_instance_id,
                input.unit_id.as_deref(),
                &input.artifacts
            )?
            .is_none(),
            "This delivery stage already has a job for the selected inputs"
        );
    }
    Ok(())
}

pub(in crate::workflows) fn evaluate(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<Option<AgentStageOutcome>> {
    let (Some(unit), Some(plan)) = (unit, run.plan_revision.as_deref()) else {
        return Ok(None);
    };
    if !enabled_for(db, &run.id, node)? {
        return Ok(None);
    }
    let node_id = &node.instance.id;
    let inputs: NodeInputs = inputs
        .iter()
        .map(|(port, artifact)| (port.clone(), artifact.id.clone()))
        .collect();
    let Some(job_id) = stage_job(db, &run.project_id, &run.id, node_id, Some(unit), &inputs)?
    else {
        return Ok(None);
    };
    let job =
        load_job(db, &run.project_id, &job_id)?.context("The delivery agent job is unavailable")?;
    let cancelled = job.queue_state == QueueState::Cancelled;
    let fail = |attempt, reason: &str| AgentStageOutcome {
        state: ControlStageState::Failed,
        port: "failed",
        reason: Some(reason.to_owned()),
        source_run_id: attempt,
    };
    let Some(attempt) = job.latest_attempt else {
        return Ok(cancelled.then(|| fail(None, "The delivery agent job was cancelled")));
    };
    let (result_job, attempt) = if StageGoalPin::for_stage(node)?.is_some() {
        match goal_result_run(db, &run.project_id, &job_id)? {
            GoalResult::Waiting if !cancelled => return Ok(None),
            GoalResult::Waiting | GoalResult::Failed => {
                return Ok(Some(fail(
                    Some(attempt.run_id),
                    "The delivery agent Goal ended without a verified result",
                )));
            }
            GoalResult::Ready { job_id, run_id } => (job_id, run_id),
        }
    } else {
        if !attempt.status.is_terminal() || job.queued_recovery.is_some() {
            return Ok(None);
        }
        if !cancelled
            && crate::workflows::generic_agent::attempt_has_open_wait(db, &attempt.run_id)?
        {
            return Ok(None);
        }
        if attempt.status != RunStatus::Succeeded {
            return Ok(Some(fail(
                Some(attempt.run_id),
                "The delivery agent job did not succeed",
            )));
        }
        (job_id, attempt.run_id)
    };
    let key = request_key(node, unit, &inputs)?;
    let existing: Option<(String, Option<String>)> = db.query_row(
        "SELECT state, diagnostic FROM workflow_automatic_delivery_results
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3 AND attempt_id = ?4",
        params![run.id, node_id, key, attempt], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    match existing.as_ref().map(|(state, _)| state.as_str()) {
        Some("published") => {
            return Ok(Some(AgentStageOutcome {
                state: ControlStageState::Completed,
                port: if node.definition.kind == BuiltinNode::Implement {
                    "implemented"
                } else {
                    "reviewed"
                },
                reason: None,
                source_run_id: Some(attempt),
            }));
        }
        Some("failed") => {
            return Ok(Some(fail(
                Some(attempt),
                existing
                    .as_ref()
                    .and_then(|(_, reason)| reason.as_deref())
                    .unwrap_or("Delivery evidence could not be verified"),
            )));
        }
        Some(_) => return Ok(None),
        None => {}
    }
    db.execute(
        "INSERT INTO workflow_automatic_delivery_results(pipeline_run_id, node_instance_id,
            request_key, plan_key, unit_id, job_id, attempt_id, inputs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT DO NOTHING",
        params![
            run.id,
            node_id,
            key,
            plan,
            unit,
            result_job,
            attempt,
            protection::protect(&serde_json::to_vec(&inputs)?)?
        ],
    )?;
    Ok(None)
}

fn request_key(node: &PinnedNode, unit: &str, inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(
        &node.revision,
        unit,
        inputs,
    ))?))
}

fn has_output(node: &PinnedNode, name: &str) -> bool {
    node.definition.outputs.iter().any(|port| port.name == name)
}

pub(in crate::workflows) fn sealed_output(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    source_run: &str,
    port: &str,
) -> Result<Option<WorkflowArtifact>> {
    let bytes: Option<Vec<u8>> = db.query_row(
        "SELECT a.payload FROM workflow_automatic_delivery_outputs o
         JOIN workflow_automatic_delivery_results r USING (pipeline_run_id, node_instance_id, request_key, attempt_id)
         JOIN workflow_artifacts a ON a.id = o.artifact_id
         WHERE r.pipeline_run_id = ?1 AND r.node_instance_id = ?2 AND r.unit_id IS ?3
           AND r.attempt_id = ?4 AND o.port_name = ?5 AND r.plan_key = ?6 AND r.state = 'published'",
        params![run.id, node, unit, source_run, port, run.plan_revision], |row| row.get(0),
    ).optional()?;
    bytes
        .map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?))
        .transpose()
}

pub(in crate::workflows) fn waits(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<AutomaticAgentWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, unit_id, coalesce(diagnostic, 'Verifying delivery agent evidence')
         FROM workflow_automatic_delivery_results WHERE pipeline_run_id = ?1 AND plan_key = ?2
           AND state = 'pending' ORDER BY node_instance_id, unit_id",
    )?;
    Ok(query
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(AutomaticAgentWait {
                node_id: row.get(0)?,
                unit_id: Some(row.get(1)?),
                reason: Some(row.get(2)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) struct PendingDeliveryResult {
    project: String,
    run: String,
    node: String,
    unit: String,
    plan: String,
    key: String,
    job: String,
    attempt: String,
    inputs: NodeInputs,
}

impl Store {
    pub(crate) async fn next_workflow_delivery_result(
        &self,
    ) -> Result<Option<PendingDeliveryResult>> {
        self.execute(|db| {
            reconcile_terminal_jobs(db)?;
            let pending = db
                .query_row(
                    "SELECT r.project_id, a.pipeline_run_id, a.node_instance_id, a.unit_id,
                        a.plan_key, a.request_key, a.job_id, a.attempt_id, a.inputs
                 FROM workflow_automatic_delivery_results a JOIN job_pipeline_runs r
                   ON r.id = a.pipeline_run_id AND r.plan_revision = a.plan_key
                 WHERE a.state = 'pending' AND a.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                   AND a.attempt_id = (SELECT run_id FROM job_attempts
                       WHERE job_id = a.job_id ORDER BY ordinal DESC LIMIT 1)
                   AND NOT EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = a.job_id)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id, a.unit_id LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get::<_, Vec<u8>>(8)?,
                        ))
                    },
                )
                .optional()?;
            pending
                .map(
                    |(project, run, node, unit, plan, key, job, attempt, payload)| {
                        Ok(PendingDeliveryResult {
                            project,
                            run,
                            node,
                            unit,
                            plan,
                            key,
                            job,
                            attempt,
                            inputs: serde_json::from_slice(&protection::unprotect(&payload)?)?,
                        })
                    },
                )
                .transpose()
        })
        .await
    }
}

fn reconcile_terminal_jobs(db: &mut Connection) -> Result<()> {
    // The job can settle immediately before shutdown, or be cancelled before its first attempt.
    let possible = db.prepare(
        "SELECT DISTINCT o.project_id, o.pipeline_run_id, o.node_instance_id
         FROM job_pipeline_runs w
         JOIN job_pipeline_origins o ON o.pipeline_run_id = w.id AND o.plan_revision = w.plan_revision
         JOIN jobs j ON j.id = o.job_id
         JOIN workflow_job_inputs i ON i.job_id = j.id AND i.unit_id IS NOT NULL
         JOIN workflow_node_pins n ON n.pipeline_run_id = o.pipeline_run_id
           AND n.node_instance_id = o.node_instance_id
           AND n.definition_revision >= 2 AND n.scheduler_profile IN ('approved_write', 'read_only')
         LEFT JOIN job_attempts a ON a.run_id = (
           SELECT latest.run_id FROM job_attempts latest WHERE latest.job_id = j.id ORDER BY ordinal DESC LIMIT 1)
         LEFT JOIN runs r ON r.id = a.run_id
         WHERE NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
           AND (EXISTS (SELECT 1 FROM workflow_automatic_runs auto WHERE auto.pipeline_run_id = w.id)
                OR EXISTS (SELECT 1 FROM workflow_automatic_agents queued
                   WHERE queued.pipeline_run_id = w.id AND queued.node_instance_id = o.node_instance_id
                     AND queued.plan_key = w.plan_revision))
           AND (j.queue_state = 'cancelled' OR r.status IN ('succeeded', 'failed', 'cancelled', 'interrupted'))
           AND NOT EXISTS (SELECT 1 FROM job_recoveries recovery WHERE recovery.job_id = j.id)
           AND NOT EXISTS (SELECT 1 FROM workflow_control_receipts c WHERE c.pipeline_run_id = w.id
             AND c.plan_key = w.plan_revision AND c.unit_key = i.unit_id AND c.node_instance_id = o.node_instance_id)
           AND NOT EXISTS (SELECT 1 FROM workflow_automatic_delivery_results d WHERE d.pipeline_run_id = w.id
             AND d.node_instance_id = o.node_instance_id AND d.attempt_id = a.run_id)",
    )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if possible.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut refreshed = std::collections::BTreeSet::new();
    for (project, workflow, node) in possible {
        if let Some(node) = load_pinned_node(&tx, &project, &workflow, &node)?
            && enabled_for(&tx, &workflow, &node)?
            && refreshed.insert((project.clone(), workflow.clone()))
        {
            control::refresh(&tx, &project, &workflow)?;
        }
    }
    tx.commit()?;
    Ok(())
}

struct DeliveryScope {
    node: PinnedNode,
    unit: WorkflowDeliveryUnit,
}

impl PendingDeliveryResult {
    fn validate(&self, db: &Connection) -> Result<DeliveryScope> {
        let run = load_workflow_run(db, &self.project, &self.run)?
            .context("The delivery workflow is unavailable")?;
        ensure!(
            run.plan_revision.as_ref() == Some(&self.plan),
            "The delivery plan was superseded"
        );
        control::require_stage(
            db,
            &self.project,
            &self.run,
            &self.node,
            Some(&self.unit),
            &self.inputs,
        )?;
        let node = load_pinned_node(db, &self.project, &self.run, &self.node)?
            .context("The delivery agent node is unavailable")?;
        ensure!(
            enabled_for(db, &self.run, &node)?
                && request_key(&node, &self.unit, &self.inputs)? == self.key,
            "The delivery agent pins changed"
        );
        let attempt = load_node_attempt(db, &self.project, &self.attempt)?
            .context("The delivery agent attempt is unavailable")?;
        let inputs = load_attempt_input(db, &self.attempt)?
            .context("The delivery agent lost its selected inputs")?;
        ensure!(
            attempt.status == RunStatus::Succeeded
                && attempt.job_id == self.job
                && attempt.pin.workflow_run_id == self.run
                && attempt.pin.node_instance_id == self.node
                && attempt.pin.plan_revision.as_ref() == Some(&self.plan)
                && attempt.pin.unit_id.as_ref() == Some(&self.unit)
                && inputs.artifacts == self.inputs,
            "The delivery result belongs to a different successful attempt"
        );
        let job = load_job(db, &self.project, &self.job)?
            .context("The delivery agent job is unavailable")?;
        ensure!(
            job.queued_recovery.is_none()
                && job
                    .latest_attempt
                    .as_ref()
                    .is_some_and(|latest| latest.run_id == self.attempt),
            "The delivery agent result was superseded by recovery"
        );
        let task = job
            .task_id
            .as_deref()
            .context("The delivery agent lost its assigned task")?;
        let unit = load_delivery_unit(db, &self.project, task, &self.unit)?
            .context("The delivery agent is outside the active unit")?;
        ensure!(
            unit.workflow_run_id == self.run
                && unit.plan_revision == self.plan
                && unit.checkout_id == job.checkout_id
                && unit.commit_evidence_id.is_none(),
            "The work scope changed before publication"
        );
        if node.definition.kind == BuiltinNode::Implement {
            job.require_workflow_writer_release(db)?;
        }
        Ok(DeliveryScope { node, unit })
    }

    async fn select_candidate(
        &self,
        store: &Store,
        unit: &WorkflowDeliveryUnit,
    ) -> Result<SelectedCandidate> {
        let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
            .attempt_worktree_evidence(self.project.clone(), self.attempt.clone())
            .await?
        else {
            anyhow::bail!("The implementation attempt has no saved candidate capture");
        };
        let target = CandidateTarget {
            project_id: self.project.clone(),
            task_id: unit.task_id.clone(),
            unit_id: self.unit.clone(),
            run_id: self.attempt.clone(),
            capture_revision: artifact.revision.clone(),
        };
        let existing = store
            .current_selected_candidate(
                self.project.clone(),
                unit.task_id.clone(),
                self.unit.clone(),
            )
            .await?;
        let previous_id = existing.as_ref().map(|candidate| candidate.id.clone());
        if let Some(existing) = existing.filter(|candidate| candidate.target == target) {
            return Ok(existing);
        }
        ensure!(
            artifact.start.starting.head != artifact.candidate.head
                || !artifact.candidate.status.is_empty(),
            "The implementation attempt produced no captured changes"
        );
        let changes = artifact
            .candidate
            .changes
            .as_ref()
            .context("The captured change content is unavailable")?;
        ensure!(
            changes
                .untracked
                .iter()
                .all(|file| file.content_base64.is_some()),
            "The capture omitted the contents of a new file; select the reviewed paths manually"
        );
        store
            .select_workflow_candidate(
                target,
                CandidateSelection {
                    tracked_paths: None,
                    untracked_paths: changes
                        .untracked
                        .iter()
                        .map(|file| file.path.clone())
                        .collect(),
                },
                previous_id,
            )
            .await
    }

    pub(crate) async fn publish(self, store: &Store) -> Result<()> {
        let pending = std::sync::Arc::new(self);
        let preparation = store
            .execute({
                let pending = pending.clone();
                move |db| pending.validate(db)
            })
            .await;
        let selection = match preparation {
            Ok(scope) if scope.node.definition.kind == BuiltinNode::Implement => {
                pending.select_candidate(store, &scope.unit).await.map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) => return pending.defer(store, error).await,
        };
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_automatic_delivery_results
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3
                   AND attempt_id = ?4 AND state = 'pending')",
                params![pending.run, pending.node, pending.key, pending.attempt], |row| row.get(0),
            )?;
            if !current { return Ok(()); }
            let scope = match pending.validate(&tx) {
                Ok(scope) => scope,
                Err(error) => {
                    pending.set_diagnostic(&tx, &error, false)?;
                    tx.commit()?;
                    return Ok(());
                }
            };
            let values = selection.and_then(|candidate| pending.outputs(&tx, &scope, candidate));
            match values {
                Ok(values) => {
                    for (port, value) in values {
                        let output = insert_scoped_artifact(&tx, &pending.project, pending.run.clone(),
                            pending.node.clone(), port.into(), Some(pending.unit.clone()), value)?;
                        record_workflow_input_ids(&tx, &output, &pending.plan, &pending.inputs)?;
                        tx.execute(
                            "INSERT INTO workflow_automatic_delivery_outputs
                             (pipeline_run_id, node_instance_id, request_key, attempt_id, port_name, artifact_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![pending.run, pending.node, pending.key, pending.attempt, port, output.id],
                        )?;
                    }
                    tx.execute(
                        "UPDATE workflow_automatic_delivery_results SET state = 'published', diagnostic = NULL
                         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3 AND attempt_id = ?4",
                        params![pending.run, pending.node, pending.key, pending.attempt],
                    )?;
                }
                Err(error) => pending.set_diagnostic(&tx, &error, true)?,
            }
            control::refresh(&tx, &pending.project, &pending.run)?;
            tx.commit()?;
            Ok(())
        }).await
    }

    async fn defer(self: &std::sync::Arc<Self>, store: &Store, error: anyhow::Error) -> Result<()> {
        let pending = self.clone();
        store
            .execute(move |db| pending.set_diagnostic(db, &error, false))
            .await
    }

    fn set_diagnostic(&self, db: &Connection, error: &anyhow::Error, terminal: bool) -> Result<()> {
        db.execute(
            "UPDATE workflow_automatic_delivery_results SET state = ?5, diagnostic = ?6, retry_after = unixepoch() + 30
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3 AND attempt_id = ?4
               AND state = 'pending'",
            params![self.run, self.node, self.key, self.attempt, if terminal { "failed" } else { "pending" },
                crate::diagnostics::preview(&error.to_string())],
        )?;
        Ok(())
    }

    fn outputs(
        &self,
        db: &Connection,
        scope: &DeliveryScope,
        candidate: Option<SelectedCandidate>,
    ) -> Result<Vec<(&'static str, ArtifactValue)>> {
        let is_implementation = scope.node.definition.kind == BuiltinNode::Implement;
        if is_implementation {
            let candidate = candidate.context("The implementation has no selected candidate")?;
            let current_id: String = db.query_row(
                "SELECT candidate_id FROM active_unit_candidates WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
                params![self.project, scope.unit.task_id, self.unit], |row| row.get(0),
            )?;
            let current =
                load_selected_candidate(db, &self.project, &scope.unit.task_id, &current_id)?;
            ensure!(
                current.target == candidate.target
                    && current.checkout_id == scope.unit.checkout_id
                    && current.target.run_id == self.attempt,
                "The candidate belongs to a different implementation attempt"
            );
            require_stage_goal_result(
                db,
                &self.project,
                &self.run,
                &self.node,
                &self.attempt,
                Some(&current),
            )?;
            return Ok(vec![("candidate", ArtifactValue::Candidate(current))]);
        }
        require_stage_goal_result(
            db,
            &self.project,
            &self.run,
            &self.node,
            &self.attempt,
            None,
        )?;
        ensure!(
            scope.node.definition.kind == BuiltinNode::ReviewCandidate,
            "The pinned node is not a candidate reviewer"
        );
        let candidate_id = self
            .inputs
            .get("candidate")
            .context("The review lost its pinned candidate")?;
        let artifact = load_artifact(
            db,
            &self.project,
            &self.run,
            candidate_id,
            ArtifactKind::Candidate,
        )?
        .context("The selected review candidate is unavailable")?;
        let ArtifactValue::Candidate(candidate) = artifact.value else {
            unreachable!()
        };
        ensure!(
            candidate.target.project_id == self.project
                && candidate.target.task_id == scope.unit.task_id
                && candidate.target.unit_id == self.unit
                && candidate.checkout_id == scope.unit.checkout_id,
            "The review candidate belongs to a different delivery unit"
        );
        require_current(db, &candidate)?;
        require_candidate_review_dispatch(db, &self.attempt, &candidate)?;
        let (dispatch_id, response_revision_id, response) = load_response(
            db,
            &self.project,
            &scope.unit.checkout_id,
            &self.job,
            &self.attempt,
        )?;
        let mut values = vec![(
            "review",
            ArtifactValue::Review {
                run_id: self.attempt.clone(),
            },
        )];
        if has_output(&scope.node, "report") {
            let report = report::normalize(
                &response,
                &scope.unit.unit.completion_criteria,
                report::Provenance {
                    job_id: &self.job,
                    run_id: &self.attempt,
                    dispatch_id: &dispatch_id,
                    response_revision_id: &response_revision_id,
                    candidate_id: &candidate.id,
                    tree: &candidate.tree,
                },
            )?;
            values.push(("report", ArtifactValue::JsonObject(report)));
        }
        Ok(values)
    }
}
