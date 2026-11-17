use anyhow::Result;
use rusqlite::{Connection, params};

use crate::{
    dispatch::{ContextSnapshot, lineage},
    jobs::JobRequest,
    protection,
    providers::{ProviderEvent, ProviderEventData, suggestions::SuggestionOutcome},
    repository::AttemptWorktreeEvidence,
};

use super::{DeletionPlan, HistoryDeletionMode, ids, seal, unseal};

pub(super) fn apply(db: &Connection, plan: &DeletionPlan) -> Result<()> {
    // Seal the original lineage of legacy snapshots before any mutable copies
    // are redacted. The lineage payload retains identities and byte ranges.
    for dispatch in &plan.dispatches {
        redact_dispatch(db, plan, dispatch)?;
    }
    let removed = serde_json::to_string(&plan.revisions)?;
    let empty_text = protection::protect(b"")?;
    db.execute(
        "INSERT OR IGNORE INTO history_deleted_revisions SELECT value FROM json_each(?1)",
        [&removed],
    )?;
    db.execute(
        "UPDATE message_revisions SET content = ?2 WHERE id IN (SELECT value FROM json_each(?1))",
        params![removed, empty_text],
    )?;
    db.execute(
        "DELETE FROM message_attachments WHERE message_revision_id IN (SELECT value FROM json_each(?1))", [&removed],
    )?;
    db.execute(
        "UPDATE queued_conversation_instructions SET attachment_labels = NULL
         WHERE revision_id IN (SELECT value FROM json_each(?1))",
        [&removed],
    )?;
    redact_steering(db, plan, &removed)?;
    redact_events(db, plan, &removed)?;
    revoke_sessions(db, plan)?;
    for job in &plan.side_requests {
        crate::jobs::side_requests::redact_retained_side_context(db, job)?;
    }

    let artifacts = serde_json::to_string(&plan.artifacts)?;
    let references = serde_json::to_string(&plan.references)?;
    db.execute(
        "INSERT OR IGNORE INTO history_deleted_artifacts SELECT value FROM json_each(?1)",
        [&artifacts],
    )?;
    db.execute(
        "UPDATE conversation_artifact_revisions SET payload = X'' WHERE id IN (SELECT value FROM json_each(?1))",
        [&artifacts],
    )?;
    db.execute(
        "UPDATE conversation_reference_previews SET payload = X'' WHERE id IN (SELECT value FROM json_each(?1))",
        [&references],
    )?;
    db.execute(
        "DELETE FROM conversation_reference_drafts WHERE preview_id IN (SELECT value FROM json_each(?1))",
        [&references],
    )?;

    for job in &plan.jobs {
        let request: Vec<u8> =
            db.query_row("SELECT request FROM jobs WHERE id = ?1", [job], |row| {
                row.get(0)
            })?;
        let mut request: JobRequest = unseal(&request)?;
        request.instructions.clear();
        db.execute(
            "UPDATE jobs SET request = ?2, queue_state = CASE
                WHEN NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = ?1)
                     OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = ?1)
                THEN 'cancelled' ELSE queue_state END
             WHERE id = ?1",
            params![job, seal(&request)?],
        )?;
        db.execute("DELETE FROM job_recoveries WHERE job_id = ?1", [job])?;
        db.execute(
            "UPDATE model_comparison_syntheses SET prompt = X'' WHERE job_id = ?1",
            [job],
        )?;
    }
    db.execute(
        "UPDATE conversation_summary_requests SET payload = X'' WHERE id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&plan.summary_requests)?],
    )?;

    for run in &plan.diffs {
        db.execute(
            "INSERT OR IGNORE INTO history_deleted_attempt_diffs VALUES (?1)",
            [run],
        )?;
        db.execute(
            "UPDATE job_worktree_candidates SET evidence = ?2 WHERE run_id = ?1",
            params![
                run,
                seal(&AttemptWorktreeEvidence::Unavailable {
                    reason: "The recorded diff was removed from history.".into(),
                })?
            ],
        )?;
    }
    for candidate in &plan.candidates {
        db.execute(
            "INSERT OR IGNORE INTO history_deleted_candidate_diffs VALUES (?1)",
            [candidate],
        )?;
        db.execute(
            "UPDATE selected_candidate_revisions SET payload = X'' WHERE id = ?1",
            [candidate],
        )?;
        db.execute(
            "DELETE FROM active_unit_candidates WHERE candidate_id = ?1",
            [candidate],
        )?;
    }
    Ok(())
}

