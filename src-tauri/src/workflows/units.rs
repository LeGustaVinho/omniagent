use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::{protection, store::Store};

use super::{ApprovalDecision, ArtifactKind, ArtifactValue, digest};

pub(super) const PLAN_UNITS_INSTRUCTIONS: &str = "End the proposed plan with exactly one fenced `omniagent-plan` JSON block containing {\"units\":[{\"purpose\":\"Specific purpose\",\"completionCriteria\":[\"Observable completion condition\"]}]}. List logical commit-sized units in implementation order. Use one unit for changes that belong together. Each unit must state its purpose and testable completion criteria, group related changes, and fit an independently understandable commit. Include the relevant build and test criteria where possible.";

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
            let ordinal = u32::try_from(index + 1)?;
            Ok(WorkflowPlanUnit {
                id: digest(&serde_json::to_vec(&(plan_revision, ordinal))?),
                ordinal,
                purpose: proposal.purpose,
                completion_criteria: proposal.completion_criteria,
            })
        })
        .collect()
}

impl Store {
    pub async fn workflow_plan_units(
        &self,
        project: String,
        workflow: String,
        revision: String,
    ) -> Result<Vec<WorkflowPlanUnit>> {
        let plan = self
            .resolve_workflow_artifact(project, workflow, revision.clone(), ArtifactKind::Plan)
            .await?
            .context("selected plan is not part of this workflow")?;
        let ArtifactValue::Plan {
            run_id,
            event_sequence,
        } = plan.value
        else {
            anyhow::bail!("plan has the wrong artifact kind");
        };
        let text = self.workflow_plan_text(run_id, event_sequence).await?;
        parse_plan_units(&revision, &text)
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
    Ok(())
}
