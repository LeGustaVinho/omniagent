use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, store::Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRecordKind {
    ProjectConvention,
    ArchitectureDecision,
    TaskCriterion,
    CodeReference,
    Note,
    PriorRunEvidence,
}

impl ContextRecordKind {
    fn key(self) -> &'static str {
        match self {
            Self::ProjectConvention => "project_convention",
            Self::ArchitectureDecision => "architecture_decision",
            Self::TaskCriterion => "task_criterion",
            Self::CodeReference => "code_reference",
            Self::Note => "note",
            Self::PriorRunEvidence => "prior_run_evidence",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReviewStatus {
    Proposed,
    InReview,
    Accepted,
    Rejected,
    Superseded,
}

impl ContextReviewStatus {
    fn key(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::InReview => "in_review",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextReviewDecision {
    Review,
    Accept,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextProvenance {
    Operator,
    Run {
        run_id: String,
    },
    RepositoryMarkdown {
        checkout_id: String,
        revision_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRecordContent {
    pub title: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRecordRevision {
    pub id: String,
    pub record_id: String,
    pub revision: i64,
    pub status: ContextReviewStatus,
    pub content: ContextRecordContent,
    pub provenance: ContextProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReviewEvent {
    pub revision_id: String,
    pub status: ContextReviewStatus,
    pub superseded_by: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRecordHistory {
    pub kind: ContextRecordKind,
    pub task_id: Option<String>,
    pub revisions: Vec<ContextRecordRevision>,
    pub review_events: Vec<ContextReviewEvent>,
}

impl Store {
    pub async fn propose_context_record(
        &self,
        project: String,
        task: Option<String>,
        kind: ContextRecordKind,
        content: ContextRecordContent,
        provenance: ContextProvenance,
    ) -> Result<ContextRecordRevision> {
        let record = Uuid::new_v4().to_string();
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO project_context_records(project_id, id, task_id, kind) VALUES (?1, ?2, ?3, ?4)",
                params![project, record, task, kind.key()],
            ).context("the context record requires a valid project and task scope")?;
            let proposed = ContextRecordRevision {
                id: Uuid::new_v4().to_string(), record_id: record, revision: 1,
                status: ContextReviewStatus::Proposed, content, provenance,
            };
            insert_proposal(&tx, &project, &proposed)?;
            tx.commit()?;
            Ok(proposed)
        }).await
    }

    pub async fn propose_context_revision(
        &self,
        project: String,
        record: String,
        expected_latest: String,
        content: ContextRecordContent,
        provenance: ContextProvenance,
    ) -> Result<ContextRecordRevision> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (old_id, old_number, status): (String, i64, String) = tx
                .query_row(
                    "SELECT id, revision, status FROM project_context_revisions
                 WHERE project_id = ?1 AND record_id = ?2 ORDER BY revision DESC LIMIT 1",
                    params![project, record],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .context("the context record is unavailable in this project")?;
            ensure!(
                old_id == expected_latest,
                "the context record has a newer revision"
            );
            let proposed = ContextRecordRevision {
                id: Uuid::new_v4().to_string(),
                record_id: record,
                revision: old_number + 1,
                status: ContextReviewStatus::Proposed,
                content,
                provenance,
            };
            if matches!(status.as_str(), "proposed" | "in_review") {
                set_review_status(
                    &tx,
                    &project,
                    &proposed.record_id,
                    &old_id,
                    ContextReviewStatus::Superseded,
                    Some(&proposed.id),
                )?;
            }
            insert_proposal(&tx, &project, &proposed)?;
            tx.commit()?;
            Ok(proposed)
        })
        .await
    }

    pub async fn review_context_revision(
        &self,
        project: String,
        record: String,
        revision: String,
        expected: ContextReviewStatus,
        decision: ContextReviewDecision,
    ) -> Result<ContextReviewStatus> {
        use ContextReviewDecision::*;
        use ContextReviewStatus::*;
        let next = match (expected, decision) {
            (Proposed, Review) => InReview,
            (Proposed | InReview, Accept) => Accepted,
            (Proposed | InReview, Reject) => Rejected,
            _ => anyhow::bail!("invalid context review transition"),
        };
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let actual: Option<String> = tx.query_row(
                "SELECT status FROM project_context_revisions WHERE project_id = ?1 AND record_id = ?2 AND id = ?3",
                params![project, record, revision], |row| row.get(0),
            ).optional()?;
            ensure!(actual.as_deref() == Some(expected.key()), "the context review state changed");

            if next == Accepted {
                let prior: Option<String> = tx.query_row(
                    "SELECT id FROM project_context_revisions
                     WHERE project_id = ?1 AND record_id = ?2 AND status = 'accepted'",
                    params![project, record], |row| row.get(0),
                ).optional()?;
                if let Some(id) = prior {
                    set_review_status(&tx, &project, &record, &id, Superseded, Some(&revision))?;
                }
            }
            set_review_status(&tx, &project, &record, &revision, next, None)?;
            tx.commit()?;
            Ok(next)
        }).await
    }

    pub async fn context_record_history(
        &self,
        project: String,
        record: String,
    ) -> Result<Option<ContextRecordHistory>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let metadata: Option<(String, Option<String>)> = tx.query_row(
                "SELECT kind, task_id FROM project_context_records WHERE project_id = ?1 AND id = ?2",
                params![project, record], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((kind, task_id)) = metadata else { return Ok(None); };
            let mut sql = tx.prepare(
                "SELECT id, revision, status, content, origin_run_id, markdown_checkout_id, markdown_revision_id
                 FROM project_context_revisions WHERE project_id = ?1 AND record_id = ?2 ORDER BY revision",
            )?;
            let mut rows = sql.query(params![project, record])?;
            let mut revisions = Vec::new();
            while let Some(row) = rows.next()? {
                let encrypted: Vec<u8> = row.get(3)?;
                let provenance = match (row.get(4)?, row.get(5)?) {
                    (Some(run_id), _) => ContextProvenance::Run { run_id },
                    (None, Some(checkout_id)) => ContextProvenance::RepositoryMarkdown { checkout_id, revision_id: row.get(6)? },
                    (None, None) => ContextProvenance::Operator,
                };
                revisions.push(ContextRecordRevision {
                    id: row.get(0)?, record_id: record.clone(), revision: row.get(1)?, status: from_key(row.get(2)?)?,
                    content: serde_json::from_slice(&protection::unprotect(&encrypted)?)?, provenance,
                });
            }

            let mut sql = tx.prepare(
                "SELECT revision_id, status, related_revision_id, recorded_at FROM project_context_review_events
                 WHERE project_id = ?1 AND record_id = ?2 ORDER BY sequence",
            )?;
            let mut rows = sql.query(params![project, record])?;
            let mut review_events = Vec::new();
            while let Some(row) = rows.next()? {
                review_events.push(ContextReviewEvent {
                    revision_id: row.get(0)?, status: from_key(row.get(1)?)?, superseded_by: row.get(2)?, recorded_at: row.get(3)?,
                });
            }
            Ok(Some(ContextRecordHistory { kind: from_key(kind)?, task_id, revisions, review_events }))
        }).await
    }
}

fn insert_proposal(db: &Connection, project: &str, proposed: &ContextRecordRevision) -> Result<()> {
    ensure!(
        !proposed.content.title.trim().is_empty(),
        "a context record needs a title"
    );
    let (run, markdown_checkout, markdown_id) = match &proposed.provenance {
        ContextProvenance::Operator => (None, None, None),
        ContextProvenance::Run { run_id } => {
            let belongs: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs r JOIN checkouts c ON c.id = r.checkout_id
                 WHERE r.id = ?1 AND c.project_id = ?2)",
                params![run_id, project],
                |row| row.get(0),
            )?;
            ensure!(belongs, "the source run is outside the project");
            (Some(run_id), None, None)
        }
        ContextProvenance::RepositoryMarkdown {
            checkout_id,
            revision_id,
        } => (None, Some(checkout_id), Some(revision_id)),
    };
    let content = protection::protect(&serde_json::to_vec(&proposed.content)?)?;
    db.execute(
        "INSERT INTO project_context_revisions
         (project_id, record_id, id, revision, status, content, origin_run_id, markdown_checkout_id, markdown_revision_id)
         VALUES (?1, ?2, ?3, ?4, 'proposed', ?5, ?6, ?7, ?8)",
        params![project, proposed.record_id, proposed.id, proposed.revision, content, run, markdown_checkout, markdown_id],
    ).context("the proposed context revision needs a valid source in this project")?;
    record_review_event(
        db,
        project,
        &proposed.record_id,
        &proposed.id,
        ContextReviewStatus::Proposed,
        None,
    )
}

