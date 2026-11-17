use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    jobs::saved_checkpoint_is_current,
    protection,
    providers::{ProviderEvent, ProviderEventData, RequestId},
    repository::verified_merge::{VerifiedMerge, load_verified_delivery_merge},
    store::Store,
    workflows::{
        active_delivery_workflow, detach_active_delivery_workflow, waiting_for_delivery_commit,
    },
};

use self::milestone::{DeliveryCompletionMilestone, PendingTaskDeliveryMerge, pending_merge};
use super::{
    AgileEventOrigin, TaskWorkflowState, WorkStateChanges, activity::load_task_details,
    load_project_task,
};

pub(crate) mod milestone;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum TaskDeliveryEvidence {
    Manual {
        note: Option<String>,
    },
    VerifiedCommit {
        workflow_run_id: String,
        plan_revision: String,
        unit_evidence_ids: Vec<String>,
        completion_sha: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDelivery {
    pub id: String,
    pub recorded_at: String,
    pub evidence: TaskDeliveryEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge: Option<VerifiedMerge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDeliveryState {
    pub state: TaskWorkflowState,
    pub waiting_for_commit: bool,
    pub waiting_for_merge: bool,
    pub pending_merge: Option<PendingTaskDeliveryMerge>,
    pub pending_repository_changes: bool,
    pub checklist_resolved: bool,
    pub has_direct_work: bool,
    pub active_workflow_run_id: Option<String>,
    pub question_blocker: Option<TaskQuestionBlocker>,
    pub completion: Option<TaskDelivery>,
    pub prerequisite_blockers: Vec<TaskPrerequisiteBlocker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPrerequisiteBlocker {
    pub task_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskQuestionBlocker {
    pub run_id: String,
    pub conversation_id: String,
    pub request_id: RequestId,
    pub state: String,
    pub answer_recorded: bool,
}

impl Store {
    pub async fn project_task_delivery_state(
        &self,
        project: String,
        task: String,
    ) -> Result<Option<TaskDeliveryState>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            load_project_task(&tx, &project, &task)?
                .map(|_| load_delivery_state(&tx, &project, &task))
                .transpose()
        })
        .await
    }

    pub async fn project_epic_delivery_state(
        &self,
        project: String,
        epic: String,
    ) -> Result<Option<TaskWorkflowState>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let exists = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM project_epics WHERE project_id = ?1 AND id = ?2)",
                params![project, epic],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let children = child_states(&tx, &project, &epic, true)?;
            Ok(Some(
                group_state(&children, false, false).unwrap_or_default(),
            ))
        })
        .await
    }

    pub async fn project_task_deliveries(
        &self,
        project: String,
        task: String,
    ) -> Result<Vec<TaskDelivery>> {
        self.execute(move |db| {
            let records = db
                .prepare(
                    "SELECT id, recorded_at, payload FROM task_delivery_events
                 WHERE project_id = ?1 AND task_id = ?2 AND payload IS NOT NULL ORDER BY sequence",
                )?
                .query_map(params![project, task], read_delivery)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
                .into_iter()
                .map(|receipt| decode_delivery(db, &project, &task, receipt))
                .collect()
        })
        .await
    }

    pub async fn record_manual_task_delivery(
        &self,
        project: String,
        task: String,
        note: Option<String>,
    ) -> Result<TaskDelivery> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let states = WorkStateChanges::capture(&tx, &project, &task)?;
            let receipt = complete_manual_delivery(&tx, &project, &task, note)?;
            states.publish(&tx, &project, &AgileEventOrigin::Operator)?;
            tx.commit()?;
            Ok(receipt)
        })
        .await
    }

    pub async fn reopen_task_delivery(&self, project: String, task: String) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let states = WorkStateChanges::capture(&tx, &project, &task)?;
            reopen_delivery(&tx, &project, &task)?;
            states.publish(&tx, &project, &AgileEventOrigin::Operator)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn cancel_task_delivery(&self, project: String, task: String) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let states = WorkStateChanges::capture(&tx, &project, &task)?;
            cancel_delivery(&tx, &project, &task)?;
            states.publish(&tx, &project, &AgileEventOrigin::Operator)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub(super) fn complete_manual_delivery(
    db: &Connection,
    project: &str,
    task: &str,
    note: Option<String>,
) -> Result<TaskDelivery> {
    validate_repository_delivery_open(db, project, task)?;
    let requires_verified_merge: bool = db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM workflow_backlog_task_runs task
            JOIN workflow_run_pins child ON child.pipeline_run_id = task.child_run_id
            WHERE child.project_id = ?1 AND task.task_id = ?2
        )",
        params![project, task],
        |row| row.get(0),
    )?;
    ensure!(
        !requires_verified_merge,
        "This backlog task requires a verified merge to complete"
    );
    let state = load_delivery_state(db, project, task)?;
    ensure!(
        state.active_workflow_run_id.is_none(),
        "repository work requires verified commit evidence"
    );
    ensure!(
        state.question_blocker.is_none(),
        "answer the task's pending question before completing"
    );
    ensure!(
        state.prerequisite_blockers.is_empty(),
        "complete the task dependencies first"
    );
    let pending = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_worktree_changes
             WHERE project_id = ?1 AND scope_task_id = ?2)
         OR EXISTS(SELECT 1 FROM app_worktree_writers
             WHERE project_id = ?1 AND scope_task_id = ?2)
         OR EXISTS(SELECT 1 FROM job_writer_reservations r
             JOIN job_attempts a ON a.run_id = r.run_id
             JOIN jobs j ON j.id = a.job_id
             LEFT JOIN job_worktree_scopes s ON s.job_id = j.id
             WHERE j.project_id = ?1 AND coalesce(s.scope_task_id, j.task_id) = ?2)",
        params![project, task],
        |row| row.get::<_, bool>(0),
    )?;
    ensure!(
        !pending,
        "repository work requires verified commit or explicit discard"
    );
    ensure!(
        state.checklist_resolved,
        "resolve the task checklist before completing"
    );
    insert_delivery(db, project, task, TaskDeliveryEvidence::Manual { note })
}

