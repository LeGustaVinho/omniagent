use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    dispatch::{
        ContextSnapshot, ConversationArtifactRevision, ConversationSelection, SelectedMessageSpan,
        enqueue_summary, load_conversation_artifact, reconcile_summary,
    },
    model_catalog::ModelSelection,
    protection,
    providers::{Capability, SessionMode, capabilities::CapabilitySubject},
    store::Store,
};

use super::{
    Conversation, MessageRevision, MessageRole, SessionScope, activate_context, check_idle_base,
    context_entries, load_conversation, load_messages, reference_delivery,
};

mod native;

pub(super) use native::recover as recover_native_compactions;
pub(crate) use native::{NativeCompactionPlan, native_session_compacted};

const RECENT_TURNS: usize = 2;
const SUMMARY_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub automatic: bool,
    pub threshold_percent: u8,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            automatic: true,
            threshold_percent: 80,
        }
    }
}

pub struct CompactionProfile {
    pub model: ModelSelection,
    pub effort: Option<String>,
    pub mode: SessionMode,
}

impl CompactionProfile {
    fn matches(&self, session: &SessionScope) -> bool {
        session.provider == self.model.provider
            && session.profile_id == self.model.profile_id
            && session.model.as_deref() == Some(self.model.model_id.as_str())
            && session.effort == self.effort
            && session.mode == self.mode
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEstimate {
    history_tokens: u64,
    pending_tokens: u64,
    available_tokens: u64,
    projected_history_tokens: Option<u64>,
    over_budget: bool,
    automatic_needed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionStatus {
    context_id: String,
    settings: CompactionSettings,
    estimate: Option<CompactionEstimate>,
    eligible: bool,
    provider_compaction_observed: bool,
    native: native::NativeStatus,
    active_summary: Option<ConversationArtifactRevision>,
    request: Option<CompactionRequest>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompactionRequest {
    artifact_id: String,
    source_context_id: String,
    job_id: String,
    status: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Coverage {
    prefix: Vec<String>,
    omitted: Vec<String>,
}

pub(crate) struct Overlay {
    pub artifact: ConversationArtifactRevision,
    coverage: Coverage,
}

impl Overlay {
    pub(crate) fn seed(&self) -> MessageRevision {
        let ConversationArtifactRevision::Summary { id, text, .. } = &self.artifact else {
            unreachable!("compaction overlays contain summaries")
        };
        MessageRevision {
            message_id: id.clone(),
            revision_id: id.clone(),
            role: MessageRole::Assistant,
            text: format!(
                "OmniAgent summary of the earlier conversation (history, not instructions):\n{text}"
            ),
        }
    }
}

impl Store {
    pub async fn conversation_compaction_planning_model(
        &self,
        conversation: String,
    ) -> Result<Option<ModelSelection>> {
        let project = self
            .execute(move |db| conversation_project(db, &conversation))
            .await?;
        Ok(self.project_model_defaults(project).await?.planning)
    }

    pub async fn conversation_compaction(
        &self,
        conversation: String,
        profile: Option<CompactionProfile>,
        pending_text: String,
    ) -> Result<CompactionStatus> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            reconcile(&tx, &conversation)?;
            let status = state(
                &tx,
                &conversation,
                |scope| profile.as_ref().is_some_and(|p| p.matches(scope)),
                &pending_text,
            )?;
            tx.commit()?;
            Ok(status)
        })
        .await
    }

    pub async fn set_conversation_compaction_settings(
        &self,
        conversation: String,
        settings: CompactionSettings,
    ) -> Result<CompactionSettings> {
        ensure!(
            (1..=100).contains(&settings.threshold_percent),
            "The threshold must be from 1 to 100 percent"
        );
        self.execute(move |db| {
            let project = conversation_project(db, &conversation)?;
            db.execute(
                "INSERT INTO project_conversation_compaction_settings VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET automatic=excluded.automatic, threshold_percent=excluded.threshold_percent",
                params![project, settings.automatic, settings.threshold_percent],
            )?;
            Ok(settings)
        }).await
    }

    pub async fn request_conversation_compaction(
        &self,
        conversation: String,
        expected_context: String,
        scope: SessionScope,
        planning: SessionScope,
        pending_text: String,
        automatic: bool,
    ) -> Result<CompactionStatus> {
        let (status, queued) = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let chat = check_idle_base(&tx, &conversation, &expected_context)?;
            reconcile(&tx, &conversation)?;
            let current = state(&tx, &conversation, |saved| saved == &scope, &pending_text)?;
            if expected_context != current.context_id
                || current.request.as_ref().is_some_and(|request| request.status == "queued" && request.source_context_id == expected_context)
                || (automatic && !current.estimate.as_ref().is_some_and(|estimate| estimate.automatic_needed))
            {
                tx.commit()?;
                return Ok((current, false));
            }
            ensure!(!busy(&tx, &conversation)?, "Wait for pending conversation work or questions to settle before compacting");
            ensure!(current.eligible, "There is no older dialogue to compact while retaining recent turns and pinned sources");
            let root: String = tx.query_row("SELECT root FROM checkouts WHERE id = ?1", [&chat.checkout_id], |row| row.get(0))?;
            ensure!(
                scope.checkout == std::path::Path::new(&root)
                    && planning.checkout == scope.checkout && planning.mode == SessionMode::ReadOnly,
                "The summary requires a read-only profile in this conversation checkout"
            );
            let entries = context_entries(&tx, &conversation, &expected_context)?;
            let messages = load_messages(&tx, &conversation, &entries)?;
            let coverage = compactable(&tx, &messages)?.context("There is no older dialogue to compact while retaining recent turns and pinned sources")?;
            let maximum = summary_limit(&messages, &coverage);
            ensure!(maximum > 0, "There is no reduction to be gained from compacting this context");
            let queued = enqueue_summary(&tx, ConversationSelection {
                conversation_id: conversation.clone(),
                context_id: expected_context.clone(),
                spans: coverage.omitted.iter().map(|revision| SelectedMessageSpan {
                    revision_id: revision.clone(), byte_range: None,
                }).collect(),
            }, planning, Some(maximum))?;
            tx.execute(
                "INSERT INTO conversation_compaction_requests
                 (artifact_id, conversation_id, source_context_id, summary_job_id, coverage, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'queued')",
                params![queued.request_id, conversation, expected_context, queued.job.id, serde_json::to_string(&coverage)?],
            )?;
            let status = state(&tx, &conversation, |saved| saved == &scope, &pending_text)?;
            tx.commit()?;
            Ok((status, true))
        }).await?;
        if queued {
            self.job_signal.send_replace(());
        }
        Ok(status)
    }
}

