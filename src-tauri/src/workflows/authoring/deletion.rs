use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior};
use serde::Deserialize;

use crate::jobs::{QueueState, load_job};

use super::{Store, load_scope, publication};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDraftKind {
    Graph,
    Package,
}

impl Store {
    pub async fn delete_workflow_draft(
        &self,
        project: String,
        draft: String,
        kind: WorkflowDraftKind,
        expected_revision: Option<String>,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match kind {
                WorkflowDraftKind::Graph => {
                    let scope = load_scope(&tx, &project, &draft)?;
                    ensure!(scope.head == expected_revision, "The draft changed. Review its latest revision before deleting.");
                    ensure!(publication::load_publication(&tx, &project, &draft)?.is_none(), "Published drafts cannot be deleted");
                    retain_authoring_job_history(&tx, &project, &draft, false)?;
                    tx.execute("DELETE FROM workflow_authoring_revisions WHERE draft_id = ?1", [&draft])?;
                    tx.execute("DELETE FROM workflow_authoring_requests WHERE draft_id = ?1", [&draft])?;
                    tx.execute("DELETE FROM workflow_authoring_drafts WHERE id = ?1", [&draft])?;
                }
                WorkflowDraftKind::Package => super::package::delete_pipeline_draft(&tx, &project, &draft, expected_revision.as_deref())?,
            }
            tx.commit()?;
            Ok(())
        }).await
    }
}

pub(super) fn retain_authoring_job_history(
    db: &Connection,
    project: &str,
    draft: &str,
    package: bool,
) -> Result<()> {
    let mut requests = db.prepare(if package {
        "SELECT job_id FROM workflow_package_requests WHERE draft_id = ?1"
    } else {
        "SELECT job_id FROM workflow_authoring_requests WHERE draft_id = ?1"
    })?;
    for id in requests.query_map([draft], |row| row.get::<_, String>(0))? {
        let id = id?;
        let job = load_job(db, project, &id)?.context("The authoring job is unavailable")?;
        let settled = job.latest_attempt.as_ref().map_or(
            job.queue_state == QueueState::Cancelled,
            |attempt| attempt.status.is_terminal(),
        );
        ensure!(
            settled && job.queued_recovery.is_none() && job.downstream_wait.is_none(),
            "Finish or cancel the active authoring request in Queue before deleting this draft."
        );
        db.execute("INSERT INTO deleted_workflow_authoring_jobs VALUES (?1)", [&id])?;
    }
    Ok(())
}
