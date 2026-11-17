use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::{protection, store::Store};

use super::{
    ApprovalDecision, ArtifactKind, ArtifactValue, Executor,
    artifacts::{load_artifact, load_plan_text},
    digest,
    runtime::{load_definition, load_workflow_run},
};

const PLAN_UNIT_FORMAT: &str = "End the proposed plan with exactly one fenced `omniagent-plan` JSON block containing {\"units\":[{\"purpose\":\"Specific purpose\",\"completionCriteria\":[\"Observable completion condition\"]}]}. Each unit must state its purpose and testable completion criteria. Include the relevant build and test criteria where possible.";
const SINGLE_UNIT_INSTRUCTIONS: &str = "The units array must contain exactly one logical unit covering the entire change. Include all required fixes and checks in that unit.";
const ORDERED_UNITS_INSTRUCTIONS: &str = "List logical commit-sized units in implementation order. Group related changes in one unit and make each unit fit an independently understandable commit.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanUnit {
    pub id: String,
    pub ordinal: u32,
    pub purpose: String,
    pub completion_criteria: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDeliveryUnit {
    pub workflow_run_id: String,
    pub plan_revision: String,
    pub project_id: String,
    pub task_id: String,
    pub checkout_id: String,
    pub unit: WorkflowPlanUnit,
    pub commit_evidence_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposedPlan {
    units: Vec<ProposedUnit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProposedUnit {
    purpose: String,
    completion_criteria: Vec<String>,
}

pub(super) fn parse_plan_units(
    plan_revision: &str,
    proposal: &str,
) -> Result<Vec<WorkflowPlanUnit>> {
    let mut lines = proposal.lines().map(str::trim);
    ensure!(
        lines.by_ref().any(|line| line == "```omniagent-plan"),
        "the proposal needs an ordered logical unit list"
    );
    let mut json = Vec::new();
    let closed = lines.by_ref().any(|line| {
        if line == "```" {
            return true;
        }
        json.push(line);
        false
    });
    ensure!(closed, "the proposed unit list is not closed");
    ensure!(
        !lines.any(|line| line == "```omniagent-plan"),
        "the proposal has more than one unit list"
    );
    let plan: ProposedPlan =
        serde_json::from_str(&json.join("\n")).context("the proposal's unit list is invalid")?;
    ensure!(
        !plan.units.is_empty(),
        "the proposal needs at least one logical unit"
    );
    plan.units
        .into_iter()
        .enumerate()
        .map(|(index, proposal)| {
            ensure!(
                !proposal.purpose.trim().is_empty(),
                "a logical unit needs a purpose"
            );
            ensure!(
                !proposal.completion_criteria.is_empty()
                    && proposal
                        .completion_criteria
                        .iter()
                        .all(|criterion| !criterion.trim().is_empty()),
                "a logical unit needs explicit completion criteria"
            );
            plan_unit(
                plan_revision,
                u32::try_from(index + 1)?,
                proposal.purpose,
                proposal.completion_criteria,
            )
        })
        .collect()
}

fn plan_unit(
    revision: &str,
    ordinal: u32,
    purpose: String,
    completion_criteria: Vec<String>,
) -> Result<WorkflowPlanUnit> {
    Ok(WorkflowPlanUnit {
        id: digest(&serde_json::to_vec(&(revision, ordinal))?),
        ordinal,
        purpose,
        completion_criteria,
    })
}

fn requires_single_unit(db: &Connection, project: &str, workflow: &str) -> Result<bool> {
    let Some(run) = load_workflow_run(db, project, workflow)? else {
        return Ok(false);
    };
    let definition = load_definition(db, project, &run.version)?
        .context("the pinned workflow definition is unavailable")?;
    // Implementation only advances past the first unit after a verified commit.
    Ok(definition
        .nodes
        .iter()
        .any(|node| node.definition.executor == Executor::WriteAgent)
        && !definition
            .nodes
            .iter()
            .any(|node| node.definition.executor == Executor::VerifyGitCommit))
}

fn parse_scoped_plan_units(
    db: &Connection,
    revision: &str,
    value: ArtifactValue,
    single_unit: bool,
) -> Result<Vec<WorkflowPlanUnit>> {
    let units = match value {
        ArtifactValue::Plan {
            run_id,
            event_sequence,
        } => parse_plan_units(revision, &load_plan_text(db, &run_id, event_sequence)?)?,
        ArtifactValue::BacklogPlan(plan) => {
            let (purpose, criteria) = plan.unit_scope();
            vec![plan_unit(revision, 1, purpose.into(), criteria.to_vec())?]
        }
        _ => anyhow::bail!("plan has the wrong artifact kind"),
    };
    ensure!(
        !single_unit || units.len() == 1,
        "an implementation workflow without a verified commit requires exactly one logical unit"
    );
    Ok(units)
}

pub(crate) fn validate_selected_plan_revision(
    db: &Connection,
    project: &str,
    workflow: &str,
    revision: &str,
) -> Result<()> {
    let backlog_child: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_backlog_task_runs WHERE child_run_id = ?1)",
        [workflow],
        |row| row.get(0),
    )?;
    if backlog_child {
        ensure!(
            load_artifact(db, project, workflow, revision, ArtifactKind::Plan)?
                .is_some_and(|artifact| matches!(artifact.value, ArtifactValue::BacklogPlan(_))),
            "The task plan is pinned to the approved backlog"
        );
    }
    if requires_single_unit(db, project, workflow)? {
        let plan = load_artifact(db, project, workflow, revision, ArtifactKind::Plan)?
            .context("selected plan is not part of this workflow")?;
        parse_scoped_plan_units(db, revision, plan.value, true)?;
    }
    Ok(())
}

