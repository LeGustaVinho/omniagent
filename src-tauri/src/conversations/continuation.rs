use super::{
    Conversation, MessageRevision, MessageRole, SessionScope, activate_context,
    automatic_continuity, check_idle_base, consume_matching_draft, context_entries,
    insert_conversation, insert_message, load_conversation, load_messages, new_id,
    previous_attempt, reference_delivery, draft_files::{consume_attachment_draft, verify_attachment_draft},
};
use crate::{
    attachments::{AttachmentDelivery, AttachmentSource, CapturedPayload, captured_for_revision},
    jobs::{Job, SubagentAllocation, direct_subagent_allocation, queue_conversation_continuation},
    protection,
    providers::SessionMode,
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationChoice {
    #[default]
    SameTab,
    ForkWithContext,
    Fresh,
}

impl ContinuationChoice {
    fn as_sql(self) -> &'static str {
        match self {
            Self::SameTab => "same_tab",
            Self::ForkWithContext => "fork_with_context",
            Self::Fresh => "fresh",
        }
    }

    fn read(value: Option<String>) -> Result<Option<Self>> {
        match value.as_deref() {
            None => Ok(None),
            Some("same_tab") => Ok(Some(Self::SameTab)),
            Some("fork_with_context") => Ok(Some(Self::ForkWithContext)),
            Some("fresh") => Ok(Some(Self::Fresh)),
            _ => bail!("Unknown conversation continuation setting"),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationLevel {
    Project,
    Conversation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuationSettings {
    pub project_default: ContinuationChoice,
    pub conversation_override: Option<ContinuationChoice>,
    pub effective: ContinuationChoice,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationReason {
    ProfileChanged,
    CheckoutChanged,
    SessionUnavailable,
    PolicyChanged,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedAttachment {
    name: String,
    sha256: String,
    size: u64,
    delivery: AttachmentDelivery,
    excerpt: Option<String>,
    reference_target: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedMessage {
    revision_id: String,
    role: MessageRole,
    text: String,
    attachments: Vec<SeedAttachment>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ContinuationInspection {
    Automatic,
    Queued {
        job: Job,
    },
    ReviewRequired {
        preview_id: String,
        context_id: String,
        reason: ContinuationReason,
        settings: ContinuationSettings,
        seed: Vec<SeedMessage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_checkout: Option<crate::repository::ProjectCheckout>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinuedConversation {
    pub conversation: Conversation,
    pub job: Job,
}

#[derive(Deserialize, Serialize)]
struct SealedReview {
    scope: SessionScope,
    seed_digest: [u8; 32],
    #[serde(default)]
    reference_ids: Vec<String>,
    #[serde(default)]
    attachment_preview_id: Option<String>,
    #[serde(default)]
    target_checkout_id: Option<String>,
}

enum ContinuationRoute {
    Queue,
    Empty,
    Native(String),
    Review(ContinuationReason),
}

impl Store {
    pub async fn conversation_continuation_settings(
        &self,
        conversation_id: String,
    ) -> Result<ContinuationSettings> {
        self.execute(move |db| {
            let conversation = load_conversation(db, &conversation_id)?;
            let (project, _) = checkout(db, &conversation)?;
            settings(db, &project, &conversation_id)
        })
        .await
    }

    pub async fn set_conversation_continuation_policy(
        &self,
        conversation_id: String,
        level: ContinuationLevel,
        choice: Option<ContinuationChoice>,
    ) -> Result<ContinuationSettings> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = load_conversation(&tx, &conversation_id)?;
            let (project, _) = checkout(&tx, &conversation)?;
            match (level, choice.map(ContinuationChoice::as_sql)) {
                (ContinuationLevel::Project, Some(choice)) => {
                    tx.execute(
                        "INSERT INTO project_continuation_policies VALUES (?1, ?2)
                         ON CONFLICT(project_id) DO UPDATE SET choice = excluded.choice",
                        params![project, choice],
                    )?;
                }
                (ContinuationLevel::Project, None) => {
                    tx.execute(
                        "DELETE FROM project_continuation_policies WHERE project_id = ?1",
                        [&project],
                    )?;
                }
                (ContinuationLevel::Conversation, Some(choice)) => {
                    tx.execute(
                        "INSERT INTO conversation_continuation_policies VALUES (?1, ?2)
                         ON CONFLICT(conversation_id) DO UPDATE SET choice = excluded.choice",
                        params![conversation_id, choice],
                    )?;
                }
                (ContinuationLevel::Conversation, None) => {
                    tx.execute(
                        "DELETE FROM conversation_continuation_policies WHERE conversation_id = ?1",
                        [&conversation_id],
                    )?;
                }
            };
            let current = settings(&tx, &project, &conversation_id)?;
            tx.commit()?;
            Ok(current)
        })
        .await
    }

    pub async fn inspect_conversation_continuation(
        &self,
        conversation_id: String,
        expected_context_id: String,
        scope: SessionScope,
    ) -> Result<ContinuationInspection> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = load_conversation(&tx, &conversation_id)?;
            let (project, root) = checkout(&tx, &conversation)?;
            validate_scope(&scope, &root)?;
            let inspected =
                match continuation_route(&tx, &conversation, &expected_context_id, &scope)? {
                    ContinuationRoute::Review(reason) => {
                        seal_review(&tx, &project, &conversation, scope, reason, Vec::new(), None, None)?
                    }
                    _ => ContinuationInspection::Automatic,
                };
            tx.commit()?;
            Ok(inspected)
        })
        .await
    }

    pub async fn submit_conversation_continuation(
        &self,
        conversation_id: String,
        expected_context_id: String,
        scope: SessionScope,
        text: String,
    ) -> Result<ContinuationInspection> {
        self.submit_conversation_with_references(conversation_id, expected_context_id, scope, text, Vec::new()).await
    }

    pub async fn submit_conversation_with_references(
        &self,
        conversation_id: String,
        expected_context_id: String,
        scope: SessionScope,
        text: String,
        reference_ids: Vec<String>,
    ) -> Result<ContinuationInspection> {
        self.submit_conversation_with_files(conversation_id, expected_context_id, scope, text, reference_ids, None).await
    }

    pub async fn submit_conversation_with_files(
        &self,
        conversation_id: String,
        expected_context_id: String,
        scope: SessionScope,
        text: String,
        reference_ids: Vec<String>,
        attachment_preview_id: Option<String>,
    ) -> Result<ContinuationInspection> {
        self.submit_conversation_with_files_at_checkout(conversation_id, expected_context_id, scope, text,
            reference_ids, attachment_preview_id, None).await
    }

    pub async fn submit_conversation_with_files_at_checkout(
        &self,
        conversation_id: String,
        expected_context_id: String,
        scope: SessionScope,
        text: String,
        reference_ids: Vec<String>,
        attachment_preview_id: Option<String>,
        requested_checkout_id: Option<String>,
    ) -> Result<ContinuationInspection> {
        let attachments = self.prepare_attachment_draft_submission(
            conversation_id.clone(), &scope, &text, attachment_preview_id.clone(),
        ).await?;
        let outcome = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let conversation = load_conversation(&tx, &conversation_id)?;
                let target = super::fork_checkout::resolve_continuation_checkout(&tx, &conversation, requested_checkout_id.as_deref())?;
                let project = target.project_id;
                validate_scope(&scope, &target.root)?;
                let moving = target.checkout_id != conversation.checkout_id;
                if moving {
                    let draft_has_files: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM conversation_attachment_drafts WHERE conversation_id = ?1)", [&conversation_id], |row| row.get(0))?;
                    ensure!(!draft_has_files || attachments.is_some(), "Review the draft files in the destination checkout before continuing");
                    check_idle_base(&tx, &conversation_id, &expected_context_id)?;
                    ensure!(!busy(&tx, &conversation)?, "Wait for pending work to settle before changing checkouts");
                }
                ensure!(attachments.as_ref().is_none_or(|draft| draft.checkout_id == target.checkout_id),
                    "Review the draft files in the destination checkout before continuing");
                verify_attachment_draft(&tx, &conversation_id, attachments.as_ref())?;
                reference_delivery::verify_draft_references(&tx, &conversation_id, &project, &reference_ids)?;
                let route = if moving { ContinuationRoute::Review(ContinuationReason::CheckoutChanged) }
                    else { continuation_route(&tx, &conversation, &expected_context_id, &scope)? };
                if let ContinuationRoute::Review(reason) = route {
                    let reviewed = seal_review(&tx, &project, &conversation, scope, reason, reference_ids, attachment_preview_id,
                        moving.then_some(target.checkout_id))?;
                    tx.commit()?;
                    return Ok(reviewed);
                }
                consume_matching_draft(&tx, &conversation_id, &text)?;
                reference_delivery::consume_draft_references(&tx, &conversation_id)?;
                consume_attachment_draft(&tx, &conversation_id)?;
                let job = queue_conversation_continuation(
                    &tx,
                    project,
                    &conversation,
                    &expected_context_id,
                    &scope,
                    &text,
                    &reference_ids,
                    attachments.as_ref().map(|draft| &draft.files),
                )?;
                match route {
                    ContinuationRoute::Queue => (),
                    ContinuationRoute::Empty => {
                        pin_continuation(&tx, &job.id, &conversation, None)?
                    }
                    ContinuationRoute::Native(session) => {
                        pin_continuation(&tx, &job.id, &conversation, Some(&session))?
                    }
                    ContinuationRoute::Review(_) => unreachable!("review returns without queuing"),
                }
                tx.execute(
                    "DELETE FROM conversation_continuation_previews WHERE conversation_id = ?1",
                    [&conversation_id],
                )?;
                tx.commit()?;
                Ok(ContinuationInspection::Queued { job })
            })
            .await?;
        if matches!(&outcome, ContinuationInspection::Queued { .. }) {
            self.job_signal.send_replace(());
        }
        Ok(outcome)
    }

    pub async fn confirm_conversation_continuation(
        &self,
        conversation_id: String,
        preview_id: String,
        choice: ContinuationChoice,
        scope: SessionScope,
        text: String,
    ) -> Result<ContinuedConversation> {
        let attachment_preview_id = self.execute({
            let conversation_id = conversation_id.clone(); let preview_id = preview_id.clone();
            move |db| Ok(load_continuation_review(db, &conversation_id, &preview_id)?.1.attachment_preview_id)
        }).await?;
        let attachments = self.prepare_attachment_draft_submission(
            conversation_id.clone(), &scope, &text, attachment_preview_id,
        ).await?;
        let continued = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (context_id, preview) = load_continuation_review(&tx, &conversation_id, &preview_id)?;
            let source = check_idle_base(&tx, &conversation_id, &context_id)?;
            ensure!(!busy(&tx, &source)?, "The conversation has pending work. Review the continuation after it settles.");
            let target = super::fork_checkout::resolve_continuation_checkout(&tx, &source, preview.target_checkout_id.as_deref())?;
            let project = target.project_id;
            validate_scope(&scope, &target.root)?;
            ensure!(preview.scope == scope, "The selected profile changed. Review the continuation again.");
            ensure!(preview.attachment_preview_id.as_deref() == attachments.as_ref().map(|draft| draft.preview_id.as_str()),
                "The reviewed files changed. Review the continuation again.");
            ensure!(attachments.as_ref().is_none_or(|draft| draft.checkout_id == target.checkout_id),
                "Review the draft files in the destination checkout before continuing");
            verify_attachment_draft(&tx, &conversation_id, attachments.as_ref())?;
            reference_delivery::verify_draft_references(&tx, &conversation_id, &project, &preview.reference_ids)?;
            let entries = context_entries(&tx, &conversation_id, &context_id)?;
            ensure!(
                preview.seed_digest == protected_history_digest(&tx, &source)?,
                "The reviewed history changed. Review the continuation again."
            );
            if target.checkout_id != source.checkout_id && choice != ContinuationChoice::Fresh {
                require_destination_seed_references(&tx, &entries, &target.root)?;
            }
            consume_matching_draft(&tx, &source.id, &text)?;
            reference_delivery::consume_draft_references(&tx, &source.id)?;
            consume_attachment_draft(&tx, &source.id)?;
            let conversation = match choice {
                ContinuationChoice::SameTab => {
                    let mut same = source;
                    if target.checkout_id != same.checkout_id {
                        tx.execute("UPDATE conversations SET checkout_id = ?2 WHERE id = ?1", params![same.id, target.checkout_id])?;
                        same.checkout_id = target.checkout_id;
                    }
                    same
                }
                ContinuationChoice::ForkWithContext => fork_history_at_checkout(&tx, &source, &entries, target.checkout_id, format!("{} (fork)", source.title))?,
                ContinuationChoice::Fresh => insert_conversation(&tx, target.checkout_id, "New conversation".into(), false)?,
            };
            let job = queue_conversation_continuation(
                &tx, project, &conversation, &conversation.context_id, &scope, &text, &preview.reference_ids,
                attachments.as_ref().map(|draft| &draft.files),
            )?;
            pin_continuation(&tx, &job.id, &conversation, None)?;
            tx.execute("DELETE FROM conversation_continuation_previews WHERE id = ?1", [&preview_id])?;
            tx.commit()?;
            Ok(ContinuedConversation { conversation, job })
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(continued)
    }

    pub async fn conversation_continuation_review_checkout(&self, conversation_id: String, preview_id: String) -> Result<String> {
        self.execute(move |db| {
            let source = load_conversation(db, &conversation_id)?;
            let (_, review) = load_continuation_review(db, &conversation_id, &preview_id)?;
            Ok(super::fork_checkout::resolve_continuation_checkout(db, &source, review.target_checkout_id.as_deref())?.checkout_id)
        }).await
    }
}

fn require_destination_seed_references(db: &Connection, entries: &[String], root: &Path) -> Result<()> {
    for revision in entries {
        for attachment in captured_for_revision(db, revision)? {
            if let (AttachmentSource::Checkout { relative_path }, CapturedPayload::Reference { .. }) =
                (&attachment.metadata.source, &attachment.payload) {
                attachment.validate_reference(root).with_context(|| format!(
                    "The saved reference {} cannot be used in the destination checkout. Choose a fresh conversation to continue with this draft.",
                    relative_path.display()))?;
            }
        }
    }
    Ok(())
}

fn load_continuation_review(db: &Connection, conversation_id: &str, preview_id: &str) -> Result<(String, SealedReview)> {
    let (context_id, payload): (String, Vec<u8>) = db.query_row(
        "SELECT context_id, payload FROM conversation_continuation_previews WHERE id = ?1 AND conversation_id = ?2",
        params![preview_id, conversation_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.context("This continuation preview is no longer available. Review the conversation again.")?;
    Ok((context_id, serde_json::from_slice(&protection::unprotect(&payload)?)?))
}

fn continuation_route(
    db: &Connection,
    conversation: &Conversation,
    expected_context_id: &str,
    scope: &SessionScope,
) -> Result<ContinuationRoute> {
    if busy(db, conversation)? {
        return Ok(ContinuationRoute::Queue);
    }
    ensure!(
        conversation.context_id == expected_context_id,
        "The conversation changed. Review its latest history."
    );
    let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
    let Some(prior) = previous_attempt(db, &conversation.id)? else {
        return Ok(if entries.is_empty() {
            ContinuationRoute::Empty
        } else {
            ContinuationRoute::Review(ContinuationReason::SessionUnavailable)
        });
    };
    let serialized = serde_json::to_string(scope)?;
    if prior.scope != serialized {
        return Ok(ContinuationRoute::Review(
            ContinuationReason::ProfileChanged,
        ));
    }
    let allocation = direct_subagent_allocation(db, &conversation.id, None, scope)?;
    if !prior.matches_allocation(&allocation) {
        return Ok(ContinuationRoute::Review(ContinuationReason::PolicyChanged));
    }
    Ok(
        if automatic_continuity(db, &conversation.id, &entries, &prior, scope, &serialized)?
            .is_some()
        {
            ContinuationRoute::Native(prior.session_id)
        } else {
            ContinuationRoute::Review(ContinuationReason::SessionUnavailable)
        },
    )
}

fn seal_review(
    db: &Connection,
    project: &str,
    conversation: &Conversation,
    scope: SessionScope,
    reason: ContinuationReason,
    reference_ids: Vec<String>,
    attachment_preview_id: Option<String>,
    target_checkout_id: Option<String>,
) -> Result<ContinuationInspection> {
    let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
    let seed = preview_seed(db, conversation, &entries)?;
    let preview_id = new_id();
    let target_checkout = target_checkout_id.as_deref().map(|target| db.query_row(
        "SELECT p.id, p.root, c.id, c.root FROM checkouts c JOIN projects p ON p.id = c.project_id WHERE c.id = ?1 AND p.id = ?2",
        params![target,project], |row| Ok(crate::repository::ProjectCheckout { project_id: row.get(0)?, repository_root: row.get(1)?,
            checkout_id: row.get(2)?, checkout_root: row.get(3)? }),
    )).transpose()?;
    let payload = protection::protect(&serde_json::to_vec(&SealedReview {
        scope,
        seed_digest: protected_history_digest(db, conversation)?,
        reference_ids,
        attachment_preview_id,
        target_checkout_id,
    })?)?;
    db.execute(
        "INSERT INTO conversation_continuation_previews VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(conversation_id) DO UPDATE SET id = excluded.id, context_id = excluded.context_id, payload = excluded.payload",
        params![preview_id, conversation.id, conversation.context_id, payload],
    )?;
    Ok(ContinuationInspection::ReviewRequired {
        preview_id,
        context_id: conversation.context_id.clone(),
        reason,
        settings: settings(db, project, &conversation.id)?,
        seed,
        target_checkout,
    })
}

fn pin_continuation(
    db: &Connection,
    job: &str,
    conversation: &Conversation,
    native: Option<&str>,
) -> Result<()> {
    let fingerprint = protected_history_digest(db, conversation)?;
    db.execute(
        "INSERT INTO conversation_continuation_jobs VALUES (?1, ?2, ?3)",
        params![job, fingerprint.as_slice(), native],
    )?;
    Ok(())
}

pub(crate) fn pinned_continuation(db: &Connection, job: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_continuation_jobs WHERE job_id = ?1)",
        [job],
        |row| row.get(0),
    )?)
}