fn set_review_status(
    db: &Connection,
    project: &str,
    record: &str,
    revision: &str,
    status: ContextReviewStatus,
    related: Option<&str>,
) -> Result<()> {
    let changed = db.execute(
        "UPDATE project_context_revisions SET status = ?4 WHERE project_id = ?1 AND record_id = ?2 AND id = ?3",
        params![project, record, revision, status.key()],
    )?;
    ensure!(
        changed == 1,
        "the context revision is unavailable in this project"
    );
    record_review_event(db, project, record, revision, status, related)
}

fn record_review_event(
    db: &Connection,
    project: &str,
    record: &str,
    revision: &str,
    status: ContextReviewStatus,
    related: Option<&str>,
) -> Result<()> {
    db.execute(
        "INSERT INTO project_context_review_events(project_id, record_id, revision_id, status, related_revision_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![project, record, revision, status.key(), related],
    )?;
    Ok(())
}

fn from_key<T: serde::de::DeserializeOwned>(value: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(value))?)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::repository;

    #[tokio::test]
    async fn findings_keep_review_history_and_only_the_accepted_revision_active() -> Result<()> {
        use ContextReviewDecision::{Accept, Reject, Review};
        use ContextReviewStatus::{Accepted, InReview, Proposed, Rejected, Superseded};

        let dir = tempfile::tempdir()?;
        let database = dir.path().join("context.sqlite");
        let store = Store::open(database.clone())?;
        let mut repos = Vec::new();
        for label in ["project", "other"] {
            let root = dir.path().join(label);
            std::fs::create_dir(&root)?;
            ensure!(
                Command::new("git.exe")
                    .args(["init", "--quiet"])
                    .current_dir(&root)
                    .status()?
                    .success()
            );
            repos.push(repository::register(&store, root).await?);
        }
        let project = repos[0].project_id.clone();
        let primary = store
            .create_project_task(project.clone(), "Task".into())
            .await?;
        let external = store.start_run(repos[1].checkout_id.clone()).await?.0;
        let content = |markdown: &str| ContextRecordContent {
            title: "API criteria".into(),
            markdown: markdown.into(),
        };
        assert!(
            store
                .propose_context_record(
                    project.clone(),
                    Some(primary.id.clone()),
                    ContextRecordKind::TaskCriterion,
                    content("Cross-project evidence"),
                    ContextProvenance::Run {
                        run_id: external.id
                    },
                )
                .await
                .is_err()
        );
        let original = store
            .propose_context_record(
                project.clone(),
                Some(primary.id.clone()),
                ContextRecordKind::TaskCriterion,
                content("Preserve machine-readable errors"),
                ContextProvenance::Operator,
            )
            .await?;
        let record = original.record_id.clone();
        let review = |revision: String, expected, decision| {
            store.review_context_revision(
                project.clone(),
                record.clone(),
                revision,
                expected,
                decision,
            )
        };
        assert_eq!(
            review(original.id.clone(), Proposed, Review).await?,
            InReview
        );
        assert!(review(original.id.clone(), Proposed, Reject).await.is_err());
        assert_eq!(
            review(original.id.clone(), InReview, Accept).await?,
            Accepted
        );
        let rejected = store
            .propose_context_revision(
                project.clone(),
                record.clone(),
                original.id.clone(),
                content("Remove existing errors"),
                ContextProvenance::Operator,
            )
            .await?;
        assert_eq!(
            review(rejected.id.clone(), Proposed, Reject).await?,
            Rejected
        );
        let before = store
            .context_record_history(project.clone(), record.clone())
            .await?
            .unwrap();
        assert_eq!(
            before
                .revisions
                .iter()
                .map(|revision| revision.status)
                .collect::<Vec<_>>(),
            [Accepted, Rejected]
        );

        let run = store.start_run(repos[0].checkout_id.clone()).await?.0;
        let evidence = ContextProvenance::Run { run_id: run.id };
        let pending = store
            .propose_context_revision(
                project.clone(),
                record.clone(),
                rejected.id.clone(),
                content("Add error codes for new endpoints"),
                evidence.clone(),
            )
            .await?;
        let replacement = store
            .propose_context_revision(
                project.clone(),
                record.clone(),
                pending.id.clone(),
                content("Keep old errors and add standard codes"),
                evidence.clone(),
            )
            .await?;
        assert!(review(pending.id.clone(), Proposed, Accept).await.is_err());
        let (one, two) = tokio::join!(
            review(replacement.id.clone(), Proposed, Accept),
            review(replacement.id.clone(), Proposed, Accept)
        );
        assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
        drop(store);

        let store = Store::open(database.clone())?;
        let history = store
            .context_record_history(project, record.clone())
            .await?
            .unwrap();
        assert_eq!(history.kind, ContextRecordKind::TaskCriterion);
        assert_eq!(history.task_id, Some(primary.id));
        assert_eq!(
            history
                .revisions
                .iter()
                .map(|revision| revision.status)
                .collect::<Vec<_>>(),
            [Superseded, Rejected, Superseded, Accepted]
        );
        assert_eq!(history.revisions[0].content, original.content);
        assert_eq!(history.revisions[3].content, replacement.content);
        assert_eq!(history.revisions[3].provenance, evidence);
        assert_eq!(
            history
                .review_events
                .iter()
                .filter(|event| event.superseded_by.as_ref() == Some(&replacement.id))
                .count(),
            2
        );
        assert!(
            store
                .context_record_history(repos[1].project_id.clone(), record)
                .await?
                .is_none()
        );
        let db = Connection::open(database)?;
        let payload: Vec<u8> = db.query_row(
            "SELECT content FROM project_context_revisions WHERE id = ?1",
            [&replacement.id],
            |row| row.get(0),
        )?;
        assert_eq!(
            protection::unprotect(&payload)?,
            serde_json::to_vec(&replacement.content)?
        );
        Ok(())
    }
}
