use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::{MessageRole, context_entries, load_messages},
    dispatch::{ContextSnapshot, NativeHistory},
    jobs::{
        GoalVerificationStatus, Job, JobRequest, PipelineDispatch, QueueState,
        load_goal_verification, load_job,
    },
    protection,
    providers::SessionMode,
    runs::RunStatus,
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, PinnedNode,
    WorkflowArtifact, WorkflowJobInput, WorkflowRun,
    artifacts::{insert_scoped_artifact, load_artifact},
    authoring::package::roles,
    comparison::{ComparisonSelection, WorkflowComparisonResults, selection::select_inputs},
    control::ControlStageState,
    inspection::record_workflow_completion_inputs,
    job_inputs::load_workflow_job_input,
    launch::load_workflow_launch,
    runtime::append_pinned_unit,
    stage_plan::StagePlanConfiguration,
    units::load_delivery_unit,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgentResponse {
    pub job_id: String,
    pub run_id: String,
    pub dispatch_id: String,
    pub response_revision_id: String,
    pub checkout_id: String,
    pub prompt_artifact_id: String,
    pub comparison_artifact_id: Option<String>,
}

pub(super) struct AgentStageOutcome {
    pub state: ControlStageState,
    pub port: &'static str,
    pub reason: Option<String>,
    pub source_run_id: Option<String>,
}

impl AgentStageOutcome {
    fn failed(run_id: Option<String>, reason: &'static str) -> Self {
        Self {
            state: ControlStageState::Failed,
            port: "failed",
            reason: Some(reason.into()),
            source_run_id: run_id,
        }
    }
}

pub(super) fn optional_input(port: &str) -> bool {
    port == "comparison"
}

pub(super) fn prompt(artifact: Option<&WorkflowArtifact>) -> Result<&str> {
    let Some(ArtifactValue::RenderedText(prompt)) = artifact.map(|artifact| &artifact.value) else {
        anyhow::bail!("The Agent stage needs a rendered prompt");
    };
    ensure!(!prompt.trim().is_empty(), "The Agent prompt is empty");
    Ok(prompt)
}

fn comparison(artifact: &WorkflowArtifact) -> Result<&WorkflowComparisonResults> {
    let ArtifactValue::ComparisonResults(results) = &artifact.value else {
        anyhow::bail!("The Agent stage requires the wired Compare models result");
    };
    Ok(results)
}

pub(super) fn selected_comparison(
    artifact: Option<&WorkflowArtifact>,
) -> Result<Option<ComparisonSelection>> {
    artifact
        .map(|artifact| {
            let selection = comparison(artifact)?.successful_selection();
            ensure!(
                !selection.run_ids.is_empty(),
                "The wired comparison has no retained responses"
            );
            Ok(selection)
        })
        .transpose()
}

fn checkout(db: &Connection, project: &str, workflow: &str) -> Result<String> {
    Ok(load_workflow_launch(db, project, workflow)?
        .context("Launch the Agent workflow in a registered checkout")?
        .checkout_id)
}

impl Store {
    pub(super) async fn generic_agent_checkout(
        &self,
        project: &str,
        workflow: &str,
    ) -> Result<String> {
        let (project, workflow) = (project.to_owned(), workflow.to_owned());
        self.execute(move |db| checkout(db, &project, &workflow))
            .await
    }
}

