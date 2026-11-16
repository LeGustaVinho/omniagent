use anyhow::{Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};

use crate::{
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

impl Store {
    pub async fn delete_conversation_side_question(
        &self,
        conversation: String,
        job: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            // Keep the job identity for late workers. Setting discarded requests a live stop.
            let discarded = tx.execute(
                "UPDATE job_side_requests SET discarded = 1, payload = X''
                 WHERE conversation_id = ?1 AND job_id = ?2 AND purpose = 'question' AND NOT validation",
                params![conversation, job],
            )?;
            ensure!(discarded == 1, "side question is outside the selected conversation");
            tx.execute(
                "DELETE FROM job_side_responses WHERE run_id IN (
                     SELECT run_id FROM job_attempts WHERE job_id = ?1
                 )",
                [&job],
            )?;
            tx.execute("DELETE FROM job_recoveries WHERE job_id = ?1", [&job])?;
            tx.execute("UPDATE jobs SET queue_state = 'cancelled' WHERE id = ?1", [&job])?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        self.event_signal.send_replace(());
        Ok(())
    }
}

pub(super) fn settle_deleted_question(db: &Connection, run: &RunRecord) -> Result<bool> {
    let job: String = db.query_row(
        "SELECT job_id FROM job_attempts WHERE run_id = ?1",
        [&run.id],
        |row| row.get(0),
    )?;
    let deleted = super::is_deleted_question(db, &job)?;
    if deleted && !run.status.is_terminal() {
        runs::advance_run(db, run, RunStatus::Cancelled)?;
    }
    Ok(deleted)
}
