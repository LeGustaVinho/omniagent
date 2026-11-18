use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::workflows::{NodeConfiguration, PinnedNode, WorkflowRun, runtime::load_definition};

pub(in crate::workflows) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    ordinal: u16,
    limit: u16,
) -> Result<&'static str> {
    let plan = run
        .plan_revision
        .as_deref()
        .context("A delivery attempt needs an approved plan")?;
    let unit = unit.context("A delivery attempt needs an approved logical unit")?;
    ensure!(
        matches!(node.instance.configuration, NodeConfiguration::DeliveryAttempt {
            ordinal: pinned_ordinal, limit: pinned_limit,
        } if ordinal == pinned_ordinal && limit == pinned_limit)
            && (1..=32).contains(&ordinal)
            && (1..=32).contains(&limit),
        "The delivery attempt differs from its pinned ordinal or limit"
    );
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_delivery_units u JOIN job_pipeline_runs p
           ON p.id = u.pipeline_run_id AND p.project_id = u.project_id
         WHERE u.unit_id = ?1 AND u.pipeline_run_id = ?2 AND u.project_id = ?3
           AND u.plan_revision = ?4 AND p.plan_revision = u.plan_revision)",
        params![unit, run.id, run.project_id, plan],
        |row| row.get(0),
    )?;
    ensure!(
        active,
        "The delivery attempt is outside the active approved plan and unit"
    );
    db.execute(
        "INSERT INTO workflow_delivery_attempts
         (pipeline_run_id, plan_revision, node_instance_id, unit_id, ordinal, attempt_number)
         SELECT ?1, ?2, ?3, ?4, ?5, count(*) + 1
         FROM workflow_delivery_attempts WHERE pipeline_run_id = ?1
         HAVING count(*) < ?6
         ON CONFLICT(pipeline_run_id, plan_revision, node_instance_id) DO NOTHING",
        params![run.id, plan, node.instance.id, unit, ordinal, limit],
    )?;
    let reserved: Option<(String, u16)> = db
        .query_row(
            "SELECT unit_id, ordinal FROM workflow_delivery_attempts
         WHERE pipeline_run_id = ?1 AND plan_revision = ?2 AND node_instance_id = ?3",
            params![run.id, plan, node.instance.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((saved_unit, saved_ordinal)) = reserved {
        ensure!(
            saved_unit == unit && saved_ordinal == ordinal,
            "The saved delivery attempt belongs to another logical unit or ordinal"
        );
        Ok("admitted")
    } else {
        Ok("exhausted")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CiRepairBudget {
    Unmarked,
    NoCurrentAttempt,
    Available,
    Exhausted(u16),
}

pub(super) fn ci_repair_budget(db: &Connection, run: &WorkflowRun) -> Result<CiRepairBudget> {
    let definition = load_definition(db, &run.project_id, &run.version)?
        .context("The pinned delivery definition is unavailable")?;
    let mut limits = definition.nodes.iter().filter_map(|node| {
        let NodeConfiguration::DeliveryAttempt { limit, .. } = node.instance.configuration else {
            return None;
        };
        Some(limit)
    });
    let Some(limit) = limits.next() else {
        return Ok(CiRepairBudget::Unmarked);
    };
    ensure!(
        (1..=32).contains(&limit) && limits.all(|other| other == limit),
        "The pinned delivery attempt gates disagree about the shared budget"
    );
    let plan = run
        .plan_revision
        .as_deref()
        .context("A CI repair needs an approved plan")?;
    let (used, in_plan): (u32, u32) = db.query_row(
        "SELECT count(*), count(CASE WHEN plan_revision = ?2 THEN 1 END)
         FROM workflow_delivery_attempts WHERE pipeline_run_id = ?1",
        params![run.id, plan],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(if in_plan == 0 {
        CiRepairBudget::NoCurrentAttempt
    } else if used >= u32::from(limit) {
        CiRepairBudget::Exhausted(limit)
    } else {
        CiRepairBudget::Available
    })
}
