use std::path::Path;

use crate::{
    protection,
    workflows::{
        BuiltinNode, NodeInstance, WorkflowDefinition, WorkflowRevision, builtin_registry,
    },
};

use super::*;

fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE workflow_run_pins (pipeline_run_id TEXT PRIMARY KEY);
         CREATE TABLE workflow_node_pins (
             pipeline_run_id TEXT NOT NULL, node_instance_id TEXT NOT NULL,
             PRIMARY KEY (pipeline_run_id, node_instance_id)
         );
         CREATE TABLE workflow_artifacts (id TEXT PRIMARY KEY);
         CREATE TABLE workflow_delivery_units (
             unit_id TEXT PRIMARY KEY, pipeline_run_id TEXT NOT NULL, project_id TEXT NOT NULL,
             plan_revision TEXT NOT NULL
         );
         CREATE TABLE job_pipeline_runs (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, plan_revision TEXT);
         CREATE TABLE workflow_revisions (
             project_id TEXT NOT NULL, workflow_id TEXT NOT NULL, revision TEXT NOT NULL, definition BLOB NOT NULL,
             PRIMARY KEY (project_id, workflow_id, revision)
         );",
    )?;
    db.execute_batch(include_str!(
        "../../../../migrations/130-workflow-delivery-attempts.sql"
    ))?;
    Ok(())
}

fn open(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch("PRAGMA foreign_keys = ON")?;
    Ok(db)
}

fn setup(
    db: &Connection,
    run_id: &str,
    version: &WorkflowRevision,
    gates: &[PinnedNode],
) -> Result<WorkflowRun> {
    let plan = format!("{run_id}-plan-original");
    db.execute("INSERT INTO workflow_run_pins VALUES (?1)", [run_id])?;
    for node in gates {
        db.execute(
            "INSERT INTO workflow_node_pins VALUES (?1, ?2)",
            params![run_id, node.instance.id],
        )?;
    }
    db.execute(
        "INSERT INTO job_pipeline_runs VALUES (?1, 'project', NULL)",
        [run_id],
    )?;
    let mut run = WorkflowRun {
        id: run_id.into(),
        project_id: "project".into(),
        version: version.clone(),
        plan_revision: None,
    };
    select_plan(db, &mut run, &plan)?;
    Ok(run)
}

fn select_plan(db: &Connection, run: &mut WorkflowRun, plan: &str) -> Result<String> {
    let unit = format!("{plan}-unit");
    db.execute("INSERT INTO workflow_artifacts VALUES (?1)", [plan])?;
    db.execute(
        "UPDATE job_pipeline_runs SET plan_revision = ?2 WHERE id = ?1",
        params![run.id, plan],
    )?;
    db.execute(
        "INSERT INTO workflow_delivery_units VALUES (?1, ?2, ?3, ?4)",
        params![unit, run.id, run.project_id, plan],
    )?;
    run.plan_revision = Some(plan.into());
    Ok(unit)
}

fn definition(db: &Connection, id: &str, nodes: Vec<PinnedNode>) -> Result<WorkflowRevision> {
    let version = WorkflowRevision {
        id: id.into(),
        revision: "revision".into(),
    };
    let pinned = WorkflowDefinition {
        version: version.clone(),
        name: "Delivery test".into(),
        nodes,
        wires: vec![],
        subflows: vec![],
    };
    db.execute(
        "INSERT INTO workflow_revisions VALUES ('project', ?1, ?2, ?3)",
        params![
            version.id,
            version.revision,
            protection::protect(&serde_json::to_vec(&pinned)?)?
        ],
    )?;
    Ok(version)
}

#[test]
fn a_ci_replan_and_quality_rounds_use_the_same_durable_delivery_slots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("attempts.sqlite");
    let db = open(&path)?;
    initialize(&db)?;
    let gate = builtin_registry()
        .into_iter()
        .find(|definition| definition.kind == BuiltinNode::DeliveryAttempt)
        .context("The delivery attempt gate is unavailable")?;
    let nodes = (1..=3)
        .map(|ordinal| PinnedNode {
            instance: NodeInstance {
                id: format!("implementation::attempt-{ordinal}"),
                kind: gate.kind,
                configuration: NodeConfiguration::DeliveryAttempt { ordinal, limit: 3 },
            },
            revision: format!("attempt-{ordinal}"),
            definition: gate.clone(),
        })
        .collect::<Vec<_>>();
    let version = definition(&db, "delivery", nodes.clone())?;
    let mut run = setup(&db, "child", &version, &nodes)?;
    let original_unit = "child-plan-original-unit";
    assert_eq!(
        ci_repair_budget(&db, &run)?,
        CiRepairBudget::NoCurrentAttempt
    );
    for _ in 0..2 {
        assert_eq!(
            stage_port(&db, &run, &nodes[0], Some(original_unit), 1, 3)?,
            "admitted"
        );
    }
    assert_eq!(
        stage_port(&db, &run, &nodes[1], Some(original_unit), 2, 3)?,
        "admitted"
    );
    assert_eq!(ci_repair_budget(&db, &run)?, CiRepairBudget::Available);
    let original = run.clone();
    let repaired_unit = select_plan(&db, &mut run, "ci-repair")?;
    assert!(stage_port(&db, &original, &nodes[2], Some(original_unit), 3, 3).is_err());
    assert_eq!(
        ci_repair_budget(&db, &run)?,
        CiRepairBudget::NoCurrentAttempt
    );
    drop(db);

    let db = open(&path)?;
    assert_eq!(
        stage_port(&db, &run, &nodes[0], Some(&repaired_unit), 1, 3)?,
        "admitted"
    );
    assert_eq!(ci_repair_budget(&db, &run)?, CiRepairBudget::Exhausted(3));
    assert_eq!(
        stage_port(&db, &run, &nodes[1], Some(&repaired_unit), 2, 3)?,
        "exhausted"
    );
    assert_eq!(
        stage_port(&db, &run, &nodes[0], Some(&repaired_unit), 1, 3)?,
        "admitted"
    );
    let count: u32 = db.query_row(
        "SELECT count(*) FROM workflow_delivery_attempts WHERE pipeline_run_id = ?1",
        [&run.id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 3);
    let sibling = setup(&db, "sibling", &version, &nodes)?;
    assert_eq!(
        stage_port(
            &db,
            &sibling,
            &nodes[0],
            Some("sibling-plan-original-unit"),
            1,
            3
        )?,
        "admitted"
    );
    assert_eq!(ci_repair_budget(&db, &sibling)?, CiRepairBudget::Available);
    let legacy = definition(&db, "original-workflow", vec![])?;
    let legacy = setup(&db, "legacy", &legacy, &[])?;
    assert_eq!(ci_repair_budget(&db, &legacy)?, CiRepairBudget::Unmarked);
    Ok(())
}
