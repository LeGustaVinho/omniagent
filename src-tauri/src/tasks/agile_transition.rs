use anyhow::{Result, ensure};
use rusqlite::{Connection, TransactionBehavior};

use crate::store::Store;

use super::{
    AgileEventOrigin, TaskDeliveryState, TaskWorkflowState, WorkStateChanges,
    activity::store_requested_workflow_state,
    delivery::{
        cancel_delivery, complete_manual_delivery, is_terminal, load_delivery_state,
        reopen_delivery,
    },
    pipeline_work_origin,
};

#[cfg(test)]
mod tests;

impl Store {
    pub async fn transition_project_work(
        &self,
        project: String,
        task: String,
        expected: TaskWorkflowState,
        next: TaskWorkflowState,
    ) -> Result<TaskDeliveryState> {
        self.transition_work(project, task, expected, next, None)
            .await
    }

    pub async fn transition_pipeline_work(
        &self,
        project: String,
        pipeline_run: String,
        task: String,
        expected: TaskWorkflowState,
        next: TaskWorkflowState,
    ) -> Result<TaskDeliveryState> {
        self.transition_work(project, task, expected, next, Some(pipeline_run))
            .await
    }

    async fn transition_work(
        &self,
        project: String,
        task: String,
        expected: TaskWorkflowState,
        next: TaskWorkflowState,
        pipeline_run: Option<String>,
    ) -> Result<TaskDeliveryState> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let origin = pipeline_run
                .as_deref()
                .map(|run| pipeline_work_origin(&tx, &project, run))
                .transpose()?
                .unwrap_or(AgileEventOrigin::Operator);
            let updated =
                transition_work_in_transaction(&tx, &project, &task, expected, next, &origin)?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }
}

pub(crate) fn current_work_state(
    db: &Connection,
    project: &str,
    task: &str,
) -> Result<TaskWorkflowState> {
    Ok(load_delivery_state(db, project, task)?.state)
}

pub(crate) fn transition_work_in_transaction(
    tx: &Connection,
    project: &str,
    task: &str,
    expected: TaskWorkflowState,
    next: TaskWorkflowState,
    origin: &AgileEventOrigin,
) -> Result<TaskDeliveryState> {
    let current = load_delivery_state(tx, project, task)?;
    ensure!(
        current.state == expected,
        "The work state changed. Refresh its current state and try again."
    );
    if current.state == next {
        return Ok(current);
    }
    let states = WorkStateChanges::capture(tx, project, task)?;

    if is_terminal(current.state) {
        ensure!(
            next == TaskWorkflowState::Ready,
            "Reopen closed work to Ready before moving it again."
        );
        reopen_delivery(tx, project, task)?;
        store_requested_workflow_state(tx, project, task, next)?;
    } else {
        match next {
            TaskWorkflowState::Completed => {
                complete_manual_delivery(tx, project, task, None)?;
            }
            TaskWorkflowState::Cancelled => cancel_delivery(tx, project, task)?,
            _ => {
                ensure!(
                    current.prerequisite_blockers.is_empty(),
                    "Complete the task dependencies first."
                );
                ensure!(
                    current.question_blocker.is_none(),
                    "Answer the task's pending question first."
                );
                ensure!(
                    !current.waiting_for_commit,
                    "Verify the pending commit before moving this task."
                );
                ensure!(
                    !current.waiting_for_merge,
                    "Verify the pending merge before moving this task."
                );
                store_requested_workflow_state(tx, project, task, next)?;
            }
        }
    }

    let updated = load_delivery_state(tx, project, task)?;
    ensure!(
        updated.state == next,
        "The work state is controlled by its child tasks or pending delivery evidence."
    );
    states.publish(tx, project, origin)?;
    Ok(updated)
}