pub(crate) struct PinnedContinuation {
    seed_digest: Vec<u8>,
    pub(crate) native_session_id: Option<String>,
}

pub(crate) fn continuation_pin(db: &Connection, job: &str) -> Result<Option<PinnedContinuation>> {
    Ok(db.query_row(
        "SELECT seed_digest, native_session_id FROM conversation_continuation_jobs WHERE job_id = ?1",
        [job],
        |row| Ok(PinnedContinuation { seed_digest: row.get(0)?, native_session_id: row.get(1)? }),
    ).optional()?)
}

impl PinnedContinuation {
    pub(crate) fn history_is_current(
        &self,
        db: &Connection,
        conversation: &Conversation,
    ) -> Result<bool> {
        match protected_history_digest(db, conversation) {
            Ok(current) => Ok(self.seed_digest == current),
            Err(error)
                if matches!(
                    error.downcast_ref(),
                    Some(rusqlite::Error::QueryReturnedNoRows)
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn native_is_available(
        &self,
        db: &Connection,
        conversation: &Conversation,
        scope: &SessionScope,
        allocation: &SubagentAllocation,
    ) -> Result<bool> {
        let Some(session) = &self.native_session_id else {
            return Ok(true);
        };
        let Some(prior) = previous_attempt(db, &conversation.id)? else {
            return Ok(false);
        };
        if &prior.session_id != session || !prior.matches_allocation(allocation) {
            return Ok(false);
        }
        let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
        Ok(automatic_continuity(
            db,
            &conversation.id,
            &entries,
            &prior,
            scope,
            &serde_json::to_string(scope)?,
        )?
        .is_some())
    }
}

fn checkout(db: &Connection, conversation: &Conversation) -> Result<(String, PathBuf)> {
    Ok(db.query_row(
        "SELECT project_id, root FROM checkouts WHERE id = ?1",
        [&conversation.checkout_id],
        |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
    )?)
}

fn validate_scope(scope: &SessionScope, root: &Path) -> Result<()> {
    ensure!(
        scope.checkout == root,
        "The selected profile is outside the conversation checkout"
    );
    ensure!(
        matches!(scope.mode, SessionMode::ReadOnly | SessionMode::Plan),
        "Conversation continuation requires a read-only or planning profile"
    );
    ensure!(
        scope.executable.is_absolute() && !scope.runner.is_empty(),
        "Invalid CLI profile"
    );
    Ok(())
}

fn settings(db: &Connection, project: &str, conversation: &str) -> Result<ContinuationSettings> {
    let project_default = ContinuationChoice::read(
        db.query_row(
            "SELECT choice FROM project_continuation_policies WHERE project_id = ?1",
            [project],
            |row| row.get(0),
        )
        .optional()?,
    )?
    .unwrap_or_default();
    let conversation_override = ContinuationChoice::read(
        db.query_row(
            "SELECT choice FROM conversation_continuation_policies WHERE conversation_id = ?1",
            [conversation],
            |row| row.get(0),
        )
        .optional()?,
    )?;
    Ok(ContinuationSettings {
        project_default,
        conversation_override,
        effective: conversation_override.unwrap_or(project_default),
    })
}

fn busy(db: &Connection, conversation: &Conversation) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1
             UNION ALL SELECT 1 FROM unconfirmed_agent_stops WHERE conversation_id = ?1
             UNION ALL SELECT 1 FROM open_agent_questions WHERE conversation_id = ?1 AND context_id = ?2
               AND state IN ('saved', 'needs_review')
             UNION ALL SELECT 1 FROM jobs job JOIN queued_conversation_instructions instruction ON instruction.job_id = job.id
               WHERE job.conversation_id = ?1 AND job.queue_state <> 'cancelled'
                 AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE job_id = job.id)
                 AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = job.id)
                      OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = job.id))
         )",
        params![conversation.id, conversation.context_id],
        |row| row.get(0),
    )?)
}