pub(super) fn require_admission(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    node: &PinnedNode,
    request: &JobRequest,
    input: &WorkflowJobInput,
) -> Result<()> {
    if node.definition.kind != BuiltinNode::Agent {
        return Ok(());
    }
    let workflow = &binding.pipeline_run_id;
    let unit = match input.unit_id.as_deref() {
        Some(id) => {
            let task = request
                .task_id
                .as_deref()
                .context("An Agent's logical unit needs its assigned work")?;
            let unit = load_delivery_unit(db, project, task, id)?
                .context("The Agent's logical unit is outside the active delivery")?;
            ensure!(
                unit.workflow_run_id == *workflow
                    && binding.plan_revision.as_deref() == Some(&unit.plan_revision),
                "The Agent's logical unit does not match the active plan"
            );
            Some(unit)
        }
        None => {
            ensure!(
                request.task_id.is_none() && binding.plan_revision.is_none(),
                "A standalone Agent uses the checkout pinned at launch"
            );
            None
        }
    };
    let checkout = if let Some(unit) = &unit {
        unit.checkout_id.clone()
    } else {
        checkout(db, project, workflow)?
    };
    let mode =
        StagePlanConfiguration::for_stage(node)?.map_or(SessionMode::ReadOnly, |plan| plan.mode());
    ensure!(
        request.checkout_id.as_deref() == Some(checkout.as_str())
            && request.mode == mode
            && request.dependencies.is_empty()
            && !request.requires_bridge
            && binding.release_id.is_none(),
        "The Agent must use its pinned checkout and non-writing mode"
    );
    let read = |port, kind| -> Result<Option<WorkflowArtifact>> {
        input
            .artifacts
            .get(port)
            .map(|id| load_artifact(db, project, workflow, id, kind))
            .transpose()
            .map(Option::flatten)
    };
    let prompt_artifact = read("prompt", ArtifactKind::RenderedText)?;
    let expected_prompt = prompt(prompt_artifact.as_ref())?;
    let NodeConfiguration::AgentRole { requirements, .. } = &node.instance.configuration else {
        anyhow::bail!("The Agent stage lost its pinned role");
    };
    let mut expected_instructions = if let Some(version) = &requirements.bound_role {
        let template = roles::load_role(db, project, version)?
            .context("The pinned Agent role is unavailable")?;
        roles::role_instructions(&template, expected_prompt)
    } else {
        expected_prompt.to_owned()
    };
    if let Some(unit) = &unit {
        append_pinned_unit(&mut expected_instructions, &unit.unit)?;
    }
    ensure!(
        request.instructions == expected_instructions,
        "The Agent prompt changed before admission"
    );
    let comparison_artifact = read("comparison", ArtifactKind::ComparisonResults)?;
    ensure!(
        input.comparison == selected_comparison(comparison_artifact.as_ref())?,
        "The Agent comparison differs from the wired result"
    );
    if let (Some(artifact), Some(selection)) = (comparison_artifact.as_ref(), &input.comparison) {
        let frozen = comparison(artifact)?;
        let selected = select_inputs(db, project, &checkout, selection)?;
        ensure!(
            selected
                .inputs
                .iter()
                .all(|input| frozen.candidates.iter().any(|candidate| {
                    candidate.run_id.as_deref() == Some(input.run_id.as_str())
                        && candidate.job_id == input.job_id
                        && candidate.dispatch_id.as_deref() == Some(input.dispatch_id.as_str())
                        && candidate.response_revision_id.as_deref()
                            == Some(input.response_revision_id.as_str())
                })),
            "A retained comparison response differs from the wired result"
        );
    }
    ensure!(
        stage_job(
            db,
            project,
            workflow,
            &binding.node_instance_id,
            input.unit_id.as_deref(),
            &input.artifacts
        )?
        .is_none(),
        "The Agent already has a job for these pinned inputs"
    );
    Ok(())
}