fn conversation_project(db: &Connection, conversation: &str) -> Result<String> {
    Ok(db.query_row(
        "SELECT checkout.project_id FROM conversations c JOIN checkouts checkout ON checkout.id = c.checkout_id
         WHERE c.id = ?1 AND c.background = 0",
        [conversation],
        |row| row.get(0),
    )?)
}

fn settings(db: &Connection, project: &str) -> Result<CompactionSettings> {
    Ok(db.query_row(
        "SELECT automatic, threshold_percent FROM project_conversation_compaction_settings WHERE project_id = ?1",
        [project],
        |row| Ok(CompactionSettings { automatic: row.get(0)?, threshold_percent: row.get(1)? }),
    ).optional()?.unwrap_or_default())
}

fn busy(db: &Connection, conversation: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1
           UNION ALL SELECT 1 FROM unconfirmed_agent_stops WHERE conversation_id = ?1
           UNION ALL SELECT 1 FROM open_agent_questions WHERE conversation_id = ?1 AND state IN ('saved', 'needs_review', 'live')
           UNION ALL SELECT 1 FROM jobs j WHERE j.conversation_id = ?1 AND j.queue_state <> 'cancelled'
             AND (NOT EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = j.id)
                  OR EXISTS (SELECT 1 FROM job_recoveries r WHERE r.job_id = j.id))
        )", [conversation], |row| row.get(0),
    )?)
}

fn compactable(db: &Connection, history: &[MessageRevision]) -> Result<Option<Coverage>> {
    let Some((boundary, _)) = history
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| message.role == MessageRole::User)
        .nth(RECENT_TURNS - 1)
    else {
        return Ok(None);
    };
    let mut selected = Coverage {
        prefix: Vec::new(),
        omitted: Vec::new(),
    };
    let mut evidence = db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM message_attachments WHERE message_revision_id = ?1
          UNION ALL SELECT 1 FROM message_conversation_references WHERE message_revision_id = ?1)",
    )?;
    for message in &history[..boundary] {
        selected.prefix.push(message.revision_id.clone());
        let pinned: bool = evidence.query_row([&message.revision_id], |row| row.get(0))?;
        if !pinned && !message.text.is_empty() {
            selected.omitted.push(message.revision_id.clone());
        }
    }
    Ok((!selected.omitted.is_empty()).then_some(selected))
}

