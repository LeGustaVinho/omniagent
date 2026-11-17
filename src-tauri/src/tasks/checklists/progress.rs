use std::fmt;

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{dispatch::ContextSnapshot, protection, store::Store};

use super::{ChecklistExecutionMode, ChecklistItem, load_checklist_items, revise_checklist_item};
use crate::tasks::{AgileEventOrigin, agile_events::agent_work_origin, pipeline_work_origin};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistProgressReceipt {
    pub item_id: String,
    pub revision: i64,
    pub checked: bool,
}

impl From<&ChecklistItem> for ChecklistProgressReceipt {
    fn from(item: &ChecklistItem) -> Self {
        Self {
            item_id: item.id.clone(),
            revision: item.revision,
            checked: item.checked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistProgressAvailability {
    Ready,
    PendingDispatch,
    Inactive,
    BridgeUnavailable,
    BridgeUnconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistProgressAccess {
    pub task_id: String,
    pub mode: ChecklistExecutionMode,
    pub selected_item_id: Option<String>,
    pub availability: ChecklistProgressAvailability,
    pub items: Vec<ChecklistProgressReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ChecklistProgressActor {
    Operator,
    Agent {
        run_id: String,
        dispatch_id: String,
        request_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecklistProgressRecord {
    pub sequence: String,
    pub actor: ChecklistProgressActor,
    pub before_revision: i64,
    pub after: ChecklistProgressReceipt,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecklistProgressError {
    RevisionConflict { current_revision: i64 },
    RequestConflict,
    OutsideScope,
    Inactive,
}

impl fmt::Display for ChecklistProgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { current_revision } => write!(
                f,
                "The checklist item changed. Its current revision is {current_revision}"
            ),
            Self::RequestConflict => f.write_str(
                "This run already used that request ID for a different checklist update",
            ),
            Self::OutsideScope => f.write_str("The item is outside the checklist update scope"),
            Self::Inactive => {
                f.write_str("The checklist attempt is inactive or its input has not been submitted")
            }
        }
    }
}

impl std::error::Error for ChecklistProgressError {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChecklistProgressRequest {
    pub request_id: String,
    pub item_id: String,
    pub expected_revision: i64,
    pub checked: bool,
}

struct RunChecklistScope {
    project_id: String,
    task_id: String,
    mode: ChecklistExecutionMode,
    selected: Option<String>,
    active: bool,
    bridge: Option<String>,
    dispatch_id: Option<String>,
    items: Vec<ChecklistProgressReceipt>,
}

impl Store {
    pub async fn set_project_checklist_item_checked(
        &self,
        project: String,
        task: String,
        item: String,
        expected_revision: i64,
        checked: bool,
    ) -> Result<ChecklistProgressReceipt> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current =
                item_at_expected_revision(&tx, &project, &task, &item, expected_revision)?;
            let revised = revise_checklist_item(
                &tx,
                &project,
                &task,
                &current,
                current.body.clone(),
                checked,
                &AgileEventOrigin::Operator,
            )?;
            record_operator_check_change(&tx, &project, &task, &current, &revised)?;
            tx.commit()?;
            Ok(ChecklistProgressReceipt::from(&revised))
        })
        .await
    }

    pub async fn set_pipeline_checklist_item_checked(
        &self,
        project: String,
        pipeline_run: String,
        task: String,
        item: String,
        expected_revision: i64,
        checked: bool,
    ) -> Result<ChecklistProgressReceipt> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let receipt = set_pipeline_checklist_item_checked_in_transaction(
                &tx,
                &project,
                &pipeline_run,
                &task,
                &item,
                expected_revision,
                checked,
            )?;
            tx.commit()?;
            Ok(receipt)
        })
        .await
    }

