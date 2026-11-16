use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    jobs::{AdmissionEnvironment, Job, JobInspection, load_job},
    protection,
    runs::RunStatus,
    store::Store,
};

use super::{
    ArtifactValue, ControlStageReceipt, NodeInputs, WorkflowArtifact, WorkflowDefinition,
    WorkflowJobInput, WorkflowPlanUnit, WorkflowRevision, WorkflowRun,
    authoring::package::roles::{WorkflowRolePin, role_pins},
    control::{recorded_control, unit_nodes},
    digest,
    launch::{WorkflowLaunchReceipt, load_workflow_launch},
    runtime::load_definition,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionInspection {
    pub run: WorkflowRun,
    pub launch: Option<WorkflowLaunchReceipt>,
    pub definition: WorkflowDefinition,
    pub role_pins: Vec<WorkflowRolePin>,
    pub unit_node_ids: Vec<String>,
    pub units: Vec<WorkflowExecutionUnit>,
    pub jobs: Vec<WorkflowExecutionJob>,
    pub outputs: Vec<WorkflowExecutionOutput>,
    pub control: Vec<ControlStageReceipt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionUnit {
    pub plan_revision: String,
    pub task_id: String,
    pub unit: WorkflowPlanUnit,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionJob {
    pub job: Job,
    pub node_id: String,
    pub unit_id: Option<String>,
    pub inputs: Option<NodeInputs>,
    pub attempts: Vec<WorkflowExecutionAttempt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionAttempt {
    pub run_id: String,
    pub ordinal: u32,
    pub previous_run_id: Option<String>,
    pub conversation_id: Option<String>,
    pub status: RunStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionOutput {
    pub artifact: WorkflowArtifact,
    pub current: bool,
    pub input_receipts: Vec<WorkflowExecutionInputs>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionInputs {
    pub execution_key: String,
    pub revision: String,
    pub inputs: NodeInputs,
}

pub(super) fn record_workflow_completion_inputs(
    db: &Connection,
    output: &WorkflowArtifact,
    key: &str,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<()> {
    let ids = inputs
        .iter()
        .map(|(port, artifact)| (port.clone(), artifact.id.clone()))
        .collect();
    record_workflow_input_ids(db, output, key, &ids)
}

pub(super) fn record_workflow_input_ids(
    db: &Connection,
    output: &WorkflowArtifact,
    key: &str,
    inputs: &NodeInputs,
) -> Result<()> {
    let payload = serde_json::to_vec(inputs)?;
    db.execute(
        "INSERT INTO workflow_completion_inputs VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT(output_artifact_id, execution_key, input_revision) DO NOTHING",
        params![
            output.id,
            key,
            digest(&payload),
            protection::protect(&payload)?
        ],
    )?;
    Ok(())
}

impl Store {
    pub async fn inspect_workflow_execution(
        &self,
        project: String,
        run_id: String,
    ) -> Result<Option<WorkflowExecutionInspection>> {
        self.refresh_control(project.clone(), run_id.clone())
            .await?;
        self.execute(move |db| {
            let tx = db.transaction()?;
            let run = tx
                .query_row(
                    "SELECT p.workflow_id, p.workflow_revision, r.plan_revision
                FROM workflow_run_pins p JOIN job_pipeline_runs r ON r.id = p.pipeline_run_id
                WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2",
                    params![project, run_id],
                    |row| {
                        Ok(WorkflowRun {
                            id: run_id.clone(),
                            project_id: project.clone(),
                            version: WorkflowRevision {
                                id: row.get(0)?,
                                revision: row.get(1)?,
                            },
                            plan_revision: row.get(2)?,
                        })
                    },
                )
                .optional()?;
            let Some(run) = run else { return Ok(None) };
            let definition = load_definition(&tx, &project, &run.version)?
                .context("The pinned workflow definition is unavailable")?;
            let role_pins = role_pins(&tx, &project, &definition)?;
            let launch = load_workflow_launch(&tx, &project, &run_id)?;
            let scoped_nodes = unit_nodes(&definition);
            let unit_node_ids = definition
                .nodes
                .iter()
                .filter(|node| scoped_nodes.contains(node.instance.id.as_str()))
                .map(|node| node.instance.id.clone())
                .collect();
            let units = load_units(&tx, &run)?;
            let jobs = load_jobs(&tx, &run)?;
            let outputs = load_outputs(&tx, &run, &jobs, &units)?;
            let control = recorded_control(&tx, &run)?;
            Ok(Some(WorkflowExecutionInspection {
                run,
                launch,
                definition,
                role_pins,
                unit_node_ids,
                units,
                jobs,
                outputs,
                control,
            }))
        })
        .await
    }

    pub async fn inspect_workflow_execution_job(
        &self,
        project: String,
        run: String,
        job: String,
        environment: AdmissionEnvironment,
    ) -> Result<Option<JobInspection>> {
        let matches = self.execute({
            let (project, job) = (project.clone(), job.clone());
            move |db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM job_pipeline_origins o
                JOIN workflow_run_pins p ON p.pipeline_run_id = o.pipeline_run_id AND p.project_id = o.project_id
                WHERE o.project_id = ?1 AND o.pipeline_run_id = ?2 AND o.job_id = ?3)", params![project, run, job], |row| row.get::<_, bool>(0))?)
        }).await?;
        if !matches {
            return Ok(None);
        }
        self.inspect_job(project, job, environment).await
    }
}

fn load_units(db: &Connection, run: &WorkflowRun) -> Result<Vec<WorkflowExecutionUnit>> {
    db.prepare(
        "SELECT plan_revision, task_id, unit FROM workflow_delivery_units
         WHERE project_id = ?1 AND pipeline_run_id = ?2 ORDER BY plan_revision, ordinal",
    )?
    .query_map(params![run.project_id, run.id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get::<_, Vec<u8>>(2)?))
    })?
    .map(|row| {
        let (plan_revision, task_id, payload) = row?;
        Ok(WorkflowExecutionUnit {
            plan_revision,
            task_id,
            unit: serde_json::from_slice(&protection::unprotect(&payload)?)?,
        })
    })
    .collect()
}

fn load_jobs(db: &Connection, run: &WorkflowRun) -> Result<Vec<WorkflowExecutionJob>> {
    db.prepare("SELECT o.node_instance_id, j.id, i.payload FROM job_pipeline_origins o
        JOIN jobs j ON j.id = o.job_id JOIN workflow_node_pins n ON n.pipeline_run_id = o.pipeline_run_id AND n.node_instance_id = o.node_instance_id
        LEFT JOIN workflow_job_inputs i ON i.job_id = j.id
        WHERE o.project_id = ?1 AND j.project_id = ?1 AND o.pipeline_run_id = ?2 ORDER BY j.sequence")?
        .query_map(params![run.project_id, run.id], |row| Ok((row.get(0)?, row.get::<_, String>(1)?, row.get::<_, Option<Vec<u8>>>(2)?)))?
        .map(|row| {
            let (node_id, id, payload) = row?;
            let job = load_job(db, &run.project_id, &id)?.context("The pinned workflow job disappeared")?;
            let input = payload.map(|bytes| -> Result<WorkflowJobInput> { Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?) }).transpose()?;
            let attempts = db.prepare("SELECT a.run_id, a.ordinal, a.previous_run_id, t.conversation_id, r.status
                 FROM job_attempts a JOIN runs r ON r.id = a.run_id LEFT JOIN agent_attempts t ON t.run_id = a.run_id
                 WHERE a.job_id = ?1 ORDER BY a.ordinal")?.query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, String>(4)?)))?
                .map(|row| {
                    let (run_id, ordinal, previous_run_id, conversation_id, status) = row?;
                    Ok(WorkflowExecutionAttempt { run_id, ordinal, previous_run_id, conversation_id, status: RunStatus::from_sql(&status)? })
                }).collect::<Result<_>>()?;
            Ok(WorkflowExecutionJob { node_id, job, attempts, unit_id: input.as_ref().and_then(|input| input.unit_id.clone()), inputs: input.map(|input| input.artifacts) })
        }).collect()
}

fn load_outputs(
    db: &Connection,
    run: &WorkflowRun,
    jobs: &[WorkflowExecutionJob],
    units: &[WorkflowExecutionUnit],
) -> Result<Vec<WorkflowExecutionOutput>> {
    let attempts: HashMap<_, _> = jobs
        .iter()
        .flat_map(|entry| {
            entry
                .attempts
                .iter()
                .map(|attempt| (attempt.run_id.as_str(), &entry.job))
        })
        .collect();
    let plans: HashMap<_, _> = units
        .iter()
        .map(|entry| (entry.unit.id.as_str(), entry.plan_revision.as_str()))
        .collect();
    let selected = run.plan_revision.as_deref();
    let decisions = db
        .prepare(
            "SELECT d.decision_artifact_id FROM workflow_approval_decisions d
             WHERE d.pipeline_run_id = ?1 AND d.plan_revision = ?2 AND NOT EXISTS (
                 SELECT 1 FROM workflow_approval_decisions successor
                 WHERE successor.pipeline_run_id = d.pipeline_run_id
                   AND successor.node_instance_id = d.node_instance_id
                   AND successor.plan_revision = d.plan_revision
                   AND successor.generation > d.generation)",
        )?
        .query_map(params![run.id, selected], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    db.prepare("WITH outputs(id, node_instance_id, port_name, payload) AS (
        SELECT a.id, a.node_instance_id, a.port_name, a.payload
        FROM workflow_artifacts a JOIN workflow_run_pins p ON p.pipeline_run_id = a.pipeline_run_id
        WHERE p.project_id = ?1 AND a.pipeline_run_id = ?2
        UNION ALL
        SELECT c.artifact_id, c.node_instance_id, 'result', c.payload
        FROM workflow_command_results c JOIN workflow_run_pins p ON p.pipeline_run_id = c.pipeline_run_id
        WHERE p.project_id = ?1 AND c.pipeline_run_id = ?2
        UNION ALL
        SELECT a.artifact_id, m.node_instance_id, 'result', a.payload FROM workflow_mcp_attempts a
        JOIN workflow_mcp_jobs m ON m.job_id = a.job_id JOIN workflow_run_pins p ON p.pipeline_run_id = m.pipeline_run_id
        WHERE p.project_id = ?1 AND m.pipeline_run_id = ?2 AND a.artifact_id IS NOT NULL AND a.payload IS NOT NULL
        ) SELECT a.payload,
        EXISTS(SELECT 1 FROM workflow_host_completions h WHERE h.artifact_id = a.id),
        EXISTS(SELECT 1 FROM workflow_host_completions h WHERE h.artifact_id = a.id AND (h.execution_key = '' OR h.execution_key = ?3))
        FROM outputs a ORDER BY a.node_instance_id, a.port_name, a.id")?
        .query_map(params![run.project_id, run.id, selected], |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, bool>(1)?, row.get::<_, bool>(2)?)))?
        .map(|row| {
            let (bytes, host, current_host) = row?;
            let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&bytes)?)?;
            let current = if let Some(unit) = artifact.unit_id.as_deref() {
                plans.get(unit).is_some_and(|plan| Some(*plan) == selected)
            } else if host {
                current_host
            } else {
                match &artifact.value {
                    ArtifactValue::Plan { .. } => selected == Some(artifact.id.as_str()),
                    ArtifactValue::Approval(_) => decisions.contains(&artifact.id),
                    ArtifactValue::Release(reference) => decisions.contains(&reference.decision_artifact_id),
                    ArtifactValue::Review { run_id } | ArtifactValue::CommandResult(super::WorkflowCommandResult { run_id, .. }) => {
                        attempts.get(run_id.as_str()).is_some_and(|job| job.pipeline.as_ref().and_then(|origin| origin.plan_revision.as_deref()) == selected)
                    }
                    ArtifactValue::McpToolResult(super::mcp_tools::WorkflowMcpToolResult { run_id, .. }) => attempts.contains_key(run_id.as_str()),
                    _ => true,
                }
            };
            let input_receipts = db.prepare("SELECT execution_key, input_revision, payload FROM workflow_completion_inputs
                WHERE output_artifact_id = ?1 ORDER BY execution_key, input_revision")?.query_map([&artifact.id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get::<_, Vec<u8>>(2)?))
            })?.map(|row| {
                let (execution_key, revision, payload) = row?;
                Ok(WorkflowExecutionInputs { execution_key, revision, inputs: serde_json::from_slice(&protection::unprotect(&payload)?)? })
            }).collect::<Result<_>>()?;
            Ok(WorkflowExecutionOutput { artifact, current, input_receipts })
        }).collect()
}
