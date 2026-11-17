use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    attachments::{
        AttachmentDelivery, AttachmentDispatch, AttachmentInput, AttachmentMetadata,
        captured_for_revision,
    },
    protection,
    store::Store,
};

use super::ContextSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentHistoryTarget {
    pub conversation_id: String,
    pub message_revision_id: String,
    /// Original zero-based position within the saved message revision.
    pub attachment_ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentHistoryRemoval {
    Selected,
    WithDerivedCopies,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRemovalPreview {
    pub revision: String,
    pub saved_revisions: usize,
    pub dispatches: usize,
    pub native_sessions: usize,
    pub unsettled_attempts: usize,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttachmentIdentity {
    metadata: AttachmentMetadata,
    delivery: AttachmentDelivery,
}

impl AttachmentIdentity {
    fn matches(&self, attachment: &AttachmentDispatch) -> bool {
        self.metadata == attachment.metadata
            && self.delivery
                == match &attachment.input {
                    AttachmentInput::Native(_) => AttachmentDelivery::Native,
                    AttachmentInput::Reference(_) => AttachmentDelivery::Reference,
                    AttachmentInput::Excerpt(excerpt) => AttachmentDelivery::Excerpt {
                        start: excerpt.start,
                        end: excerpt.end,
                    },
                }
    }
}

#[derive(Serialize)]
struct RemovalPlan {
    target: AttachmentHistoryTarget,
    removal: AttachmentHistoryRemoval,
    identity: AttachmentIdentity,
    saved: BTreeMap<String, Vec<usize>>,
    dispatches: BTreeMap<String, Vec<DispatchAttachment>>,
    sessions: BTreeSet<String>,
    unsettled: BTreeSet<String>,
}

#[derive(Serialize)]
struct DispatchAttachment {
    ordinal: usize,
    revision_id: String,
}

impl RemovalPlan {
    fn preview(&self) -> Result<AttachmentRemovalPreview> {
        Ok(AttachmentRemovalPreview {
            revision: format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)),
            saved_revisions: self.saved.len(),
            dispatches: self.dispatches.len(),
            native_sessions: self.sessions.len(),
            unsettled_attempts: self.unsettled.len(),
        })
    }
}

impl Store {
    pub async fn preview_attachment_history_removal(
        &self,
        target: AttachmentHistoryTarget,
        removal: AttachmentHistoryRemoval,
    ) -> Result<AttachmentRemovalPreview> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            removal_plan(&tx, target, removal)?.preview()
        })
        .await
    }

    pub async fn confirm_attachment_history_removal(
        &self,
        target: AttachmentHistoryTarget,
        removal: AttachmentHistoryRemoval,
        expected_revision: String,
    ) -> Result<AttachmentRemovalPreview> {
        let preview = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let plan = removal_plan(&tx, target, removal)?;
                let preview = plan.preview()?;
                ensure!(
                    preview.revision == expected_revision,
                    "Attachment deletion impact changed"
                );
                ensure!(
                    plan.unsettled.is_empty(),
                    "Settle affected attempts before deleting attachment history"
                );
                apply(&tx, &plan)?;
                tx.commit()?;
                Ok(preview)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(preview)
    }
}

pub(crate) fn revoked_history_run(db: &Connection, run_id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_attempts attempt
         JOIN history_revoked_sessions retired ON retired.session_id = attempt.agent_session_id
         WHERE attempt.run_id = ?1)",
        [run_id],
        |row| row.get(0),
    )?)
}