impl Store {
    pub(super) async fn workflow_plan_unit_instructions(
        &self,
        project: String,
        workflow: String,
    ) -> Result<String> {
        self.execute(move |db| {
            let requirements = if requires_single_unit(db, &project, &workflow)? {
                SINGLE_UNIT_INSTRUCTIONS
            } else {
                ORDERED_UNITS_INSTRUCTIONS
            };
            Ok(format!("{PLAN_UNIT_FORMAT} {requirements}"))
        })
        .await
    }

    pub async fn workflow_plan_units(
        &self,
        project: String,
        workflow: String,
        revision: String,
    ) -> Result<Vec<WorkflowPlanUnit>> {
        let plan = self
            .resolve_workflow_artifact(
                project.clone(),
                workflow.clone(),
                revision.clone(),
                ArtifactKind::Plan,
            )
            .await?
            .context("selected plan is not part of this workflow")?;
        self.execute(move |db| {
            let single_unit = requires_single_unit(db, &project, &workflow)?;
            parse_scoped_plan_units(db, &revision, plan.value, single_unit)
        })
        .await
    }

    pub async fn workflow_delivery_units(
        &self,
        project: String,
        task: String,
        run: String,
    ) -> Result<Vec<WorkflowDeliveryUnit>> {
        self.execute(move |db| load_delivery_units(db, &project, &task, &run))
            .await
    }
}

pub(super) fn record_delivery_units(
    db: &Connection,
    decision: &ApprovalDecision,
    checkout: &str,
    units: Vec<WorkflowPlanUnit>,
) -> Result<()> {
    for unit in units {
        let existing = db.query_row(
            "SELECT pipeline_run_id, plan_revision, project_id, task_id, checkout_id, unit, commit_evidence_id
             FROM workflow_delivery_units WHERE unit_id = ?1",
            [&unit.id], |row| Ok(decode_unit(row)),
        ).optional()?.transpose()?;
        if let Some(existing) = existing {
            ensure!(
                existing.workflow_run_id == decision.workflow_run_id
                    && existing.plan_revision == decision.plan_revision
                    && existing.project_id == decision.task.project_id
                    && existing.task_id == decision.task.task_id
                    && existing.checkout_id == checkout
                    && existing.unit == unit,
                "the preparation differs from the worktree and units originally scoped to this plan"
            );
            continue;
        }
        db.execute(
            "INSERT INTO workflow_delivery_units VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            params![
                unit.id,
                decision.workflow_run_id,
                decision.plan_revision,
                decision.task.project_id,
                decision.task.task_id,
                checkout,
                unit.ordinal,
                protection::protect(&serde_json::to_vec(&unit)?)?
            ],
        )?;
    }
    Ok(())
}

const ACTIVE_UNITS: &str = "SELECT u.pipeline_run_id, u.plan_revision, u.project_id,
    u.task_id, u.checkout_id, u.unit, u.commit_evidence_id FROM workflow_delivery_units u
    JOIN workflow_delivery_heads h ON h.project_id = u.project_id AND h.task_id = u.task_id
        AND h.pipeline_run_id = u.pipeline_run_id AND h.plan_revision = u.plan_revision
    JOIN job_pipeline_runs p ON p.id = h.pipeline_run_id AND p.plan_revision = h.plan_revision
    WHERE u.project_id = ?1 AND u.task_id = ?2";

