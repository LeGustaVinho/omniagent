use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::Store;

use super::{TaskPlacement, TaskWorkflowState, delivery::load_delivery_state, load_project_task};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkEvent {
    pub id: String,
    pub sequence: String,
    pub project_id: String,
    pub task_id: String,
    pub recorded_at: String,
    pub data: AgileEventData,
    pub origin: AgileEventOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgileEventData {
    Created {
        placement: TaskPlacement,
    },
    StateChanged {
        previous: TaskWorkflowState,
        current: TaskWorkflowState,
    },
    ChecklistChecked {
        item_id: String,
        revision: i64,
    },
    ChecklistReopened {
        item_id: String,
        revision: i64,
    },
}

impl AgileEventData {
    fn key(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::StateChanged { .. } => "state_changed",
            Self::ChecklistChecked { .. } => "checklist_checked",
            Self::ChecklistReopened { .. } => "checklist_reopened",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgileEventOrigin {
    Operator,
    Pipeline {
        context: AgilePipelineOrigin,
    },
    Agent {
        run_id: String,
        pipeline: Option<AgilePipelineOrigin>,
    },
}

impl AgileEventOrigin {
    fn pipeline(&self) -> Option<&AgilePipelineOrigin> {
        match self {
            Self::Operator => None,
            Self::Pipeline { context } => Some(context),
            Self::Agent { pipeline, .. } => pipeline.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgilePipelineLink {
    pub pipeline_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgilePipelineOrigin {
    pub run_id: String,
    pub event_id: Option<String>,
    pub automation_chain: Vec<AgilePipelineLink>,
}

impl Store {
    pub async fn project_work_events(
        &self,
        project: String,
        after: Option<i64>,
        limit: u16,
    ) -> Result<Vec<ProjectWorkEvent>> {
        let after = after.unwrap_or_default();
        ensure!(after >= 0, "invalid Agile event sequence");
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT id, sequence, project_id, task_id, recorded_at, payload, origin
                 FROM project_work_events WHERE project_id = ?1 AND sequence > ?2
                 ORDER BY sequence LIMIT ?3",
            )?;
            let rows =
                statement.query_map(params![project, after, limit.clamp(1, 500)], read_event)?;
            rows.map(|row| decode_event(row?)).collect()
        })
        .await
    }

    pub async fn bind_pipeline_work_event_origin(
        &self,
        project: String,
        pipeline_run: String,
        event: String,
    ) -> Result<AgilePipelineOrigin> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_pipeline_origin(&tx, &project, &pipeline_run)?;
            if let Some(prior) = &current.event_id {
                ensure!(prior == &event, "this pipeline run already has a different triggering event");
                return Ok(current);
            }
            let source = tx.query_row(
                "SELECT origin FROM project_work_events WHERE project_id = ?1 AND id = ?2",
                params![project, event], |row| row.get::<_, String>(0),
            ).optional()?.context("triggering work event is outside the project")?;
            let source: AgileEventOrigin = serde_json::from_str(&source)?;
            let mut chain = source.pipeline().map_or_else(Vec::new, |prior| prior.automation_chain.clone());
            let tail = current.automation_chain.into_iter().next().context("missing pipeline identity")?;
            ensure!(!chain.iter().any(|link| link.pipeline_id == tail.pipeline_id), "this pipeline already appears in the originating automation chain");
            let authored: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM project_work_events WHERE project_id = ?1 AND (
                   json_extract(origin, '$.context.runId') = ?2 OR json_extract(origin, '$.pipeline.runId') = ?2))",
                params![project, pipeline_run], |row| row.get(0),
            )?;
            ensure!(!authored, "bind the triggering event before publishing pipeline work changes");
            chain.push(tail);
            tx.execute(
                "INSERT INTO pipeline_work_event_origins (pipeline_run_id, event_id, automation_chain) VALUES (?1, ?2, ?3)",
                params![pipeline_run, event, serde_json::to_string(&chain)?],
            )?;
            tx.commit()?;
            Ok(AgilePipelineOrigin { run_id: pipeline_run, event_id: Some(event), automation_chain: chain })
        }).await
    }
}

pub(super) fn publish_event(
    db: &Connection,
    project: &str,
    task: &str,
    data: AgileEventData,
    origin: &AgileEventOrigin,
) -> Result<()> {
    db.execute(
        "INSERT INTO project_work_events (id, project_id, task_id, kind, payload, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![Uuid::new_v4().to_string(), project, task, data.key(), serde_json::to_string(&data)?, serde_json::to_string(origin)?],
    )?;
    Ok(())
}