pub(super) fn reopen_delivery(db: &Connection, project: &str, task: &str) -> Result<()> {
    let state = load_delivery_state(db, project, task)?;
    ensure!(
        state.has_direct_work || child_states(db, project, task, false)?.is_empty(),
        "reopen a child to reopen a grouping task"
    );
    detach_active_delivery_workflow(db, project, task)?;
    db.execute(
        "UPDATE project_task_details SET workflow_state = 'ready'
         WHERE project_id = ?1 AND task_id = ?2 AND workflow_state IN ('completed', 'cancelled')",
        params![project, task],
    )?;
    insert_transition(db, project, task, "reopened")
}

pub(super) fn cancel_delivery(db: &Connection, project: &str, task: &str) -> Result<()> {
    let state = load_delivery_state(db, project, task)?;
    ensure!(
        state.state != TaskWorkflowState::Completed,
        "reopen the task before cancelling it"
    );
    if state.state == TaskWorkflowState::Cancelled {
        return Ok(());
    }
    ensure!(
        child_states(db, project, task, false)?
            .into_iter()
            .all(is_terminal),
        "complete or cancel the child tasks first"
    );
    detach_active_delivery_workflow(db, project, task)?;
    insert_transition(db, project, task, "cancelled")
}

pub(crate) fn validate_repository_delivery_open(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<()> {
    let state = load_delivery_state(db, project, task)?;
    let has_receipt =
        latest_event(db, project, task)?.is_some_and(|(_, _, _, payload)| payload.is_some());
    ensure!(
        !has_receipt && !is_terminal(state.state),
        "reopen the task before starting another delivery"
    );
    Ok(())
}

pub(super) fn load_delivery_state(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<TaskDeliveryState> {
    let mut state = load_delivery_core(db, project, task)?;
    if !is_terminal(state.state) {
        let dependencies = db.prepare(
            "SELECT task.id, task.title FROM project_task_prerequisites prereq
             JOIN project_tasks task ON task.project_id = prereq.project_id AND task.id = prereq.prerequisite_id
             WHERE prereq.project_id = ?1 AND prereq.task_id = ?2 ORDER BY task.title, task.id"
        )?.query_map(params![project, task], |row| Ok(TaskPrerequisiteBlocker { task_id: row.get(0)?, title: row.get(1)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // A prerequisite may be another grouping task. Its completion is calculated without
        // traversing further prerequisite links, which can intersect the child hierarchy.
        for dependency in dependencies {
            if load_delivery_core(db, project, &dependency.task_id)?.state
                != TaskWorkflowState::Completed
            {
                state.prerequisite_blockers.push(dependency);
            }
        }
        if !state.prerequisite_blockers.is_empty() {
            state.state = TaskWorkflowState::Blocked;
        }
    }
    Ok(state)
}

fn load_delivery_core(db: &Connection, project: &str, task: &str) -> Result<TaskDeliveryState> {
    ensure!(
        load_project_task(db, project, task)?.is_some(),
        "task must belong to this project"
    );
    let base = load_task_details(db, project, task)?.workflow_state;
    let (has_checklist, checklist_resolved) = checklist_status(db, project, task)?;
    let active_workflow_run_id = active_delivery_workflow(db, project, task)?;
    let waiting_for_commit =
        active_workflow_run_id.is_some() && waiting_for_delivery_commit(db, project, task)?;
    let merge_candidate = active_workflow_run_id
        .as_deref()
        .map(|workflow| pending_merge(db, project, task, workflow))
        .transpose()?
        .flatten();
    let waiting_for_merge = merge_candidate
        .as_ref()
        .is_some_and(|(milestone, _)| *milestone == DeliveryCompletionMilestone::VerifiedMerge);
    let pending_merge = merge_candidate.map(|(_, candidate)| candidate);
    let pending_repository_changes: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_worktree_changes
         WHERE project_id = ?1 AND scope_task_id = ?2)",
        params![project, task],
        |row| row.get(0),
    )?;
    let latest = latest_event(db, project, task)?;
    let has_delivery_history = latest.is_some();
    let cancelled = latest
        .as_ref()
        .is_some_and(|(kind, ..)| kind == "cancelled")
        || (!has_delivery_history && base == TaskWorkflowState::Cancelled);
    let awaiting_completion = latest
        .as_ref()
        .is_some_and(|(_, _, _, payload)| payload.is_some())
        && (!checklist_resolved || pending_repository_changes);
    let completion = latest
        .and_then(|(_, id, at, payload)| payload.map(|payload| (id, at, payload)))
        .map(|receipt| decode_delivery(db, project, task, receipt))
        .transpose()?
        .filter(|_| checklist_resolved && !pending_repository_changes);
    let children = child_states(db, project, task, false)?;
    let had_direct_delivery = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_delivery_events
             WHERE project_id = ?1 AND task_id = ?2 AND payload IS NOT NULL)",
        params![project, task],
        |row| row.get::<_, bool>(0),
    )?;
    let has_direct_work = has_checklist || had_direct_delivery || active_workflow_run_id.is_some();
    let legacy_completion =
        !has_delivery_history && checklist_resolved && base == TaskWorkflowState::Completed;
    let direct_delivered = completion.is_some() || legacy_completion;
    let question_blocker = if cancelled || direct_delivered {
        None
    } else {
        open_task_question(db, project, task)?
    };
    let state = if cancelled {
        TaskWorkflowState::Cancelled
    } else if children.is_empty() && direct_delivered {
        TaskWorkflowState::Completed
    } else if question_blocker.is_some() {
        TaskWorkflowState::Blocked
    } else if waiting_for_commit || waiting_for_merge || awaiting_completion {
        TaskWorkflowState::InReview
    } else {
        group_state(
            &children,
            has_direct_work || legacy_completion,
            direct_delivered,
        )
        .unwrap_or_else(|| match base {
            TaskWorkflowState::Completed | TaskWorkflowState::Cancelled => TaskWorkflowState::Ready,
            TaskWorkflowState::Backlog if direct_delivered => TaskWorkflowState::InProgress,
            other => other,
        })
    };
    Ok(TaskDeliveryState {
        state,
        waiting_for_commit,
        waiting_for_merge,
        pending_merge,
        pending_repository_changes,
        checklist_resolved,
        has_direct_work,
        active_workflow_run_id,
        question_blocker,
        completion,
        prerequisite_blockers: Vec::new(),
    })
}

fn open_task_question(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<Option<TaskQuestionBlocker>> {
    let question = db.query_row(
        "SELECT waiting.run_id, waiting.conversation_id, waiting.request_event_seq, waiting.state,
                answer.request_event_seq IS NOT NULL, event.payload
         FROM task_runs work
         JOIN open_agent_questions waiting ON waiting.run_id = work.run_id
         JOIN agent_events event ON event.seq = waiting.request_event_seq
         LEFT JOIN recorded_question_answers answer ON answer.request_event_seq = waiting.request_event_seq
         WHERE work.project_id = ?1 AND work.task_id = ?2
           AND work.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = work.job_id)
         ORDER BY answer.request_event_seq IS NOT NULL, waiting.request_event_seq LIMIT 1",
        params![project, task],
        |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?, row.get::<_, bool>(4)?, row.get::<_, Vec<u8>>(5)?,
        )),
    ).optional()?;
    question
        .map(
            |(run_id, conversation_id, sequence, mut state, answer_recorded, payload)| {
                if state == "saved" && !saved_checkpoint_is_current(db, &run_id, sequence)? {
                    state = "needs_review".into();
                }
                let event: ProviderEvent =
                    serde_json::from_slice(&protection::unprotect(&payload)?)?;
                let ProviderEventData::Question { request_id, .. } = event.data else {
                    anyhow::bail!("The saved task blocker is missing its originating question");
                };
                Ok(TaskQuestionBlocker {
                    run_id,
                    conversation_id,
                    request_id,
                    state,
                    answer_recorded,
                })
            },
        )
        .transpose()
}