fn stage_job(
    db: &Connection,
    project: &str,
    workflow: &str,
    node: &str,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<String>> {
    let mut query = db.prepare(
        "SELECT o.job_id FROM job_pipeline_origins o
         JOIN workflow_job_inputs i ON i.job_id = o.job_id
         LEFT JOIN goal_continuations g ON g.job_id = o.job_id
         WHERE o.project_id = ?1 AND o.pipeline_run_id = ?2 AND o.node_instance_id = ?3
           AND i.unit_id IS ?4
           AND (g.checkpoint_ordinal IS NULL OR g.checkpoint_ordinal = 0)",
    )?;
    let mut rows = query.query(params![project, workflow, node, unit])?;
    while let Some(row) = rows.next()? {
        let job: String = row.get(0)?;
        if load_workflow_job_input(db, &job)?.is_some_and(|input| &input.artifacts == inputs) {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

pub(super) fn evaluate(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<Option<AgentStageOutcome>> {
    let project = &run.project_id;
    let node_id = &node.instance.id;
    let input_ids = inputs
        .iter()
        .map(|(port, artifact)| (port.clone(), artifact.id.clone()))
        .collect();
    let Some(job_id) = stage_job(db, project, &run.id, node_id, unit, &input_ids)? else {
        return Ok(None);
    };
    let job = load_job(db, project, &job_id)?.context("The Agent job is unavailable")?;
    let cancelled = job.queue_state == QueueState::Cancelled;
    let Some(attempt) = job.latest_attempt else {
        return Ok(
            cancelled.then(|| AgentStageOutcome::failed(None, "The Agent job was cancelled"))
        );
    };
    let source_run = if super::stage_goal::StageGoalPin::for_stage(node)?.is_some() {
        match goal_result_run(db, project, &job_id)? {
            GoalResult::Waiting if !cancelled => return Ok(None),
            GoalResult::Waiting | GoalResult::Failed => {
                return Ok(Some(AgentStageOutcome::failed(
                    Some(attempt.run_id),
                    "The Agent Goal ended without a verified result",
                )));
            }
            GoalResult::Ready { job_id, run_id } => {
                super::stage_goal::require_stage_goal_result(
                    db, project, &run.id, node_id, &run_id, None,
                )?;
                (job_id, run_id)
            }
        }
    } else {
        if !attempt.status.is_terminal() || job.queued_recovery.is_some() {
            return Ok(None);
        }
        let waiting: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM open_agent_questions WHERE run_id = ?1
             UNION ALL SELECT 1 FROM job_downstream_waits WHERE parent_run_id = ?1
               AND state IN ('yielding', 'waiting', 'needs_review'))",
            [&attempt.run_id],
            |row| row.get(0),
        )?;
        if waiting && !cancelled {
            return Ok(None);
        }
        if attempt.status != RunStatus::Succeeded {
            return Ok(Some(AgentStageOutcome::failed(
                Some(attempt.run_id),
                "The Agent job did not succeed",
            )));
        }
        (job_id, attempt.run_id)
    };
    let (source_job, run_id) = source_run;
    let response =
        match record_response(db, project, &job.checkout_id, &source_job, &run_id, inputs) {
            Ok(response) => response,
            Err(_) => {
                return Ok(Some(AgentStageOutcome::failed(
                    Some(run_id),
                    "The Agent returned no retained response",
                )));
            }
        };
    let artifact = insert_scoped_artifact(
        db,
        project,
        run.id.clone(),
        node_id.clone(),
        "response".into(),
        unit.map(str::to_owned),
        ArtifactValue::AgentResponse(response),
    )?;
    record_workflow_completion_inputs(
        db,
        &artifact,
        run.plan_revision.as_deref().unwrap_or(""),
        inputs,
    )?;
    Ok(Some(AgentStageOutcome {
        state: ControlStageState::Completed,
        port: "completed",
        reason: None,
        source_run_id: Some(run_id),
    }))
}

enum GoalResult {
    Waiting,
    Failed,
    Ready { job_id: String, run_id: String },
}

fn goal_result_run(db: &Connection, project: &str, initial: &str) -> Result<GoalResult> {
    let cancelled: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM goal_revisions r JOIN goal_revision_controls c
          ON c.revision_id = r.id WHERE r.initial_job_id = ?1 AND c.state = 'cancelled')",
        [initial],
        |row| row.get(0),
    )?;
    if cancelled {
        return Ok(GoalResult::Failed);
    }
    let selected: Option<(String, String, String)> = db
        .query_row(
            "SELECT c.run_id, a.job_id, c.next_action
         FROM goal_revisions r JOIN goal_checkpoints c ON c.revision_id = r.id
         JOIN job_attempts a ON a.run_id = c.run_id
         WHERE r.initial_job_id = ?1 ORDER BY c.ordinal DESC LIMIT 1",
            [initial],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((run_id, job_id, next_action)) = selected else {
        return Ok(GoalResult::Waiting);
    };
    Ok(match next_action.as_str() {
        "limit_reached" => GoalResult::Failed,
        "review" => match load_goal_verification(db, project, &run_id)?.map(|result| result.status)
        {
            Some(GoalVerificationStatus::Completed) => GoalResult::Ready { job_id, run_id },
            Some(GoalVerificationStatus::Failed) => GoalResult::Failed,
            _ => GoalResult::Waiting,
        },
        _ => GoalResult::Waiting,
    })
}

fn record_response(
    db: &Connection,
    project: &str,
    checkout: &str,
    job: &str,
    run: &str,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<WorkflowAgentResponse> {
    let (dispatch_id, response_revision_id, _) = load_response(db, project, checkout, job, run)?;
    Ok(WorkflowAgentResponse {
        job_id: job.into(),
        run_id: run.into(),
        dispatch_id,
        response_revision_id,
        checkout_id: checkout.into(),
        prompt_artifact_id: inputs
            .get("prompt")
            .context("The Agent lost its pinned prompt")?
            .id
            .clone(),
        comparison_artifact_id: inputs.get("comparison").map(|artifact| artifact.id.clone()),
    })
}

fn load_response(
    db: &Connection,
    project: &str,
    checkout: &str,
    job: &str,
    run: &str,
) -> Result<(String, String, String)> {
    let (dispatch, conversation, input, result, initial_job) = db.query_row(
        "SELECT d.id, a.conversation_id, a.context_id, a.result_context_id, coalesce(goal.initial_job_id, j.id)
         FROM agent_attempts a JOIN runs r ON r.id = a.run_id
         JOIN conversations c ON c.id = a.conversation_id
         JOIN job_attempts x ON x.run_id = r.id JOIN jobs j ON j.id = x.job_id
         JOIN workflow_job_inputs w ON w.job_id = j.id
         JOIN context_snapshots s ON s.run_id = r.id
         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
         LEFT JOIN goal_continuations g ON g.job_id = j.id
         LEFT JOIN goal_revisions goal ON goal.id = g.revision_id
         WHERE r.id = ?1 AND j.id = ?2 AND j.project_id = ?3 AND r.checkout_id = ?4
           AND c.background = 1 AND j.mode IN ('read_only', 'plan') AND r.status = 'succeeded'
           AND a.provider_terminal = 'completed' AND d.delivery = 'submitted'
           AND NOT EXISTS (SELECT 1 FROM history_deleted_dispatch_payloads x WHERE x.dispatch_id = d.id)",
        params![run, job, project, checkout],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                 row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
    ).optional()?.context("The exact Agent dispatch is missing or unavailable")?;
    require_initial_dispatch(db, &initial_job, checkout)?;
    let initial = context_entries(db, &conversation, &input)?;
    let final_context = context_entries(db, &conversation, &result)?;
    let output = final_context
        .strip_prefix(initial.as_slice())
        .filter(|output| output.len() == 1)
        .context("The Agent did not seal one final response")?;
    let message = load_messages(db, &conversation, output)?
        .pop()
        .context("The Agent response is missing")?;
    ensure!(
        message.role == MessageRole::Assistant && !message.text.trim().is_empty(),
        "The Agent response is empty"
    );
    Ok((dispatch, message.revision_id, message.text))
}

fn require_initial_dispatch(db: &Connection, job: &str, checkout: &str) -> Result<()> {
    let (request, snapshot, available) = db.query_row(
        "SELECT j.request, s.payload, NOT EXISTS (
            SELECT 1 FROM history_deleted_dispatch_payloads removed WHERE removed.dispatch_id = d.id)
         FROM jobs j JOIN job_attempts a ON a.job_id = j.id
         JOIN context_snapshots s ON s.run_id = a.run_id
         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
         WHERE j.id = ?1 AND j.checkout_id = ?2 AND d.delivery = 'submitted'
         ORDER BY a.ordinal LIMIT 1",
        params![job, checkout],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, bool>(2)?)),
    ).optional()?.context("The initial Agent dispatch is missing")?;
    ensure!(available, "The initial Agent dispatch was removed");
    let request: JobRequest = serde_json::from_slice(&protection::unprotect(&request)?)?;
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
    ensure!(
        snapshot.scope.mode == request.mode
            && matches!(request.mode, SessionMode::ReadOnly | SessionMode::Plan)
            && matches!(snapshot.native_history, NativeHistory::Fresh)
            && matches!(snapshot.submitted_messages.as_slice(), [message]
                if message.role == MessageRole::User && message.text == request.instructions),
        "The initial Agent dispatch differs from the protected request"
    );
    Ok(())
}