fn summary_limit(messages: &[MessageRevision], coverage: &Coverage) -> usize {
    let bytes: usize = messages
        .iter()
        .filter(|message| coverage.omitted.contains(&message.revision_id))
        .map(|message| message.text.len())
        .sum();
    bytes
        .saturating_sub(128)
        .min(bytes / 2)
        .min(SUMMARY_MAX_BYTES)
}

fn recent_request(db: &Connection, conversation: &str) -> Result<Option<CompactionRequest>> {
    Ok(db.query_row(
        "SELECT artifact_id, source_context_id, summary_job_id, state, detail FROM conversation_compaction_requests
         WHERE conversation_id = ?1 ORDER BY sequence DESC LIMIT 1",
        [conversation],
        |row| Ok(CompactionRequest {
            artifact_id: row.get(0)?, source_context_id: row.get(1)?, job_id: row.get(2)?,
            status: row.get(3)?, detail: row.get(4)?,
        }),
    ).optional()?)
}

fn reconcile(db: &Connection, conversation: &str) -> Result<()> {
    let Some(request) =
        recent_request(db, conversation)?.filter(|request| request.status == "queued")
    else {
        return Ok(());
    };
    let chat = load_conversation(db, conversation)?;
    if chat.context_id != request.source_context_id {
        return finish(
            db,
            &request,
            "stale",
            Some("The conversation changed before compaction could be applied."),
        );
    }
    let (queue, status): (String, Option<String>) = db.query_row(
        "SELECT j.queue_state, (SELECT r.status FROM job_attempts a JOIN runs r ON r.id = a.run_id
         WHERE a.job_id = j.id ORDER BY a.ordinal DESC LIMIT 1) FROM jobs j WHERE j.id = ?1",
        [&request.job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if queue == "cancelled"
        || matches!(
            status.as_deref(),
            Some("failed" | "cancelled" | "interrupted")
        )
    {
        return finish(
            db,
            &request,
            "failed",
            Some("The read-only summary job did not finish successfully."),
        );
    }
    if status.as_deref() != Some("succeeded") || busy(db, conversation)? {
        return Ok(());
    }
    let stored: String = db.query_row(
        "SELECT coverage FROM conversation_compaction_requests WHERE artifact_id = ?1",
        [&request.artifact_id],
        |row| row.get(0),
    )?;
    let coverage: Coverage = serde_json::from_str(&stored)?;
    let entries = context_entries(db, conversation, &request.source_context_id)?;
    let messages = load_messages(db, conversation, &entries)?;
    let artifact = match reconcile_summary(db, conversation, &request.artifact_id) {
        Ok(Some(artifact @ ConversationArtifactRevision::Summary { .. })) => artifact,
        _ => {
            return finish(
                db,
                &request,
                "failed",
                Some("The summary could not be verified against its saved input."),
            );
        }
    };
    let ConversationArtifactRevision::Summary {
        text,
        covered,
        source_context_id,
        ..
    } = &artifact
    else {
        unreachable!()
    };
    if source_context_id != &request.source_context_id
        || !entries.starts_with(&coverage.prefix)
        || !covered
            .iter()
            .map(|part| &part.origin.revision_id)
            .eq(&coverage.omitted)
        || text.trim().is_empty()
        || text.len() > summary_limit(&messages, &coverage)
    {
        return finish(
            db,
            &request,
            "failed",
            Some("The summary did not reduce the reviewed source context."),
        );
    }
    let before = projected_messages(db, &chat, &entries)?;
    let projected = apply_projection(
        messages,
        &Overlay {
            artifact: artifact.clone(),
            coverage: coverage.clone(),
        },
    );
    if estimated(&projected)? >= estimated(&before)? {
        return finish(
            db,
            &request,
            "failed",
            Some("The summary did not reduce the previously active context."),
        );
    }
    // Preserve the full transcript with a new immutable context identity and a fresh provider seed.
    let compacted = activate_context(db, conversation, &entries)?;
    save_overlay(db, &compacted, artifact.id(), &coverage)?;
    db.execute(
        "UPDATE agent_sessions SET cursor_context_id = NULL WHERE conversation_id = ?1",
        [conversation],
    )?;
    finish(db, &request, "activated", None)
}

fn finish(
    db: &Connection,
    request: &CompactionRequest,
    state: &str,
    detail: Option<&str>,
) -> Result<()> {
    db.execute(
        "UPDATE conversation_compaction_requests SET state = ?2, detail = ?3 WHERE artifact_id = ?1 AND state = 'queued'",
        params![request.artifact_id, state, detail],
    )?;
    Ok(())
}

fn state(
    db: &Connection,
    conversation: &str,
    profile_matches: impl Fn(&SessionScope) -> bool,
    pending_text: &str,
) -> Result<CompactionStatus> {
    let chat = load_conversation(db, conversation)?;
    let project = conversation_project(db, conversation)?;
    let settings = settings(db, &project)?;
    let entries = context_entries(db, conversation, &chat.context_id)?;
    let history = load_messages(db, conversation, &entries)?;
    let overlay = load_overlay(db, &chat.context_id)?;
    let coverage = compactable(db, &history)?;
    let eligible = coverage.as_ref().is_some_and(|coverage| {
        summary_limit(&history, coverage) > 0
            && overlay
                .as_ref()
                .is_none_or(|overlay| &overlay.coverage != coverage)
    });
    let projected_history_tokens = eligible
        .then(|| {
            let coverage = coverage.as_ref().expect("eligible dialogue");
            let retained = history
                .iter()
                .filter(|message| !coverage.omitted.contains(&message.revision_id))
                .collect::<Vec<_>>();
            Ok::<_, anyhow::Error>(
                (serde_json::to_vec(&retained)?.len() + summary_limit(&history, coverage) + 256)
                    as u64,
            )
        })
        .transpose()?;
    let projected = match &overlay {
        Some(overlay) => apply_projection(history, overlay),
        None => history,
    };
    let snapshot = latest_observed_snapshot(db, conversation, &profile_matches)?;
    let assessment = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.capabilities.as_ref());
    let capacity = assessment
        .and_then(|profile| {
            profile
                .entries
                .iter()
                .find(|entry| entry.descriptor.subject == CapabilitySubject::AttachmentLimits)
        })
        .and_then(|entry| entry.evidence.as_ref())
        .filter(|evidence| evidence.verified())
        .and_then(|evidence| evidence.attachment_limits.as_ref())
        .and_then(|limits| limits.context.as_ref());
    let history_tokens = estimated(&projected)?;
    let pending_tokens = (pending_text.len() as u64).saturating_add(
        reference_delivery::draft_reference_bytes(db, conversation, &project)?,
    );
    let estimate = capacity.map(|context| {
        let available_tokens = context
            .window_tokens
            .saturating_sub(context.reserved_output_tokens)
            .saturating_sub(context.fixed_input_tokens);
        let remaining = available_tokens.saturating_sub(pending_tokens);
        CompactionEstimate {
            history_tokens,
            pending_tokens,
            available_tokens,
            projected_history_tokens,
            over_budget: history_tokens.saturating_add(pending_tokens) > available_tokens,
            automatic_needed: settings.automatic
                && eligible
                && remaining > 0
                && history_tokens.saturating_mul(100)
                    >= remaining.saturating_mul(u64::from(settings.threshold_percent)),
        }
    });
    let provider_compaction_observed = assessment.is_some_and(|profile| {
        profile.entries.iter().any(|entry| {
            matches!(
                entry.descriptor.subject,
                CapabilitySubject::Protocol {
                    capability: Capability::NativeCompaction | Capability::ManualNativeCompaction
                }
            ) && (entry.descriptor.observed.is_some()
                || entry
                    .evidence
                    .as_ref()
                    .is_some_and(|evidence| evidence.verified()))
        })
    });
    Ok(CompactionStatus {
        native: native::status(db, &chat, profile_matches)?,
        context_id: chat.context_id,
        settings,
        estimate,
        eligible,
        provider_compaction_observed,
        active_summary: overlay.map(|overlay| overlay.artifact),
        request: recent_request(db, conversation)?,
    })
}

