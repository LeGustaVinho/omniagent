use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    attachments::{self, AttachmentPreview},
    conversations::{MessageRole, context_entries, insert_message, load_messages},
    protection,
    providers::{Provider, SessionMode},
    repository,
    store::Store,
};

use super::{
    AdmissionBlocker, AdmissionEnvironment, Job, JobRequest, QueueState, admission::inspect,
    change_pending_job_state, load_job,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum InstructionDelivery {
    Queued,
    Paused,
    Delivering,
    NeedsReview,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct PendingInstruction {
    pub id: String,
    pub revision_id: String,
    pub text: String,
    pub checkout_id: String,
    pub context_id: String,
    pub provider: Provider,
    pub model: Option<String>,
    pub mode: SessionMode,
    pub attachments: Vec<String>,
    pub delivery: InstructionDelivery,
    pub editable: bool,
    pub run_id: Option<String>,
    pub recorded_context: bool,
    pub blockers: Vec<AdmissionBlocker>,
}

#[derive(Debug)]
pub(super) struct QueuedRevision {
    pub revision_id: String,
}

pub(super) fn load_revision(db: &Connection, job: &str) -> Result<Option<QueuedRevision>> {
    Ok(db
        .query_row(
            "SELECT revision_id FROM queued_conversation_instructions WHERE job_id = ?1",
            [job],
            |row| {
                Ok(QueuedRevision {
                    revision_id: row.get(0)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn record_instruction(
    db: &Connection,
    job_id: &str,
    conversation: &str,
    text: &str,
    attachments: Option<&FrozenAttachments>,
) -> Result<()> {
    let revision = insert_message(
        db,
        conversation,
        MessageRole::User,
        protection::protect(text.as_bytes())?,
    )?;
    db.execute(
        "INSERT INTO queued_conversation_instructions(job_id, message_id, revision_id, attachment_labels)
         SELECT ?1, message_id, id, ?3 FROM message_revisions WHERE id = ?2",
        params![job_id, revision, attachments.map(|files| &files.labels)],
    )?;
    save_attachments(db, &revision, attachments)
}

fn save_attachments(
    db: &Connection,
    revision: &str,
    files: Option<&FrozenAttachments>,
) -> Result<()> {
    if let Some(files) = files {
        db.execute(
            "INSERT INTO message_attachments(message_revision_id, payload) VALUES (?1, ?2)",
            params![revision, files.payload],
        )?;
    }
    Ok(())
}

pub(super) struct FrozenAttachments {
    payload: Vec<u8>,
    labels: Vec<u8>,
}

pub(super) async fn freeze_attachments(
    store: &Store,
    checkout: String,
    root: &Path,
    previews: Vec<AttachmentPreview>,
) -> Result<Option<FrozenAttachments>> {
    if previews.is_empty() {
        return Ok(None);
    }
    let layout = repository::verify_checkout(store, checkout.clone(), root.into()).await?;
    let limits = store.checkout_attachment_limits(checkout).await?;
    tokio::task::spawn_blocking(move || {
        let captured = attachments::capture(&layout.root, previews, limits)?;
        let names = captured
            .iter()
            .map(|file| file.metadata.source.file_name().unwrap_or("Attachment"))
            .collect::<Vec<_>>();
        Ok(Some(FrozenAttachments {
            payload: protection::protect(&serde_json::to_vec(&captured)?)?,
            labels: protection::protect(&serde_json::to_vec(&names)?)?,
        }))
    })
    .await?
}

pub(super) fn predecessor(db: &Connection, job: &Job) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT j.id FROM jobs current JOIN jobs j
           ON j.conversation_id = current.conversation_id AND j.sequence < current.sequence
         JOIN queued_conversation_instructions earlier ON earlier.job_id = j.id
         WHERE current.id = ?1 AND j.queue_state <> 'cancelled'
           AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE job_id = j.id)
           AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
                OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id))
           AND (EXISTS (SELECT 1 FROM job_history_bindings WHERE job_id = j.id)) =
               (EXISTS (SELECT 1 FROM job_history_bindings WHERE job_id = current.id))
         ORDER BY j.sequence LIMIT 1",
            [&job.id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn preserves_destination(
    db: &Connection,
    conversation: &str,
    anchor: &str,
    active: &str,
) -> Result<bool> {
    if anchor == active {
        return Ok(true);
    }
    let same_history: bool = db.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM conversation_contexts original JOIN conversation_contexts selected
               ON selected.conversation_id = original.conversation_id
                 AND selected.history_generation = original.history_generation
             WHERE original.conversation_id = ?1 AND original.id = ?2 AND selected.id = ?3
         )",
        params![conversation, anchor, active],
        |row| row.get(0),
    )?;
    if !same_history {
        return Ok(false);
    }
    let original = context_entries(db, conversation, anchor)?;
    let current = context_entries(db, conversation, active)?;
    Ok(current.starts_with(&original))
}

pub(super) fn parked_owner(
    db: &Connection,
    conversation: &str,
    context: &str,
) -> Result<Option<String>> {
    Ok(db.query_row(
        "SELECT a.run_id
         FROM job_attempts j
         JOIN agent_attempts a ON a.run_id = j.run_id
         JOIN jobs job ON job.id = j.job_id
         LEFT JOIN job_pipeline_origins origin ON origin.job_id = j.job_id
         LEFT JOIN job_pipeline_runs pipeline ON pipeline.id = origin.pipeline_run_id AND pipeline.project_id = job.project_id
         LEFT JOIN job_history_bindings binding ON binding.job_id = job.id
         WHERE a.conversation_id = ?1 AND job.queue_state <> 'cancelled'
           AND (
               (pipeline.id IS NOT NULL AND origin.plan_revision IS pipeline.plan_revision)
               OR EXISTS (SELECT 1 FROM goal_continuations c JOIN active_goals g ON g.revision_id = c.revision_id
                          WHERE c.job_id = job.id)
           )
           AND j.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = job.id)
           AND (a.result_context_id = ?2 OR binding.context_id = ?2)
           AND (
               EXISTS (SELECT 1 FROM job_downstream_waits WHERE parent_run_id = a.run_id
                   AND state IN ('yielding', 'waiting', 'needs_review'))
               OR EXISTS (SELECT 1 FROM open_agent_questions WHERE run_id = a.run_id)
           )
         ORDER BY job.sequence LIMIT 1",
        params![conversation, context],
        |row| row.get(0),
    ).optional()?)
}

pub(super) fn fits_continuation(
    db: &Connection,
    instruction: &Job,
    conversation: &str,
    boundary: &str,
    revision: &str,
) -> Result<bool> {
    let anchor = instruction
        .context
        .as_ref()
        .context("Instruction has no destination")?;
    let candidate_history = context_entries(db, conversation, &anchor.context_id)?;
    let receiving_history = context_entries(db, conversation, boundary)?;
    if !receiving_history.starts_with(&candidate_history)
        || receiving_history.iter().any(|id| id == revision)
    {
        return Ok(false);
    }
    let mut files = attachments::captured_for_revision(db, revision)?;
    if files.is_empty() {
        return Ok(true);
    }
    for prior in receiving_history {
        files.extend(attachments::captured_for_revision(db, &prior)?);
    }
    Ok(attachments::check_limits(
        attachments::limits_for_checkout(db, &instruction.checkout_id)?,
        files.iter().map(|item| (&item.metadata, item.delivery())),
    )
    .is_ok())
}

struct EditableInstruction {
    project: String,
    message: String,
}

fn editable(
    db: &Connection,
    conversation: &str,
    id: &str,
    expected: &str,
) -> Result<EditableInstruction> {
    db.query_row(
        "SELECT j.project_id, i.message_id FROM queued_conversation_instructions i JOIN jobs j ON j.id = i.job_id
         WHERE j.conversation_id = ?1 AND i.job_id = ?2 AND i.revision_id = ?3
           AND j.queue_state IN ('queued', 'paused')
           AND NOT EXISTS (SELECT 1 FROM instruction_delivery_attempts WHERE job_id = j.id AND superseded = 0)
           AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)",
        params![conversation, id, expected],
        |row| Ok(EditableInstruction { project: row.get(0)?, message: row.get(1)? }),
    ).optional()?.context("The instruction changed or was already claimed")
}

impl Store {
    pub async fn revise_pending_instruction(
        &self,
        conversation: String,
        id: String,
        expected: String,
        text: String,
        previews: Option<Vec<AttachmentPreview>>,
    ) -> Result<()> {
        ensure!(!text.trim().is_empty(), "The instruction is empty");
        let attachments = if let Some(previews) = previews {
            let destination = self.conversation(conversation.clone()).await?;
            let root = repository::checkout_root(self, destination.checkout_id.clone()).await?;
            Some(freeze_attachments(self, destination.checkout_id, &root, previews).await?)
        } else {
            None
        };
        let encrypted = protection::protect(text.as_bytes())?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let instruction = editable(&tx, &conversation, &id, &expected)?;
            let revision = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO message_revisions(id, message_id, revision, content)
                 SELECT ?1, ?2, max(revision) + 1, ?3 FROM message_revisions WHERE message_id = ?2",
                params![revision, instruction.message, encrypted],
            )?;
            match attachments {
                Some(files) => {
                    save_attachments(&tx, &revision, files.as_ref())?;
                    tx.execute("UPDATE queued_conversation_instructions SET attachment_labels = ?2 WHERE job_id = ?1",
                        params![id, files.as_ref().map(|files| &files.labels)])?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO message_attachments(message_revision_id, payload)
                         SELECT ?1, payload FROM message_attachments WHERE message_revision_id = ?2",
                        params![revision, expected],
                    )?;
                }
            }
            tx.execute("UPDATE queued_conversation_instructions SET revision_id = ?2 WHERE job_id = ?1",
                params![id, revision])?;
            let job = load_job(&tx, &instruction.project, &id)?.context("Instruction lost its job")?;
            if super::policy::effective_policy(&tx, &job)?.permission == super::JobPermission::Review {
                tx.execute("DELETE FROM job_approvals WHERE job_id = ?1", [&id])?;
                super::policy::reconcile_approval(&tx, &job)?;
            }
            tx.commit()?;
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn remove_pending_instruction(
        &self,
        conversation: String,
        id: String,
        expected: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let instruction = editable(&tx, &conversation, &id, &expected)?;
            change_pending_job_state(&tx, &instruction.project, &id, QueueState::Cancelled)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn pending_conversation_instructions(
        &self,
        conversation: String,
        env: AdmissionEnvironment,
    ) -> Result<Vec<PendingInstruction>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let mut statement = tx.prepare(
                "SELECT j.project_id, j.id, i.revision_id, coalesce(h.context_id, j.context_id), h.job_id IS NOT NULL,
                        j.checkout_id, j.operator_scope, m.content, i.attachment_labels, coalesce(a.run_id, delivery.run_id),
                        CASE WHEN a.run_id IS NULL AND delivery.run_id IS NULL THEN j.queue_state
                             WHEN delivery.uncertain THEN 'needs_review'
                             WHEN EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = coalesce(a.run_id, delivery.run_id)) THEN 'delivering'
                             WHEN delivery.run_id IS NOT NULL THEN 'needs_review'
                             WHEN EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                                          WHERE s.run_id = a.run_id AND d.delivery <> 'submitted') THEN 'needs_review'
                             WHEN EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = j.id) THEN 'delivering'
                             ELSE 'settled' END
                 FROM jobs j JOIN queued_conversation_instructions i ON i.job_id = j.id
                 JOIN message_revisions m ON m.id = i.revision_id
                 LEFT JOIN job_history_bindings h ON h.job_id = j.id
                 LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id)
                 LEFT JOIN borrowed_instruction_deliveries delivery ON delivery.job_id = j.id
                 WHERE j.conversation_id = ?1 AND j.queue_state <> 'cancelled'
                   AND NOT EXISTS (SELECT 1 FROM job_attempts prior
                                   JOIN context_snapshots s ON s.run_id = prior.run_id JOIN dispatches d ON d.snapshot_id = s.id
                                   WHERE prior.job_id = j.id AND d.ordinal = 1 AND d.delivery = 'submitted')
                   AND coalesce(delivery.delivered, 0) = 0
                 ORDER BY j.sequence",
            )?;
            let mut rows = statement.query([&conversation])?;
            let mut instructions = Vec::new();
            while let Some(row) = rows.next()? {
                let delivery = match row.get_ref(10)?.as_str()? {
                    "queued" => InstructionDelivery::Queued,
                    "paused" => InstructionDelivery::Paused,
                    "delivering" => InstructionDelivery::Delivering,
                    "needs_review" => InstructionDelivery::NeedsReview,
                    _ => continue,
                };
                let project: String = row.get(0)?;
                let id: String = row.get(1)?;
                let job = load_job(&tx, &project, &id)?.context("Instruction lost its job")?;
                let scope: crate::conversations::SessionScope = serde_json::from_slice(&protection::unprotect(&row.get::<_, Vec<u8>>(6)?)?)?;
                let text = String::from_utf8(protection::unprotect(&row.get::<_, Vec<u8>>(7)?)?)?;
                let attachments = if let Some(payload) = row.get::<_, Option<Vec<u8>>>(8)? {
                    serde_json::from_slice(&protection::unprotect(&payload)?)?
                } else { Vec::new() };
                let run_id: Option<String> = row.get(9)?;
                instructions.push(PendingInstruction {
                    id, revision_id: row.get(2)?, context_id: row.get(3)?, recorded_context: row.get(4)?,
                    text, checkout_id: row.get(5)?, provider: scope.provider, model: scope.model, mode: scope.mode,
                    attachments, delivery, editable: run_id.is_none(), run_id,
                    blockers: inspect(&tx, job, &env)?.blockers,
                });
            }
            Ok(instructions)
        }).await
    }
}