fn preview_seed(
    db: &Connection,
    conversation: &Conversation,
    entries: &[String],
) -> Result<Vec<SeedMessage>> {
    load_messages(db, &conversation.id, entries)?
        .into_iter()
        .map(
            |MessageRevision {
                 revision_id,
                 role,
                 text,
                 ..
             }| {
                let attachments = captured_for_revision(db, &revision_id)?
                    .into_iter()
                    .map(|file| {
                        let delivery = file.delivery();
                        let metadata = file.metadata;
                        let (excerpt, reference_target) = match file.payload {
                            CapturedPayload::Native { .. } => (None, None),
                            CapturedPayload::Excerpt { excerpt } => (Some(excerpt.text), None),
                            CapturedPayload::Reference { reference_path } => {
                                (None, Some(reference_path))
                            }
                        };
                        SeedAttachment {
                            name: metadata.source.file_name().unwrap_or("Attachment").into(),
                            sha256: metadata.sha256,
                            size: metadata.size,
                            delivery,
                            excerpt,
                            reference_target,
                        }
                    })
                    .collect();
                Ok(SeedMessage {
                    revision_id,
                    role,
                    text,
                    attachments,
                })
            },
        )
        .collect()
}

fn protected_history_digest(db: &Connection, conversation: &Conversation) -> Result<[u8; 32]> {
    let revisions = context_entries(db, &conversation.id, &conversation.context_id)?;
    let mut hash = Sha256::new();
    let mut lookup = db.prepare(
        "SELECT m.role, r.content, a.payload FROM message_revisions r JOIN messages m ON m.id = r.message_id
         LEFT JOIN message_attachments a ON a.message_revision_id = r.id
         WHERE m.conversation_id = ?1 AND r.id = ?2",
    )?;
    for revision in revisions {
        let (role, content, attachment): (String, Vec<u8>, Option<Vec<u8>>) = lookup
            .query_row(params![conversation.id, revision], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        for bytes in [
            revision.as_bytes(),
            role.as_bytes(),
            &content,
            attachment.as_deref().unwrap_or_default(),
        ] {
            hash.update(bytes.len().to_le_bytes());
            hash.update(bytes);
        }
        for reference in reference_delivery::reference_ids(db, &revision)? {
            hash.update(reference.len().to_le_bytes());
            hash.update(reference.as_bytes());
        }
    }
    Ok(hash.finalize().into())
}

pub(super) fn fork_history(
    db: &Connection,
    source: &Conversation,
    entries: &[String],
    title: String,
) -> Result<Conversation> {
    fork_history_at_checkout(db, source, entries, source.checkout_id.clone(), title)
}

pub(super) fn fork_history_at_checkout(
    db: &Connection,
    source: &Conversation,
    entries: &[String],
    checkout_id: String,
    title: String,
) -> Result<Conversation> {
    let source_session: Option<String> = if source.checkout_id == checkout_id { db
        .query_row(
            "SELECT a.agent_session_id FROM agent_attempts a
         JOIN agent_sessions s ON s.id = a.agent_session_id
         JOIN conversation_contexts c ON c.id = a.result_context_id
         WHERE a.conversation_id = ?1 AND c.entries = ?2 AND s.provider_session_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM history_revoked_sessions WHERE session_id = s.id)
         ORDER BY a.ordinal DESC LIMIT 1",
            params![source.id, serde_json::to_string(entries)?],
            |row| row.get(0),
        )
        .optional()? } else { None };
    let mut fork = insert_conversation(db, checkout_id, title, false)?;
    let mut copied = Vec::with_capacity(entries.len());
    let mut read = db.prepare(
        "SELECT m.role, r.content FROM messages m JOIN message_revisions r ON r.message_id = m.id
         WHERE m.conversation_id = ?1 AND r.id = ?2",
    )?;
    for revision in entries {
        let (role, payload): (String, Vec<u8>) = read
            .query_row(params![source.id, revision], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        let role = if role == "user" {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        let copy = insert_message(db, &fork.id, role, payload)?;
        crate::dispatch::record_message_copy(db, revision, &copy)?;
        reference_delivery::copy_references(db, revision, &copy)?;
        db.execute(
            "INSERT INTO message_attachments (message_revision_id, payload)
             SELECT ?1, payload FROM message_attachments WHERE message_revision_id = ?2",
            params![copy, revision],
        )?;
        copied.push(copy);
    }
    if !copied.is_empty() {
        fork.context_id = activate_context(db, &fork.id, &copied)?;
    }
    db.execute(
        "INSERT INTO conversation_forks (conversation_id, source_conversation_id,
            source_context_id, source_revision_id, source_session_id) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            fork.id,
            source.id,
            source.context_id,
            entries.last(),
            source_session
        ],
    )?;
    Ok(fork)
}

#[cfg(test)]
mod tests;