fn latest_observed_snapshot(
    db: &Connection,
    conversation: &str,
    matches: impl Fn(&SessionScope) -> bool,
) -> Result<Option<ContextSnapshot>> {
    let mut query = db.prepare_cached(
        "SELECT session.scope, snapshot.payload FROM agent_attempts attempt
         JOIN agent_sessions session ON session.id = attempt.agent_session_id
         JOIN context_snapshots snapshot ON snapshot.run_id = attempt.run_id
         JOIN dispatches dispatch ON dispatch.snapshot_id = snapshot.id AND dispatch.ordinal = 1
         WHERE attempt.conversation_id = ?1 AND dispatch.delivery = 'submitted' AND NOT attempt.detached_context
         ORDER BY attempt.ordinal DESC LIMIT 32",
    )?;
    let mut rows = query.query([conversation])?;
    while let Some(row) = rows.next()? {
        let session: String = row.get(0)?;
        if !matches(&serde_json::from_str(&session)?) {
            continue;
        }
        let cipher: Vec<u8> = row.get(1)?;
        let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&cipher)?)?;
        if matches(&snapshot.scope) {
            return Ok(Some(snapshot));
        }
    }
    Ok(None)
}

fn estimated(messages: &[MessageRevision]) -> Result<u64> {
    if messages.is_empty() {
        return Ok(0);
    }
    Ok(serde_json::to_vec(messages)?.len() as u64)
}