pub(super) fn current_request(db: &Connection, job: &Job, encrypted: &[u8]) -> Result<JobRequest> {
    let mut request: JobRequest = serde_json::from_slice(&protection::unprotect(encrypted)?)?;
    if let Some(revision) = load_revision(db, &job.id)? {
        let conversation = &job
            .context
            .as_ref()
            .context("Instruction has no destination")?
            .conversation_id;
        let mut messages = load_messages(db, conversation, &[revision.revision_id])?;
        request.instructions = messages
            .pop()
            .context("Instruction revision is missing")?
            .text;
    }
    Ok(request)
}

pub(crate) fn migrate_pending_instructions(db: &Connection) -> Result<()> {
    let mut statement = db.prepare(
        "SELECT j.conversation_id, j.id, j.request FROM jobs j
         WHERE j.conversation_id IS NOT NULL AND j.origin_run_id IS NULL AND j.operator_scope IS NOT NULL
           AND j.task_id IS NULL AND j.queue_state <> 'cancelled'
           AND NOT EXISTS (SELECT 1 FROM job_pipeline_origins WHERE job_id = j.id)
           AND NOT EXISTS (SELECT 1 FROM job_commands WHERE job_id = j.id)
           AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = j.id)
         ORDER BY j.sequence",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (conversation, id, payload) in rows {
        let request: JobRequest = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        record_instruction(db, &id, &conversation, &request.instructions, None)?;
    }
    Ok(())
}