pub(super) fn response_text(
    db: &Connection,
    project: &str,
    workflow: &str,
    response: &WorkflowAgentResponse,
) -> Result<String> {
    let origin = super::runtime::load_node_attempt(db, project, &response.run_id)?
        .context("The linked Agent run is unavailable")?;
    ensure!(
        origin.pin.workflow_run_id == workflow && origin.job_id == response.job_id,
        "The response belongs to a different workflow stage"
    );
    let (dispatch, revision, text) = load_response(
        db,
        project,
        &response.checkout_id,
        &response.job_id,
        &response.run_id,
    )?;
    ensure!(
        dispatch == response.dispatch_id && revision == response.response_revision_id,
        "The linked Agent response changed"
    );
    Ok(text)
}

impl Store {
    pub(crate) fn require_generic_agent_recovery(db: &Connection, job: &Job) -> Result<()> {
        let Some(origin) = &job.pipeline else {
            return Ok(());
        };
        let Some(stage) = super::runtime::load_pinned_node(
            db,
            &job.project_id,
            &origin.pipeline_run_id,
            &origin.node_instance_id,
        )?
        else {
            return Ok(());
        };
        if stage.definition.kind != BuiltinNode::Agent {
            return Ok(());
        }
        let input = load_workflow_job_input(db, &job.id)?
            .context("The Agent lost its pinned workflow inputs")?;
        super::control::require_stage(
            db,
            &job.project_id,
            &origin.pipeline_run_id,
            &origin.node_instance_id,
            input.unit_id.as_deref(),
            &input.artifacts,
        )
    }

