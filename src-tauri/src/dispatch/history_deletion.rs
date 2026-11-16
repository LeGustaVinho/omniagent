use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    conversations::MessageRevision, protection, repository::AttemptWorktreeEvidence, store::Store,
};

mod apply;
mod plan;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HistoryDeletionTarget {
    Message {
        conversation_id: String,
        message_revision_id: String,
    },
    AttemptDiff {
        conversation_id: String,
        run_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryDeletionMode {
    Selected,
    WithDerivedCopies,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDeletionPreview {
    pub revision: String,
    pub message_revision_ids: BTreeSet<String>,
    pub artifact_ids: BTreeSet<String>,
    pub dispatch_ids: BTreeSet<String>,
    pub reference_ids: BTreeSet<String>,
    pub attempt_diff_ids: BTreeSet<String>,
    pub candidate_ids: BTreeSet<String>,
    pub native_sessions: usize,
    pub retained_runs: usize,
    pub affected_jobs: usize,
    pub side_request_ids: BTreeSet<String>,
    pub unsettled_run_ids: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDeletionDiff {
    pub run_id: String,
    pub label: String,
    pub deleted: bool,
}

#[derive(Serialize)]
struct DeletionPlan {
    target: HistoryDeletionTarget,
    mode: HistoryDeletionMode,
    target_already_deleted: bool,
    revisions: BTreeSet<String>,
    artifacts: BTreeSet<String>,
    dispatches: BTreeSet<String>,
    synthetic_input_runs: BTreeSet<String>,
    answer_runs: BTreeSet<String>,
    references: BTreeSet<String>,
    jobs: BTreeSet<String>,
    summary_requests: BTreeSet<String>,
    side_requests: BTreeSet<String>,
    diffs: BTreeSet<String>,
    diff_revision: Option<String>,
    candidates: BTreeSet<String>,
    sessions: BTreeSet<String>,
    retained_runs: BTreeSet<String>,
    unsettled: BTreeSet<String>,
}

impl DeletionPlan {
    fn new(target: HistoryDeletionTarget, mode: HistoryDeletionMode) -> Self {
        Self {
            target,
            mode,
            target_already_deleted: false,
            revisions: BTreeSet::new(),
            artifacts: BTreeSet::new(),
            dispatches: BTreeSet::new(),
            synthetic_input_runs: BTreeSet::new(),
            answer_runs: BTreeSet::new(),
            references: BTreeSet::new(),
            jobs: BTreeSet::new(),
            summary_requests: BTreeSet::new(),
            side_requests: BTreeSet::new(),
            diffs: BTreeSet::new(),
            diff_revision: None,
            candidates: BTreeSet::new(),
            sessions: BTreeSet::new(),
            retained_runs: BTreeSet::new(),
            unsettled: BTreeSet::new(),
        }
    }

    fn preview(&self) -> Result<HistoryDeletionPreview> {
        Ok(HistoryDeletionPreview {
            revision: format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)),
            message_revision_ids: self.revisions.clone(),
            artifact_ids: self.artifacts.clone(),
            dispatch_ids: self.dispatches.clone(),
            reference_ids: self.references.clone(),
            attempt_diff_ids: self.diffs.clone(),
            candidate_ids: self.candidates.clone(),
            native_sessions: self.sessions.len(),
            retained_runs: self.retained_runs.len(),
            affected_jobs: self.jobs.len(),
            side_request_ids: self.side_requests.clone(),
            unsettled_run_ids: self.unsettled.clone(),
        })
    }
}

impl Store {
    pub async fn conversation_history_deletion_diffs(
        &self,
        conversation_id: String,
    ) -> Result<Vec<HistoryDeletionDiff>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT a.run_id, a.ordinal, c.evidence,
                        EXISTS(SELECT 1 FROM history_deleted_attempt_diffs WHERE run_id = a.run_id)
                 FROM agent_attempts a JOIN job_worktree_candidates c ON c.run_id = a.run_id
                 WHERE a.conversation_id = ?1 ORDER BY a.ordinal DESC",
            )?;
            let mut rows = statement.query([conversation_id])?;
            let mut diffs = Vec::new();
            while let Some(row) = rows.next()? {
                let deleted: bool = row.get(3)?;
                if deleted
                    || matches!(
                        unseal::<AttemptWorktreeEvidence>(&row.get::<_, Vec<u8>>(2)?)?,
                        AttemptWorktreeEvidence::Saved { .. }
                    )
                {
                    diffs.push(HistoryDeletionDiff {
                        run_id: row.get(0)?,
                        label: format!("Attempt {}", row.get::<_, u32>(1)?),
                        deleted,
                    });
                }
            }
            Ok(diffs)
        })
        .await
    }

    pub async fn preview_conversation_history_deletion(
        &self,
        target: HistoryDeletionTarget,
        mode: HistoryDeletionMode,
    ) -> Result<HistoryDeletionPreview> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            plan::build(&tx, target, mode)?.preview()
        })
        .await
    }

    pub async fn confirm_conversation_history_deletion(
        &self,
        target: HistoryDeletionTarget,
        mode: HistoryDeletionMode,
        expected_revision: String,
    ) -> Result<HistoryDeletionPreview> {
        let preview = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let plan = plan::build(&tx, target, mode)?;
                let preview = plan.preview()?;
                ensure!(
                    preview.revision == expected_revision,
                    "History deletion impact changed. Review it again."
                );
                ensure!(
                    plan.unsettled.is_empty(),
                    "Settle affected attempts before deleting history"
                );
                apply::apply(&tx, &plan)?;
                tx.commit()?;
                Ok(preview)
            })
            .await?;
        self.job_signal.send_replace(());
        self.event_signal.send_replace(());
        Ok(preview)
    }
}

pub(crate) fn omit_deleted_dispatch_messages(
    db: &Connection,
    input: Vec<MessageRevision>,
) -> Result<Vec<MessageRevision>> {
    let mut deleted = db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM history_deleted_revisions WHERE revision_id = ?1)",
    )?;
    let mut retained = Vec::with_capacity(input.len());
    for message in input {
        if !deleted.query_row([&message.revision_id], |row| row.get::<_, bool>(0))? {
            retained.push(message);
        }
    }
    Ok(retained)
}

fn ids(db: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<BTreeSet<String>> {
    let mut statement = db.prepare(sql)?;
    Ok(statement
        .query_map(args, |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn unseal<T: DeserializeOwned>(value: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(value)?)?)
}