    pub async fn run_checklist_progress(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<ChecklistProgressAccess>> {
        self.execute(move |db| {
            let Some(scope) = load_run_scope(db, &run, Some(&project))? else {
                return Ok(None);
            };
            let availability = if !scope.active {
                ChecklistProgressAvailability::Inactive
            } else if scope.dispatch_id.is_none() {
                ChecklistProgressAvailability::PendingDispatch
            } else {
                match scope.bridge.as_deref() {
                    Some("unavailable" | "blocked") => {
                        ChecklistProgressAvailability::BridgeUnavailable
                    }
                    Some("observed" | "verified") => ChecklistProgressAvailability::Ready,
                    _ => ChecklistProgressAvailability::BridgeUnconfirmed,
                }
            };
            Ok(Some(ChecklistProgressAccess {
                task_id: scope.task_id,
                mode: scope.mode,
                selected_item_id: scope.selected,
                availability,
                items: scope.items,
            }))
        })
        .await
    }

    pub async fn checklist_progress_history(
        &self,
        project: String,
        task: String,
        item: String,
    ) -> Result<Vec<ChecklistProgressRecord>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT sequence, actor, run_id, dispatch_id, request_id, before_revision,
                        after_revision, checked, recorded_at
                 FROM checklist_progress_events
                 WHERE project_id = ?1 AND task_id = ?2 AND item_id = ?3 ORDER BY sequence",
            )?;
            Ok(statement
                .query_map(params![project, task, item], |row| {
                    let actor = match row.get::<_, String>(1)?.as_str() {
                        "agent" => ChecklistProgressActor::Agent {
                            run_id: row.get(2)?,
                            dispatch_id: row.get(3)?,
                            request_id: row.get(4)?,
                        },
                        _ => ChecklistProgressActor::Operator,
                    };
                    Ok(ChecklistProgressRecord {
                        sequence: row.get::<_, i64>(0)?.to_string(),
                        actor,
                        before_revision: row.get(5)?,
                        after: ChecklistProgressReceipt {
                            item_id: item.clone(),
                            revision: row.get(6)?,
                            checked: row.get(7)?,
                        },
                        recorded_at: row.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub(crate) async fn record_agent_checklist_progress(
        &self,
        run: String,
        request: ChecklistProgressRequest,
    ) -> Result<ChecklistProgressReceipt> {
        ensure!(
            !request.request_id.trim().is_empty() && request.request_id.len() <= 256,
            "invalid checklist progress request ID"
        );
        ensure!(
            request.expected_revision > 0,
            "invalid expected checklist revision"
        );
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let scope =
                load_run_scope(&tx, &run, None)?.ok_or(ChecklistProgressError::OutsideScope)?;
            ensure!(scope.active, ChecklistProgressError::Inactive);
            let dispatch_id = scope.dispatch_id.ok_or(ChecklistProgressError::Inactive)?;
            let prior = tx
                .query_row(
                    "SELECT item_id, before_revision, after_revision, checked
                 FROM checklist_progress_events WHERE run_id = ?1 AND request_id = ?2",
                    params![run, request.request_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((item_id, before, revision, checked)) = prior {
                ensure!(
                    item_id == request.item_id
                        && before == request.expected_revision
                        && checked == request.checked,
                    ChecklistProgressError::RequestConflict
                );
                return Ok(ChecklistProgressReceipt {
                    item_id,
                    revision,
                    checked,
                });
            }

            let allowed = match scope.mode {
                ChecklistExecutionMode::TrackProgress => scope.selected.is_none(),
                ChecklistExecutionMode::RunByItem => {
                    scope.selected.as_deref() == Some(request.item_id.as_str())
                }
            };
            ensure!(
                allowed
                    && scope
                        .items
                        .iter()
                        .any(|item| item.item_id == request.item_id),
                ChecklistProgressError::OutsideScope
            );
            let current = item_at_expected_revision(
                &tx,
                &scope.project_id,
                &scope.task_id,
                &request.item_id,
                request.expected_revision,
            )?;
            let origin = agent_work_origin(&tx, &scope.project_id, &run)?;
            let revised = revise_checklist_item(
                &tx,
                &scope.project_id,
                &scope.task_id,
                &current,
                current.body.clone(),
                request.checked,
                &origin,
            )?;
            record_progress(
                &tx,
                &scope.project_id,
                &scope.task_id,
                &current,
                &revised,
                ChecklistProgressActor::Agent {
                    run_id: run,
                    dispatch_id,
                    request_id: request.request_id,
                },
            )?;
            tx.commit()?;
            Ok(ChecklistProgressReceipt::from(&revised))
        })
        .await
    }
}

fn load_run_scope(
    db: &Connection,
    run: &str,
    project: Option<&str>,
) -> Result<Option<RunChecklistScope>> {
    let row = db.query_row(
        "SELECT work.project_id, work.task_id, modes.mode, work.checklist_item_id,
                (live.run_id IS NOT NULL AND r.status IN ('starting', 'running', 'waiting_for_input') AND attempts.cancel_requested = 0),
                json_extract(summary.summary, '$.protocol.run_scoped_mcp'), dispatch.id,
                CASE WHEN dispatch.id IS NOT NULL THEN snapshot.payload END
         FROM job_attempts attempts
         JOIN job_task_run_scopes work ON work.job_id = attempts.job_id
         JOIN job_checklist_modes modes ON modes.job_id = work.job_id
         JOIN runs r ON r.id = attempts.run_id
         JOIN checkouts c ON c.id = r.checkout_id AND c.project_id = work.project_id
         LEFT JOIN active_agent_attempts live ON live.run_id = r.id
         LEFT JOIN agent_capability_summaries summary ON summary.run_id = r.id
         LEFT JOIN context_snapshots snapshot ON snapshot.run_id = r.id
         LEFT JOIN dispatches dispatch ON dispatch.snapshot_id = snapshot.id AND dispatch.ordinal = 1
            AND dispatch.delivery = 'submitted' AND r.status <> 'starting'
         WHERE r.id = ?1 AND (?2 IS NULL OR work.project_id = ?2)",
        params![run, project],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?, row.get::<_, bool>(4)?, row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?, row.get::<_, Option<Vec<u8>>>(7)?)),
    ).optional()?;
    let Some((project_id, task_id, mode, selected, active, bridge, dispatch_id, payload)) = row
    else {
        return Ok(None);
    };
    let mode = ChecklistExecutionMode::from_sql(&mode)?;
    let items = if let Some(payload) = payload {
        let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        let context = snapshot
            .task_context
            .ok_or(ChecklistProgressError::OutsideScope)?;
        ensure!(
            context.task_id == task_id
                && context.checklist_mode == Some(mode)
                && context.checklist_item_id == selected,
            ChecklistProgressError::OutsideScope
        );
        context
            .checklist
            .iter()
            .map(ChecklistProgressReceipt::from)
            .collect()
    } else {
        Vec::new()
    };
    Ok(Some(RunChecklistScope {
        project_id,
        task_id,
        mode,
        selected,
        active,
        bridge,
        dispatch_id,
        items,
    }))
}

pub(super) fn item_at_expected_revision(
    db: &Connection,
    project: &str,
    task: &str,
    item: &str,
    expected_revision: i64,
) -> Result<ChecklistItem> {
    let current = load_checklist_items(db, project, task, Some(item), None)?
        .pop()
        .ok_or(ChecklistProgressError::OutsideScope)?;
    ensure!(
        current.revision == expected_revision,
        ChecklistProgressError::RevisionConflict {
            current_revision: current.revision
        }
    );
    Ok(current)
}

pub(crate) fn set_pipeline_checklist_item_checked_in_transaction(
    db: &Connection,
    project: &str,
    pipeline_run: &str,
    task: &str,
    item: &str,
    expected_revision: i64,
    checked: bool,
) -> Result<ChecklistProgressReceipt> {
    let origin = pipeline_work_origin(db, project, pipeline_run)?;
    crate::workflows::require_launch_checklist_item(db, project, pipeline_run, task, Some(item))?;
    let current = item_at_expected_revision(db, project, task, item, expected_revision)?;
    let revised = revise_checklist_item(
        db,
        project,
        task,
        &current,
        current.body.clone(),
        checked,
        &origin,
    )?;
    Ok(ChecklistProgressReceipt::from(&revised))
}

pub(super) fn record_operator_check_change(
    db: &Connection,
    project: &str,
    task: &str,
    before: &ChecklistItem,
    after: &ChecklistItem,
) -> Result<()> {
    if before.checked != after.checked {
        record_progress(
            db,
            project,
            task,
            before,
            after,
            ChecklistProgressActor::Operator,
        )?;
    }
    Ok(())
}

fn record_progress(
    db: &Connection,
    project: &str,
    task: &str,
    before: &ChecklistItem,
    after: &ChecklistItem,
    actor: ChecklistProgressActor,
) -> Result<()> {
    let (actor, run, dispatch, request) = match actor {
        ChecklistProgressActor::Operator => ("operator", None, None, None),
        ChecklistProgressActor::Agent {
            run_id,
            dispatch_id,
            request_id,
        } => ("agent", Some(run_id), Some(dispatch_id), Some(request_id)),
    };
    db.execute(
        "INSERT INTO checklist_progress_events
         (project_id, task_id, item_id, before_revision, after_revision, checked, actor, run_id, dispatch_id, request_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![project, task, before.id, before.revision, after.revision, after.checked, actor, run, dispatch, request],
    )?;
    Ok(())
}