    pub(crate) async fn settle_generic_agent_after_attempt(&self, attempt: String) {
        let refreshed = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let origin: Option<(String, String, String)> = tx
                    .query_row(
                        "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id
                 FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
                 JOIN workflow_job_inputs i ON i.job_id = o.job_id WHERE a.run_id = ?1",
                        [attempt],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                if let Some((project, workflow, node)) = origin
                    && super::runtime::load_pinned_node(&tx, &project, &workflow, &node)?
                        .is_some_and(|node| node.definition.kind == BuiltinNode::Agent)
                {
                    super::control::refresh(&tx, &project, &workflow)?;
                }
                tx.commit()?;
                Ok(())
            })
            .await;
        if let Err(error) = refreshed {
            eprintln!(
                "Agent workflow settlement: {}",
                crate::diagnostics::preview(&error.to_string())
            );
        }
    }

    pub async fn workflow_agent_response(
        &self,
        project: String,
        workflow: String,
        artifact: String,
    ) -> Result<String> {
        self.execute(move |db| {
            let artifact = load_artifact(
                db,
                &project,
                &workflow,
                &artifact,
                ArtifactKind::AgentResponse,
            )?
            .context("The Agent response artifact is unavailable")?;
            let ArtifactValue::AgentResponse(response) = artifact.value else {
                anyhow::bail!("The workflow artifact is not an Agent response");
            };
            response_text(db, &project, &workflow, &response)
        })
        .await
    }
}