pub(crate) fn load_overlay(db: &Connection, context: &str) -> Result<Option<Overlay>> {
    let Some((artifact_id, source, coverage)) = overlay_record(db, context)? else {
        return Ok(None);
    };
    let artifact = load_conversation_artifact(db, &source, &artifact_id)?
        .context("The compacted context refers to a removed summary. Review or rewind the conversation to choose another context.")?;
    ensure!(
        matches!(artifact, ConversationArtifactRevision::Summary { .. }),
        "The compacted context summary is invalid"
    );
    Ok(Some(Overlay { artifact, coverage }))
}

fn overlay_record(db: &Connection, context: &str) -> Result<Option<(String, String, Coverage)>> {
    let saved: Option<(String, String, String)> = db.query_row(
        "SELECT overlay.artifact_id, artifact.source_conversation_id, overlay.coverage
         FROM conversation_context_compactions overlay JOIN conversation_artifact_revisions artifact ON artifact.id = overlay.artifact_id
         WHERE overlay.context_id = ?1",
        [context], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    saved
        .map(|(artifact, source, coverage)| {
            Ok((artifact, source, serde_json::from_str(&coverage)?))
        })
        .transpose()
}

fn save_overlay(db: &Connection, context: &str, artifact: &str, coverage: &Coverage) -> Result<()> {
    db.execute(
        "INSERT INTO conversation_context_compactions VALUES (?1, ?2, ?3)
         ON CONFLICT(context_id) DO UPDATE SET artifact_id=excluded.artifact_id, coverage=excluded.coverage",
        params![context, artifact, serde_json::to_string(coverage)?],
    )?;
    Ok(())
}

pub(super) fn inherit(
    db: &Connection,
    source: &str,
    destination: &str,
    entries: &[String],
) -> Result<()> {
    if let Some((artifact_id, _, coverage)) = overlay_record(db, source)?
        && entries.starts_with(&coverage.prefix)
    {
        save_overlay(db, destination, &artifact_id, &coverage)?;
    }
    Ok(())
}

pub(super) fn inherit_fork(
    db: &Connection,
    source: &str,
    destination: &str,
    entries: &[String],
    copied: &[String],
) -> Result<()> {
    let Some((artifact_id, _, coverage)) = overlay_record(db, source)? else {
        return Ok(());
    };
    if !entries.starts_with(&coverage.prefix) {
        return Ok(());
    }
    let prefix = copied
        .get(..coverage.prefix.len())
        .context("The copied transcript prefix is incomplete")?;
    let omitted = coverage
        .prefix
        .iter()
        .zip(prefix)
        .filter(|(origin, _)| coverage.omitted.contains(origin))
        .map(|(_, copy)| copy.clone())
        .collect();
    save_overlay(
        db,
        destination,
        &artifact_id,
        &Coverage {
            prefix: prefix.to_vec(),
            omitted,
        },
    )
}

pub(super) fn same_seed(db: &Connection, first: &str, second: &str) -> Result<bool> {
    let read = |context| {
        db.query_row(
            "SELECT artifact_id FROM conversation_context_compactions WHERE context_id = ?1",
            [context],
            |row| row.get::<_, String>(0),
        )
        .optional()
    };
    Ok(read(first)? == read(second)?)
}

pub(crate) fn projected_messages(
    db: &Connection,
    chat: &Conversation,
    entries: &[String],
) -> Result<Vec<MessageRevision>> {
    let history = load_messages(db, &chat.id, entries)?;
    Ok(match load_overlay(db, &chat.context_id)? {
        Some(overlay) if entries.starts_with(&overlay.coverage.prefix) => {
            apply_projection(history, &overlay)
        }
        _ => history,
    })
}

pub(super) fn apply_projection(
    mut history: Vec<MessageRevision>,
    overlay: &Overlay,
) -> Vec<MessageRevision> {
    history.retain(|message| !overlay.coverage.omitted.contains(&message.revision_id));
    history.insert(0, overlay.seed());
    history
}

#[cfg(test)]
mod tests;