pub(crate) fn load_delivery_units(
    db: &Connection,
    project: &str,
    task: &str,
    run: &str,
) -> Result<Vec<WorkflowDeliveryUnit>> {
    let mut statement = db.prepare(&format!(
        "{ACTIVE_UNITS} AND u.pipeline_run_id = ?3 ORDER BY u.ordinal"
    ))?;
    let mut rows = statement.query(params![project, task, run])?;
    let mut units = Vec::new();
    while let Some(row) = rows.next()? {
        units.push(decode_unit(row)?);
    }
    Ok(units)
}

pub(crate) fn load_delivery_unit(
    db: &Connection,
    project: &str,
    task: &str,
    unit: &str,
) -> Result<Option<WorkflowDeliveryUnit>> {
    db.query_row(
        &format!("{ACTIVE_UNITS} AND u.unit_id = ?3"),
        params![project, task, unit],
        |row| Ok(decode_unit(row)),
    )
    .optional()?
    .transpose()
}

fn decode_unit(row: &Row<'_>) -> Result<WorkflowDeliveryUnit> {
    let unit: Vec<u8> = row.get(5)?;
    Ok(WorkflowDeliveryUnit {
        workflow_run_id: row.get(0)?,
        plan_revision: row.get(1)?,
        project_id: row.get(2)?,
        task_id: row.get(3)?,
        checkout_id: row.get(4)?,
        unit: serde_json::from_slice(&protection::unprotect(&unit)?)?,
        commit_evidence_id: row.get(6)?,
    })
}

pub(crate) fn validate_unit_capture(
    db: &Connection,
    unit: &WorkflowDeliveryUnit,
    capture_run_id: &str,
) -> Result<()> {
    let matched: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN jobs j ON j.id = a.job_id
         JOIN runs r ON r.id = a.run_id JOIN workflow_job_inputs i ON i.job_id = j.id
         JOIN job_pipeline_origins o ON o.job_id = j.id
         WHERE a.run_id = ?1 AND i.unit_id = ?2 AND o.pipeline_run_id = ?3 AND o.plan_revision = ?4
           AND j.project_id = ?5 AND j.checkout_id = ?6 AND j.mode = 'approved_write' AND r.status = 'succeeded')",
        params![capture_run_id, unit.unit.id, unit.workflow_run_id, unit.plan_revision, unit.project_id, unit.checkout_id], |row| row.get(0),
    )?;
    ensure!(
        matched,
        "candidate capture is outside the logical unit's workflow attempt"
    );
    Ok(())
}

pub(crate) fn bind_delivery_commit(
    db: &Connection,
    unit: &WorkflowDeliveryUnit,
    evidence_id: &str,
) -> Result<()> {
    let active = load_delivery_unit(db, &unit.project_id, &unit.task_id, &unit.unit.id)?
        .context("logical unit is no longer part of the active delivery")?;
    ensure!(
        active.workflow_run_id == unit.workflow_run_id
            && active.plan_revision == unit.plan_revision
            && active.checkout_id == unit.checkout_id,
        "logical unit delivery scope changed"
    );
    let origin = crate::tasks::pipeline_work_origin(db, &unit.project_id, &unit.workflow_run_id)?;
    let states = crate::tasks::WorkStateChanges::capture(db, &unit.project_id, &unit.task_id)?;
    let changed = db.execute(
        "UPDATE workflow_delivery_units SET commit_evidence_id = ?2 WHERE unit_id = ?1 AND (commit_evidence_id IS NULL OR commit_evidence_id = ?2)",
        params![unit.unit.id, evidence_id],
    )?;
    ensure!(
        changed == 1,
        "logical unit already has different commit evidence"
    );
    let units = load_delivery_units(db, &unit.project_id, &unit.task_id, &unit.workflow_run_id)?;
    if units.iter().all(|unit| unit.commit_evidence_id.is_some()) {
        crate::tasks::record_verified_task_delivery(
            db,
            &unit.project_id,
            &unit.task_id,
            &unit.workflow_run_id,
            &unit.plan_revision,
        )?;
    }
    states.publish(db, &unit.project_id, &origin)?;
    Ok(())
}