type DeliveryEvent = (String, String, String, Option<Vec<u8>>);

fn latest_event(db: &Connection, project: &str, task: &str) -> Result<Option<DeliveryEvent>> {
    Ok(db
        .query_row(
            "SELECT kind, id, recorded_at, payload FROM task_delivery_events
         WHERE project_id = ?1 AND task_id = ?2 ORDER BY sequence DESC LIMIT 1",
            params![project, task],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?)
}

pub(super) fn is_terminal(state: TaskWorkflowState) -> bool {
    matches!(
        state,
        TaskWorkflowState::Completed | TaskWorkflowState::Cancelled
    )
}

fn group_state(
    children: &[TaskWorkflowState],
    has_direct_work: bool,
    direct_delivered: bool,
) -> Option<TaskWorkflowState> {
    if children.is_empty()
        || (has_direct_work && !direct_delivered)
        || children.iter().any(|&state| !is_terminal(state))
    {
        return None;
    }
    Some(
        if direct_delivered || children.contains(&TaskWorkflowState::Completed) {
            TaskWorkflowState::Completed
        } else {
            TaskWorkflowState::Cancelled
        },
    )
}

fn child_states(
    db: &Connection,
    project: &str,
    parent: &str,
    epic: bool,
) -> Result<Vec<TaskWorkflowState>> {
    let owners = db.prepare(if epic {
        "SELECT task_id FROM project_task_memberships WHERE project_id = ?1 AND epic_id = ?2"
    } else {
        "SELECT task_id FROM project_task_memberships WHERE project_id = ?1 AND parent_task_id = ?2"
    })?.query_map(params![project, parent], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    owners
        .into_iter()
        .map(|task| Ok(load_delivery_core(db, project, &task)?.state))
        .collect()
}

fn checklist_status(db: &Connection, project: &str, task: &str) -> Result<(bool, bool)> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_checklists WHERE project_id = ?1 AND task_id = ?2),
                NOT EXISTS(
                    SELECT 1 FROM project_checklist_items i
                    JOIN project_checklist_item_revisions r
                      ON r.project_id = i.project_id AND r.task_id = i.task_id AND r.item_id = i.id
                    WHERE i.project_id = ?1 AND i.task_id = ?2 AND r.checked = 0 AND r.revision = (
                        SELECT max(v.revision) FROM project_checklist_item_revisions v
                        WHERE v.project_id = i.project_id AND v.task_id = i.task_id AND v.item_id = i.id
                    )
                )",
        params![project, task],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

fn insert_delivery(
    db: &Connection,
    project: &str,
    task: &str,
    evidence: TaskDeliveryEvidence,
) -> Result<TaskDelivery> {
    let id = Uuid::new_v4().to_string();
    let (kind, workflow_run, plan_revision) = match &evidence {
        TaskDeliveryEvidence::Manual { .. } => ("manual", None, None),
        TaskDeliveryEvidence::VerifiedCommit {
            workflow_run_id,
            plan_revision,
            ..
        } => (
            "verified_commit",
            Some(workflow_run_id),
            Some(plan_revision),
        ),
    };
    let protected = protection::protect(&serde_json::to_vec(&evidence)?)?;
    let recorded_at = db.query_row(
        "INSERT INTO task_delivery_events(id, project_id, task_id, kind, workflow_run_id, workflow_plan_revision, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING recorded_at",
        params![id, project, task, kind, workflow_run, plan_revision, protected],
        |row| row.get(0),
    )?;
    Ok(TaskDelivery {
        id,
        recorded_at,
        evidence,
        merge: None,
    })
}

fn insert_transition(db: &Connection, project: &str, task: &str, kind: &str) -> Result<()> {
    db.execute(
        "INSERT INTO task_delivery_events(id, project_id, task_id, kind) VALUES (?1, ?2, ?3, ?4)",
        params![Uuid::new_v4().to_string(), project, task, kind],
    )?;
    Ok(())
}

type ProtectedDelivery = (String, String, Vec<u8>);

fn read_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProtectedDelivery> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

fn decode_delivery(
    db: &Connection,
    project: &str,
    task: &str,
    (id, recorded_at, protected): ProtectedDelivery,
) -> Result<TaskDelivery> {
    let evidence = serde_json::from_slice(&protection::unprotect(&protected)?)?;
    let merge = match &evidence {
        TaskDeliveryEvidence::VerifiedCommit {
            unit_evidence_ids, ..
        } => unit_evidence_ids
            .last()
            .map(|commit| load_verified_delivery_merge(db, project, task, commit))
            .transpose()?
            .flatten(),
        TaskDeliveryEvidence::Manual { .. } => None,
    };
    Ok(TaskDelivery {
        id,
        recorded_at,
        evidence,
        merge,
    })
}
