use anyhow::{Result, ensure};
use rusqlite::TransactionBehavior;

use crate::store::Store;

use super::{
    TaskDeliveryState, TaskWorkflowState,
    activity::store_requested_workflow_state,
    delivery::{
        cancel_delivery, complete_manual_delivery, is_terminal, load_delivery_state,
        reopen_delivery,
    },
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
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_delivery_state(&tx, &project, &task)?;
            ensure!(
                current.state == expected,
                "The work state changed. Refresh its current state and try again."
            );
            if current.state == next {
                return Ok(current);
            }

            if is_terminal(current.state) {
                ensure!(
                    next == TaskWorkflowState::Ready,
                    "Reopen closed work to Ready before moving it again."
                );
                reopen_delivery(&tx, &project, &task)?;
                store_requested_workflow_state(&tx, &project, &task, next)?;
            } else {
                match next {
                    TaskWorkflowState::Completed => {
                        complete_manual_delivery(&tx, &project, &task, None)?;
                    }
                    TaskWorkflowState::Cancelled => cancel_delivery(&tx, &project, &task)?,
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
                        store_requested_workflow_state(&tx, &project, &task, next)?;
                    }
                }
            }

            let updated = load_delivery_state(&tx, &project, &task)?;
            ensure!(
                updated.state == next,
                "The work state is controlled by its child tasks or pending delivery evidence."
            );
            tx.commit()?;
            Ok(updated)
        })
        .await
    }
}