fn removal_plan(
    db: &Connection,
    target: AttachmentHistoryTarget,
    removal: AttachmentHistoryRemoval,
) -> Result<RemovalPlan> {
    let owner: Option<String> = db
        .query_row(
            "SELECT m.conversation_id FROM message_revisions revision
         JOIN messages m ON m.id = revision.message_id WHERE revision.id = ?1",
            [&target.message_revision_id],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        owner.as_ref() == Some(&target.conversation_id),
        "Attachment revision is outside this conversation"
    );
    let identity = saved_identities(db, &target.message_revision_id)?
        .remove(&target.attachment_ordinal)
        .context("Attachment does not exist at the selected revision")?;
    let revisions = copied_revisions(db, &target.message_revision_id)?;
    let mut saved = BTreeMap::new();
    let mut live_revisions = BTreeSet::new();
    let mut dispatches = BTreeMap::new();
    let mut sessions = BTreeSet::new();
    let mut unsettled = BTreeSet::new();
    for revision in &revisions {
        let removed = retired_identities(db, revision)?;
        let mut matches: Vec<_> = saved_identities(db, revision)?
            .into_iter()
            .filter_map(|(ordinal, entry)| {
                (entry == identity && !removed.contains_key(&ordinal)).then_some(ordinal)
            })
            .collect();
        if !matches.is_empty() {
            live_revisions.insert(revision);
            if removal == AttachmentHistoryRemoval::Selected {
                matches.retain(|ordinal| {
                    revision == &target.message_revision_id && *ordinal == target.attachment_ordinal
                });
            }
            if !matches.is_empty() {
                saved.insert(revision.clone(), matches);
            }
        }
    }

    let revisions_json = serde_json::to_string(&revisions)?;
    // Include prepared attempts whose snapshots have not been written yet.
    // This excludes a race with a launcher holding a frozen copy in memory.
    let mut attempt_query = db.prepare(
        "SELECT a.run_id FROM agent_attempts a
         JOIN conversation_contexts c ON c.id = a.context_id
         WHERE EXISTS (SELECT 1 FROM json_each(c.entries) entry
                       WHERE entry.value IN (SELECT value FROM json_each(?1)))
           AND (EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
             OR EXISTS (SELECT 1 FROM unconfirmed_agent_stops WHERE run_id = a.run_id))",
    )?;
    let live_json = serde_json::to_string(&live_revisions)?;
    for id in attempt_query.query_map([live_json], |row| row.get::<_, String>(0))? {
        unsettled.insert(id?);
    }

    let mut dispatch_query = db.prepare(
        "SELECT d.id, snapshot.payload, a.agent_session_id, s.provider_session_id IS NOT NULL
         FROM dispatches d JOIN context_snapshots snapshot ON snapshot.id = d.snapshot_id
         JOIN agent_attempts a ON a.run_id = snapshot.run_id
         JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE d.ordinal = 1 AND (EXISTS (
             SELECT 1 FROM dispatch_lineage_messages line WHERE line.dispatch_id = d.id
                AND line.revision_id IN (SELECT value FROM json_each(?1))
         ) OR (NOT EXISTS (SELECT 1 FROM dispatch_lineage line WHERE line.dispatch_id = d.id) AND EXISTS (
             SELECT 1 FROM messages m JOIN message_revisions v ON v.message_id = m.id
             WHERE m.conversation_id = a.conversation_id AND v.id IN (SELECT value FROM json_each(?1))
         )))",
    )?;
    let mut rows = dispatch_query.query([&revisions_json])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let snapshot: ContextSnapshot = decode(&row.get::<_, Vec<u8>>(1)?)?;
        let ordinals = dispatch_removed_ordinals(db, &id)?;
        let matches: Vec<_> = present_ordinals(&ordinals)
            .zip(snapshot.attachments.iter())
            .filter_map(|(ordinal, attachment)| {
                (revisions.contains(&attachment.message_revision_id)
                    && identity.matches(attachment))
                .then(|| DispatchAttachment {
                    ordinal,
                    revision_id: attachment.message_revision_id.clone(),
                })
            })
            .collect();
        if !matches.is_empty() {
            if row.get::<_, bool>(3)? {
                sessions.insert(row.get::<_, String>(2)?);
            }
            if removal == AttachmentHistoryRemoval::WithDerivedCopies {
                dispatches.insert(id, matches);
            }
        }
    }
    let sessions_json = serde_json::to_string(&sessions)?;
    let mut active_query = db.prepare(
        "SELECT a.run_id FROM agent_attempts a
         WHERE a.agent_session_id IN (SELECT value FROM json_each(?1)) AND (
            EXISTS(SELECT 1 FROM active_agent_attempts WHERE run_id = a.run_id)
         OR EXISTS(SELECT 1 FROM unconfirmed_agent_stops WHERE run_id = a.run_id))
         UNION SELECT attempt.run_id FROM job_side_requests side
         JOIN job_attempts attempt ON attempt.job_id = side.job_id
         JOIN runs r ON r.id = attempt.run_id AND r.status IN ('starting','running','waiting_for_input')
         JOIN agent_attempts source ON source.run_id = side.source_run_id
         WHERE source.agent_session_id IN (SELECT value FROM json_each(?1))",
    )?;
    for id in active_query.query_map([sessions_json], |row| row.get::<_, String>(0))? {
        unsettled.insert(id?);
    }
    Ok(RemovalPlan {
        target,
        removal,
        identity,
        saved,
        dispatches,
        sessions,
        unsettled,
    })
}

