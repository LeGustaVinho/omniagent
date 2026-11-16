use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    DescriptionOwner, DescriptionRevision, current_description, description_revision,
    write_description,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionDraft {
    pub revision: String,
    pub base: Option<DescriptionRevision>,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptionEditorState {
    pub saved: Option<DescriptionRevision>,
    pub draft: Option<DescriptionDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionEditorConflict {
    SavedRevisionChanged,
    DraftRevisionChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DescriptionEditorSave {
    Saved {
        revision: DescriptionRevision,
    },
    Conflict {
        reason: DescriptionEditorConflict,
        state: DescriptionEditorState,
    },
}

impl Store {
    pub async fn project_description_editor(
        &self,
        project: String,
        owner: DescriptionOwner,
    ) -> Result<DescriptionEditorState> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            validate_owner(&tx, &project, &owner)?;
            load_editor_state(&tx, &project, &owner)
        })
        .await
    }

    pub async fn save_project_description_draft(
        &self,
        project: String,
        owner: DescriptionOwner,
        base_revision: Option<String>,
        expected_draft_revision: Option<String>,
        markdown: String,
    ) -> Result<DescriptionDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            validate_owner(&tx, &project, &owner)?;
            let current = load_draft(&tx, &project, &owner)?;
            ensure!(
                current.as_ref().map(|draft| draft.revision.as_str())
                    == expected_draft_revision.as_deref(),
                "the description draft changed before the edit was saved"
            );
            if let Some(current) = current
                && current.base.as_ref().map(|base| base.id.as_str()) == base_revision.as_deref()
                && current.markdown == markdown
            {
                return Ok(current);
            }
            let base = load_draft_base(&tx, &project, &owner, base_revision.as_deref())?;
            if let Some(expected) = expected_draft_revision {
                ensure!(
                    consume_draft(&tx, &project, &owner, &expected)?,
                    "the draft could not be replaced"
                );
            }
            let revision = Uuid::new_v4().to_string();
            let body = protection::protect(markdown.as_bytes())?;
            let (epic, task) = owner.database_scope();
            tx.execute(
                "INSERT INTO project_description_drafts
                 (project_id, epic_id, task_id, draft_revision, base_revision_id, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![project, epic, task, revision, base_revision, body],
            )?;
            tx.commit()?;
            Ok(DescriptionDraft {
                revision,
                base,
                markdown,
            })
        })
        .await
    }

    pub async fn discard_project_description_draft(
        &self,
        project: String,
        owner: DescriptionOwner,
        expected_draft_revision: String,
    ) -> Result<bool> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            validate_owner(&tx, &project, &owner)?;
            let discarded = consume_draft(&tx, &project, &owner, &expected_draft_revision)?;
            tx.commit()?;
            Ok(discarded)
        })
        .await
    }

    pub async fn save_project_description_editor(
        &self,
        project: String,
        owner: DescriptionOwner,
        expected_revision: Option<String>,
        expected_draft_revision: Option<String>,
        markdown: Option<String>,
    ) -> Result<DescriptionEditorSave> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            validate_owner(&tx, &project, &owner)?;
            let state = load_editor_state(&tx, &project, &owner)?;
            if state.saved.as_ref().map(|saved| &saved.id) != expected_revision.as_ref() {
                return Ok(DescriptionEditorSave::Conflict {
                    reason: DescriptionEditorConflict::SavedRevisionChanged,
                    state,
                });
            }
            if state.draft.as_ref().map(|draft| &draft.revision) != expected_draft_revision.as_ref()
            {
                return Ok(DescriptionEditorSave::Conflict {
                    reason: DescriptionEditorConflict::DraftRevisionChanged,
                    state,
                });
            }
            if let Some(expected) = expected_draft_revision {
                ensure!(
                    consume_draft(&tx, &project, &owner, &expected)?,
                    "the draft could not be consumed"
                );
            }
            let revision = write_description(&tx, &project, owner, state.saved, markdown)?;
            tx.commit()?;
            Ok(DescriptionEditorSave::Saved { revision })
        })
        .await
    }
}

fn validate_owner(db: &Connection, project: &str, owner: &DescriptionOwner) -> Result<()> {
    let (sql, id) = match owner {
        DescriptionOwner::Epic(id) => (
            "SELECT EXISTS(SELECT 1 FROM project_epics WHERE project_id = ?1 AND id = ?2)",
            id,
        ),
        DescriptionOwner::Work(id) => (
            "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2)",
            id,
        ),
    };
    let exists: bool = db.query_row(sql, params![project, id], |row| row.get(0))?;
    ensure!(
        exists,
        "the description owner is unavailable in this project"
    );
    Ok(())
}

fn load_editor_state(
    db: &Connection,
    project: &str,
    owner: &DescriptionOwner,
) -> Result<DescriptionEditorState> {
    Ok(DescriptionEditorState {
        saved: current_description(db, project, owner)?,
        draft: load_draft(db, project, owner)?,
    })
}

fn load_draft(
    db: &Connection,
    project: &str,
    owner: &DescriptionOwner,
) -> Result<Option<DescriptionDraft>> {
    let (epic, task) = owner.database_scope();
    let row = db
        .query_row(
            "SELECT draft_revision, base_revision_id, body FROM project_description_drafts
         WHERE project_id = ?1 AND epic_id IS ?2 AND task_id IS ?3",
            params![project, epic, task],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()?;
    row.map(|(revision, base, body)| {
        let base = load_draft_base(db, project, owner, base.as_deref())?;
        let markdown = String::from_utf8(protection::unprotect(&body)?)?;
        Ok(DescriptionDraft {
            revision,
            base,
            markdown,
        })
    })
    .transpose()
}

fn load_draft_base(
    db: &Connection,
    project: &str,
    owner: &DescriptionOwner,
    base: Option<&str>,
) -> Result<Option<DescriptionRevision>> {
    base.map(|id| {
        description_revision(db, project, id)?
            .filter(|revision| &revision.owner == owner)
            .context("the draft base revision is unavailable for this description")
    })
    .transpose()
}

fn consume_draft(
    db: &Connection,
    project: &str,
    owner: &DescriptionOwner,
    expected: &str,
) -> Result<bool> {
    let (epic, task) = owner.database_scope();
    Ok(db.execute(
        "DELETE FROM project_description_drafts
         WHERE project_id = ?1 AND epic_id IS ?2 AND task_id IS ?3 AND draft_revision = ?4",
        params![project, epic, task, expected],
    )? == 1)
}
