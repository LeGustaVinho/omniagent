use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{dispatch::ConversationArtifactRevision, protection, store::Store};

use super::{
    load_conversation,
    references::{
        ConversationReferenceSnapshot, load_conversation_reference, recheck_conversation_reference,
    },
};

const MAX_DRAFT_REFERENCES: usize = 8;
const MAX_DRAFT_REFERENCE_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmittedConversationReference {
    pub message_revision_id: String,
    pub reference: ConversationReferenceSnapshot,
}

impl Store {
    pub async fn conversation_reference_draft(
        &self,
        conversation_id: String,
    ) -> Result<Vec<ConversationReferenceSnapshot>> {
        self.execute(move |db| {
            let project = conversation_project(db, &conversation_id)?;
            draft_references(db, &conversation_id, &project)
        })
        .await
    }

    pub async fn edit_conversation_reference_draft(
        &self,
        conversation_id: String,
        preview_id: String,
        attach: bool,
    ) -> Result<Vec<ConversationReferenceSnapshot>> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let project = conversation_project(&tx, &conversation_id)?;
            if attach {
                let reference = load_conversation_reference(&tx, &conversation_id, &project, &preview_id)?;
                let current = draft_references(&tx, &conversation_id, &project)?;
                if current.iter().all(|item| item.id != preview_id) {
                    ensure!(current.len() < MAX_DRAFT_REFERENCES, "The draft has too many conversation references");
                    let bytes = current.iter().chain([&reference]).try_fold(0usize, |sum, item| {
                        Ok::<_, anyhow::Error>(sum + serde_json::to_vec(&item.artifact)?.len())
                    })?;
                    ensure!(bytes <= MAX_DRAFT_REFERENCE_BYTES, "The selected conversation references are too large. Select fewer messages.");
                    tx.execute(
                        "INSERT INTO conversation_reference_drafts(conversation_id, ordinal, preview_id)
                         SELECT ?1, coalesce(max(ordinal), 0) + 1, ?2 FROM conversation_reference_drafts WHERE conversation_id = ?1",
                        params![conversation_id, preview_id],
                    )?;
                }
            } else {
                tx.execute(
                    "DELETE FROM conversation_reference_drafts WHERE conversation_id = ?1 AND preview_id = ?2",
                    params![conversation_id, preview_id],
                )?;
            }
            let result = draft_references(&tx, &conversation_id, &project)?;
            tx.commit()?;
            Ok(result)
        }).await
    }

    pub(crate) async fn conversation_dispatch_references(
        &self,
        run_id: String,
        message_revisions: Vec<String>,
    ) -> Result<Vec<SubmittedConversationReference>> {
        self.execute(move |db| submitted_references(db, &run_id, &message_revisions))
            .await
    }
}

fn conversation_project(db: &Connection, conversation: &str) -> Result<String> {
    let conversation = load_conversation(db, conversation)?;
    db.query_row(
        "SELECT project_id FROM checkouts WHERE id = ?1",
        [&conversation.checkout_id],
        |row| row.get(0),
    )
    .context("The conversation project is unavailable")
}

fn draft_references(
    db: &Connection,
    conversation: &str,
    project: &str,
) -> Result<Vec<ConversationReferenceSnapshot>> {
    let mut statement = db.prepare_cached(
        "SELECT preview_id FROM conversation_reference_drafts WHERE conversation_id = ?1 ORDER BY ordinal",
    )?;
    statement
        .query_map([conversation], |row| row.get::<_, String>(0))?
        .map(|id| load_conversation_reference(db, conversation, project, &id?))
        .collect()
}

pub(super) fn verify_draft_references(
    db: &Connection,
    conversation: &str,
    project: &str,
    expected: &[String],
) -> Result<()> {
    let saved = draft_references(db, conversation, project)?;
    ensure!(
        saved.iter().map(|item| &item.id).eq(expected),
        "The conversation references changed. Review the draft and send again."
    );
    Ok(())
}

pub(super) fn consume_draft_references(db: &Connection, conversation: &str) -> Result<()> {
    db.execute(
        "DELETE FROM conversation_reference_drafts WHERE conversation_id = ?1",
        [conversation],
    )?;
    Ok(())
}

pub(crate) fn bind_references(
    db: &Connection,
    revision: &str,
    reference_ids: &[String],
) -> Result<()> {
    let mut bind =
        db.prepare_cached("INSERT INTO message_conversation_references VALUES (?1, ?2, ?3)")?;
    for (ordinal, reference_id) in reference_ids.iter().enumerate() {
        bind.execute(params![revision, i64::try_from(ordinal)?, reference_id])?;
    }
    Ok(())
}

pub(crate) fn copy_references(db: &Connection, source: &str, destination: &str) -> Result<()> {
    db.execute(
        "INSERT INTO message_conversation_references
         SELECT ?2, ordinal, preview_id FROM message_conversation_references WHERE message_revision_id = ?1",
        params![source, destination],
    )?;
    Ok(())
}

pub(super) fn reference_ids(db: &Connection, revision: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare_cached(
        "SELECT preview_id FROM message_conversation_references WHERE message_revision_id = ?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map([revision], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn submitted_references(
    db: &Connection,
    run: &str,
    revisions: &[String],
) -> Result<Vec<SubmittedConversationReference>> {
    let (conversation, project): (String, String) = db.query_row(
        "SELECT attempt.conversation_id, checkout.project_id FROM agent_attempts attempt
         JOIN conversations conversation ON conversation.id = attempt.conversation_id
         JOIN checkouts checkout ON checkout.id = conversation.checkout_id WHERE attempt.run_id = ?1",
        [run], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = db.prepare_cached(
        "SELECT preview.payload FROM message_conversation_references link
         JOIN conversation_reference_previews preview ON preview.id = link.preview_id
         JOIN message_revisions revision ON revision.id = link.message_revision_id
         JOIN messages message ON message.id = revision.message_id
         WHERE link.message_revision_id = ?1 AND message.conversation_id = ?2 ORDER BY link.ordinal",
    )?;
    let mut result = Vec::new();
    for revision in revisions {
        let rows = statement.query_map(params![revision, conversation], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        for payload in rows {
            let reference = serde_json::from_slice(&protection::unprotect(&payload?)?)?;
            recheck_conversation_reference(db, &conversation, &project, &reference)?;
            result.push(SubmittedConversationReference {
                message_revision_id: revision.clone(),
                reference,
            });
        }
    }
    Ok(result)
}

pub(crate) fn source_revisions(artifact: &ConversationArtifactRevision) -> Vec<&str> {
    match artifact {
        ConversationArtifactRevision::Excerpt { excerpts, .. } => excerpts
            .iter()
            .map(|entry| entry.source.origin.revision_id.as_str())
            .collect(),
        ConversationArtifactRevision::Summary { covered, run, .. } => covered
            .iter()
            .map(|entry| entry.origin.revision_id.as_str())
            .chain([run.output_revision_id.as_str()])
            .collect(),
    }
}