fn copied_revisions(db: &Connection, source: &str) -> Result<BTreeSet<String>> {
    let mut query = db.prepare(
        "WITH RECURSIVE copies(id) AS (
             SELECT ?1 UNION SELECT copied_revision_id FROM attachment_revision_copy_sources
             JOIN copies ON source_revision_id = copies.id
         ) SELECT id FROM copies",
    )?;
    Ok(query
        .query_map([source], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn retired_identities(
    db: &Connection,
    revision: &str,
) -> Result<BTreeMap<usize, AttachmentIdentity>> {
    let mut query = db.prepare(
        "SELECT attachment_ordinal, identity FROM removed_history_attachments
         WHERE message_revision_id = ?1",
    )?;
    let mut retired = BTreeMap::new();
    let mut rows = query.query([revision])?;
    while let Some(row) = rows.next()? {
        retired.insert(
            usize::try_from(row.get::<_, u32>(0)?)?,
            decode(&row.get::<_, Vec<u8>>(1)?)?,
        );
    }
    Ok(retired)
}

pub(super) fn archived_attachment_was_removed(
    db: &Connection,
    attachment: &AttachmentDispatch,
) -> Result<bool> {
    Ok(retired_identities(db, &attachment.message_revision_id)?
        .values()
        .any(|identity| identity.matches(attachment)))
}

fn saved_identities(
    db: &Connection,
    revision: &str,
) -> Result<BTreeMap<usize, AttachmentIdentity>> {
    let mut identities = retired_identities(db, revision)?;
    let retired = identities.keys().copied().collect();
    for (ordinal, entry) in present_ordinals(&retired).zip(captured_for_revision(db, revision)?) {
        identities.insert(
            ordinal,
            AttachmentIdentity {
                delivery: entry.delivery(),
                metadata: entry.metadata,
            },
        );
    }
    Ok(identities)
}

pub(super) fn dispatch_removed_ordinals(
    db: &Connection,
    dispatch: &str,
) -> Result<BTreeSet<usize>> {
    let mut query = db.prepare(
        "SELECT attachment_ordinal FROM removed_dispatch_attachments WHERE dispatch_id = ?1",
    )?;
    let ordinals = query
        .query_map([dispatch], |row| row.get::<_, u32>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ordinals
        .into_iter()
        .map(usize::try_from)
        .collect::<Result<_, _>>()?)
}

pub(super) fn present_ordinals(retired: &BTreeSet<usize>) -> impl Iterator<Item = usize> + '_ {
    (0..).filter(|ordinal| !retired.contains(ordinal))
}

fn apply(db: &Connection, plan: &RemovalPlan) -> Result<()> {
    let identity = encode(&plan.identity)?;
    for (revision, slots) in &plan.saved {
        let retired = retired_identities(db, revision)?.into_keys().collect();
        let retained: Vec<_> = present_ordinals(&retired)
            .zip(captured_for_revision(db, revision)?)
            .filter_map(|(ordinal, attachment)| (!slots.contains(&ordinal)).then_some(attachment))
            .collect();
        for slot in slots {
            db.execute(
                "INSERT INTO removed_history_attachments(message_revision_id, attachment_ordinal, identity)
                 VALUES (?1, ?2, ?3)",
                params![revision, u32::try_from(*slot)?, identity],
            )?;
        }
        if retained.is_empty() {
            db.execute(
                "DELETE FROM message_attachments WHERE message_revision_id = ?1",
                [revision],
            )?;
        } else {
            db.execute(
                "UPDATE message_attachments SET payload = ?2 WHERE message_revision_id = ?1",
                params![revision, encode(&retained)?],
            )?;
        }
        let labels = retained
            .iter()
            .map(|attachment| {
                attachment
                    .metadata
                    .source
                    .file_name()
                    .unwrap_or("Attachment")
            })
            .collect::<Vec<_>>();
        db.execute("UPDATE queued_conversation_instructions SET attachment_labels = ?2 WHERE revision_id = ?1",
            params![revision, (!labels.is_empty()).then(|| encode(&labels)).transpose()?])?;
    }
    let unavailable_frames = encode(&Vec::<Vec<u8>>::new())?;
    for (dispatch, attachments) in &plan.dispatches {
        let (snapshot_id, run_id, payload): (String, String, Vec<u8>) = db.query_row(
            "SELECT s.id, s.run_id, s.payload FROM dispatches d JOIN context_snapshots s ON s.id = d.snapshot_id WHERE d.id = ?1",
            [dispatch], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let mut snapshot: ContextSnapshot = decode(&payload)?;
        super::lineage::preserve_legacy_attachment_lineage(db, dispatch, &run_id, &snapshot)?;
        let retired = dispatch_removed_ordinals(db, dispatch)?;
        snapshot.attachments = present_ordinals(&retired)
            .zip(snapshot.attachments)
            .filter_map(|(ordinal, attachment)| {
                (!attachments.iter().any(|removed| removed.ordinal == ordinal))
                    .then_some(attachment)
            })
            .collect();
        for attachment in attachments {
            db.execute(
                "INSERT INTO removed_dispatch_attachments(dispatch_id, attachment_ordinal, message_revision_id)
                 VALUES (?1, ?2, ?3)",
                params![dispatch, u32::try_from(attachment.ordinal)?, attachment.revision_id],
            )?;
        }
        db.execute(
            "UPDATE context_snapshots SET payload = ?2 WHERE id = ?1",
            params![snapshot_id, encode(&snapshot)?],
        )?;
        // Each protected frame is exact wire evidence. Retaining fragments would
        // be misleading and could leave an encoded copy of the removed content.
        db.execute(
            "UPDATE dispatches SET frames = ?2 WHERE snapshot_id = ?1",
            params![snapshot_id, unavailable_frames],
        )?;
    }
    for session in &plan.sessions {
        db.execute(
            "INSERT OR IGNORE INTO history_revoked_sessions(session_id) VALUES (?1)",
            [session],
        )?;
        db.execute(
            "UPDATE agent_sessions SET cursor_context_id = NULL WHERE id = ?1",
            [session],
        )?;
        db.execute(
            "UPDATE jobs SET queue_state = 'cancelled'
             WHERE queue_state IN ('queued','paused')
               AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = jobs.id)
                    OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = jobs.id))
               AND id IN (
                 SELECT side.job_id FROM job_side_requests side
                 JOIN agent_attempts source ON source.run_id = side.source_run_id
                 WHERE source.agent_session_id = ?1
             )",
            [session],
        )?;
    }
    Ok(())
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}

#[cfg(test)]
mod tests;