pub(crate) struct WorkStateChanges(Vec<(String, TaskWorkflowState)>);

impl WorkStateChanges {
    pub(crate) fn capture(db: &Connection, project: &str, task: &str) -> Result<Self> {
        load_project_task(db, project, task)?.context("work is outside the selected project")?;
        let mut statement = db.prepare(
            "WITH RECURSIVE affected(id) AS (
                VALUES(?2)
                UNION SELECT membership.parent_task_id FROM project_task_memberships membership
                        JOIN affected ON affected.id = membership.task_id
                        WHERE membership.project_id = ?1 AND membership.parent_task_id IS NOT NULL
                UNION SELECT dependent.task_id FROM project_task_prerequisites dependent
                        JOIN affected ON affected.id = dependent.prerequisite_id WHERE dependent.project_id = ?1
             ) SELECT id FROM affected ORDER BY id",
        )?;
        let work = statement.query_map(params![project, task], |row| row.get::<_, String>(0))?;
        Ok(Self(
            work.map(|id| {
                let id = id?;
                let state = load_delivery_state(db, project, &id)?.state;
                Ok((id, state))
            })
            .collect::<Result<_>>()?,
        ))
    }

    pub(crate) fn publish(
        self,
        db: &Connection,
        project: &str,
        origin: &AgileEventOrigin,
    ) -> Result<()> {
        for (task, previous) in self.0 {
            let current = load_delivery_state(db, project, &task)?.state;
            if current != previous {
                publish_event(
                    db,
                    project,
                    &task,
                    AgileEventData::StateChanged { previous, current },
                    origin,
                )?;
            }
        }
        Ok(())
    }
}

pub(crate) fn pipeline_work_origin(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<AgileEventOrigin> {
    Ok(AgileEventOrigin::Pipeline {
        context: load_pipeline_origin(db, project, run)?,
    })
}

pub(super) fn agent_work_origin(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<AgileEventOrigin> {
    let pipeline: Option<String> = db
        .query_row(
            "SELECT origin.pipeline_run_id FROM job_attempts attempt
         JOIN jobs job ON job.id = attempt.job_id
         LEFT JOIN job_pipeline_origins origin ON origin.job_id = job.id
         WHERE attempt.run_id = ?1 AND job.project_id = ?2",
            params![run, project],
            |row| row.get(0),
        )
        .optional()?
        .context("agent attempt is outside the selected project")?;
    Ok(AgileEventOrigin::Agent {
        run_id: run.to_owned(),
        pipeline: pipeline
            .map(|run_id| load_pipeline_origin(db, project, &run_id))
            .transpose()?,
    })
}

fn load_pipeline_origin(db: &Connection, project: &str, run: &str) -> Result<AgilePipelineOrigin> {
    let (pipeline_id, event_id, chain): (String, Option<String>, Option<String>) = db.query_row(
        "SELECT coalesce(pin.workflow_id, pipeline.id), causal.event_id, causal.automation_chain
         FROM job_pipeline_runs pipeline
         LEFT JOIN workflow_run_pins pin ON pin.pipeline_run_id = pipeline.id
         LEFT JOIN pipeline_work_event_origins causal ON causal.pipeline_run_id = pipeline.id
         WHERE pipeline.id = ?1 AND pipeline.project_id = ?2",
        params![run, project], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?.context("pipeline run is outside the selected project")?;
    let automation_chain = chain
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?
        .unwrap_or_else(|| {
            vec![AgilePipelineLink {
                pipeline_id,
                run_id: run.to_owned(),
            }]
        });
    Ok(AgilePipelineOrigin {
        run_id: run.to_owned(),
        event_id,
        automation_chain,
    })
}

type StoredEvent = (String, i64, String, String, String, String, String);

fn read_event(row: &Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
    ))
}

fn decode_event(
    (id, sequence, project_id, task_id, recorded_at, payload, origin): StoredEvent,
) -> Result<ProjectWorkEvent> {
    Ok(ProjectWorkEvent {
        id,
        sequence: sequence.to_string(),
        project_id,
        task_id,
        recorded_at,
        data: serde_json::from_str(&payload)?,
        origin: serde_json::from_str(&origin)?,
    })
}