fn redact_dispatch(db: &Connection, plan: &DeletionPlan, dispatch: &str) -> Result<()> {
    let (snapshot_id, run, payload): (String, String, Vec<u8>) = db.query_row(
        "SELECT s.id, s.run_id, s.payload FROM dispatches d JOIN context_snapshots s ON s.id = d.snapshot_id WHERE d.id = ?1",
        [dispatch], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let mut snapshot: ContextSnapshot = unseal(&payload)?;
    lineage::preserve_legacy_attachment_lineage(db, dispatch, &run, &snapshot)?;
    for message in &mut snapshot.submitted_messages {
        if plan.revisions.contains(&message.revision_id)
            || plan.artifacts.contains(&message.revision_id)
            || plan.synthetic_input_runs.contains(&run)
        {
            message.text.clear();
        }
    }
    snapshot
        .conversation_references
        .retain(|entry| !plan.artifacts.contains(entry.reference.artifact.id()));
    let retired = super::super::attachment_retention::dispatch_removed_ordinals(db, dispatch)?;
    let mut attachments = Vec::new();
    for (ordinal, attachment) in
        super::super::attachment_retention::present_ordinals(&retired).zip(snapshot.attachments)
    {
        if plan.revisions.contains(&attachment.message_revision_id) {
            db.execute(
                "INSERT INTO removed_dispatch_attachments(dispatch_id, attachment_ordinal, message_revision_id)
                 VALUES (?1, ?2, ?3)",
                params![dispatch, u32::try_from(ordinal)?, attachment.message_revision_id],
            )?;
        } else {
            attachments.push(attachment);
        }
    }
    snapshot.attachments = attachments;
    db.execute(
        "UPDATE context_snapshots SET payload = ?2 WHERE id = ?1",
        params![snapshot_id, seal(&snapshot)?],
    )?;
    // Frames are verbatim wire evidence. Invalidate the entire frame set
    // rather than presenting a hand-edited approximation as captured bytes.
    db.execute(
        "UPDATE dispatches SET frames = ?2 WHERE snapshot_id = ?1",
        params![snapshot_id, seal(&Vec::<Vec<u8>>::new())?],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO history_deleted_dispatch_payloads
         SELECT id FROM dispatches WHERE snapshot_id = ?1",
        [snapshot_id],
    )?;
    Ok(())
}

fn redact_steering(db: &Connection, plan: &DeletionPlan, revisions: &str) -> Result<()> {
    if plan.mode != HistoryDeletionMode::WithDerivedCopies {
        return Ok(());
    }
    let mut statement = db.prepare(
        "SELECT DISTINCT e.seq, e.payload FROM agent_events e
         JOIN instruction_delivery_attempts i ON i.prepared_event_seq = e.seq
         WHERE i.revision_id IN (SELECT value FROM json_each(?1))",
    )?;
    let mut rows = statement.query([revisions])?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next()? {
        let mut event: ProviderEvent = unseal(&row.get::<_, Vec<u8>>(1)?)?;
        if let ProviderEventData::SteeringPrepared { frame, .. } = &mut event.data {
            frame.clear();
            updates.push((row.get::<_, i64>(0)?, seal(&event)?));
        }
    }
    for (seq, payload) in updates {
        db.execute(
            "UPDATE agent_events SET payload = ?2 WHERE seq = ?1",
            params![seq, payload],
        )?;
    }
    Ok(())
}

fn redact_events(db: &Connection, plan: &DeletionPlan, revisions: &str) -> Result<()> {
    let mut runs = plan.answer_runs.clone();
    let mut message_ids = std::collections::BTreeSet::new();
    if plan.mode == HistoryDeletionMode::WithDerivedCopies {
        runs.extend(plan.retained_runs.iter().cloned());
        message_ids = ids(
            db,
            "SELECT DISTINCT message_id FROM message_revisions WHERE id IN (SELECT value FROM json_each(?1))",
            [revisions],
        )?;
    }
    let mut statement = db.prepare(
        "SELECT seq, run_id, payload FROM agent_events WHERE run_id IN (SELECT value FROM json_each(?1))",
    )?;
    let mut rows = statement.query([serde_json::to_string(&runs)?])?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next()? {
        let run: String = row.get(1)?;
        let mut event: ProviderEvent = unseal(&row.get::<_, Vec<u8>>(2)?)?;
        match &mut event.data {
            ProviderEventData::AssistantDelta { text }
            | ProviderEventData::AssistantMessage { text }
            | ProviderEventData::AssistantFinal { text }
                if plan.answer_runs.contains(&run) || plan.synthetic_input_runs.contains(&run) =>
            {
                text.clear()
            }
            ProviderEventData::Reasoning { text, .. }
                if plan.synthetic_input_runs.contains(&run) =>
            {
                text.clear()
            }
            ProviderEventData::PromptSuggestion { suggestion }
                if message_ids.contains(&suggestion.source.message_id)
                    && matches!(suggestion.outcome, SuggestionOutcome::Available { .. }) =>
            {
                suggestion.outcome = SuggestionOutcome::Unavailable;
            }
            _ => continue,
        }
        updates.push((row.get::<_, i64>(0)?, seal(&event)?));
    }
    for (seq, payload) in updates {
        db.execute(
            "UPDATE agent_events SET payload = ?2 WHERE seq = ?1",
            params![seq, payload],
        )?;
    }
    Ok(())
}

fn revoke_sessions(db: &Connection, plan: &DeletionPlan) -> Result<()> {
    let sessions = serde_json::to_string(&plan.sessions)?;
    db.execute(
        "INSERT OR IGNORE INTO history_revoked_sessions SELECT value FROM json_each(?1)",
        [&sessions],
    )?;
    db.execute(
        "UPDATE agent_sessions SET cursor_context_id = NULL WHERE id IN (SELECT value FROM json_each(?1))",
        [&sessions],
    )?;
    db.execute(
        "UPDATE jobs SET queue_state = 'cancelled'
         WHERE queue_state IN ('queued','paused')
           AND (NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = jobs.id)
                OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = jobs.id))
           AND id IN (
             SELECT side.job_id FROM job_side_requests side JOIN agent_attempts source ON source.run_id = side.source_run_id
             WHERE source.agent_session_id IN (SELECT value FROM json_each(?1))
           )",
        [&sessions],
    )?;
    Ok(())
}
