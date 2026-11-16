use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::conversations::reference_delivery::source_revisions;
use crate::dispatch::{ContextSnapshot, ConversationArtifactRevision, NativeHistory, lineage};

use super::{DeletionPlan, HistoryDeletionMode, HistoryDeletionTarget, ids, unseal};

pub(super) fn build(
    db: &Connection,
    target: HistoryDeletionTarget,
    mode: HistoryDeletionMode,
) -> Result<DeletionPlan> {
    let mut plan = DeletionPlan::new(target.clone(), mode);
    match target {
        HistoryDeletionTarget::Message {
            conversation_id,
            message_revision_id,
        } => {
            message(db, &mut plan, &conversation_id, &message_revision_id)?;
        }
        HistoryDeletionTarget::AttemptDiff {
            conversation_id,
            run_id,
        } => {
            diff(db, &mut plan, &conversation_id, &run_id)?;
        }
    }
    Ok(plan)
}

fn message(db: &Connection, plan: &mut DeletionPlan, owner: &str, revision: &str) -> Result<()> {
    let deleted = db
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM history_deleted_revisions WHERE revision_id = r.id)
         FROM message_revisions r JOIN messages m ON m.id = r.message_id
         WHERE r.id = ?1 AND m.conversation_id = ?2",
            params![revision, owner],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .context("The selected revision is outside this conversation")?;
    ensure!(
        !deleted || plan.mode == HistoryDeletionMode::WithDerivedCopies,
        "This history item was already deleted"
    );
    plan.target_already_deleted = deleted;
    let cascade = plan.mode == HistoryDeletionMode::WithDerivedCopies;
    plan.revisions.insert(revision.into());

    // Run the fixed point even for selected-only deletion. Its retained copies
    // are left intact, but a native session that consumed them can replay data.
    let mut derivatives = DeletionPlan::new(plan.target.clone(), plan.mode);
    derivatives.revisions.insert(revision.into());
    loop {
        let previous = (
            derivatives.revisions.len(),
            derivatives.synthetic_input_runs.len(),
        );
        expand_sources(db, &mut derivatives)?;
        expand_summary_runs(db, &mut derivatives, cascade)?;
        if previous
            == (
                derivatives.revisions.len(),
                derivatives.synthetic_input_runs.len(),
            )
        {
            break;
        }
    }
    if cascade {
        plan.revisions.clone_from(&derivatives.revisions);
        plan.artifacts.clone_from(&derivatives.artifacts);
        plan.synthetic_input_runs
            .clone_from(&derivatives.synthetic_input_runs);
        plan.answer_runs.clone_from(&derivatives.answer_runs);
        plan.jobs.clone_from(&derivatives.jobs);
    }
    plan.summary_requests
        .clone_from(&derivatives.summary_requests);
    plan.jobs.extend(ids(
        db, "SELECT job_id FROM conversation_summary_requests WHERE id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&plan.summary_requests)?],
    )?);

    let revision_set = serde_json::to_string(&derivatives.revisions)?;
    let removed = serde_json::to_string(&plan.revisions)?;
    plan.answer_runs.extend(answer_runs(db, &removed)?);
    plan.references = ids(
        db,
        "SELECT id FROM conversation_reference_previews WHERE artifact_id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&plan.artifacts)?],
    )?;
    plan.jobs.extend(ids(
        db,
        "SELECT job_id FROM queued_conversation_instructions WHERE revision_id IN (SELECT value FROM json_each(?1))",
        [&removed],
    )?);
    if cascade {
        plan.jobs.extend(ids(
            db,
            "SELECT DISTINCT job.job_id
             FROM job_attempts job JOIN agent_attempts a ON a.run_id = job.run_id
             JOIN conversation_contexts c ON c.id = a.context_id
             JOIN message_revisions revision ON revision.id = json_extract(c.entries, '$[#-1]')
             JOIN messages m ON m.id = revision.message_id AND m.role = 'user'
             WHERE revision.id IN (SELECT value FROM json_each(?1))",
            [&removed],
        )?);
    }

    let source_runs = serde_json::to_string(&derivatives.synthetic_input_runs)?;
    let answer_runs = serde_json::to_string(&derivatives.answer_runs)?;
    plan.retained_runs = ids(
        db,
        "SELECT a.run_id FROM agent_attempts a
         JOIN conversation_contexts input ON input.id = a.context_id
         LEFT JOIN conversation_contexts cursor ON cursor.id = a.resume_cursor_id
         LEFT JOIN conversation_contexts result ON result.id = a.result_context_id
         WHERE EXISTS (
             SELECT 1 FROM (
                SELECT value FROM json_each(input.entries)
                UNION ALL SELECT value FROM json_each(cursor.entries)
                UNION ALL SELECT value FROM json_each(result.entries)
             ) WHERE value IN (SELECT value FROM json_each(?1))
         ) OR a.run_id IN (SELECT value FROM json_each(?2))
           OR a.run_id IN (SELECT value FROM json_each(?3))
         UNION
         SELECT run_id FROM instruction_delivery_attempts
         WHERE revision_id IN (SELECT value FROM json_each(?1))",
        params![revision_set, source_runs, answer_runs],
    )?;
    inspect_dispatches(db, plan, &derivatives)?;

    // Revoke sessions of prepared runs too, even when no provider id was
    // retained. A delayed callback can populate it only on an eligible run.
    plan.sessions = ids(
        db,
        "SELECT DISTINCT agent_session_id FROM agent_attempts WHERE run_id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&plan.retained_runs)?],
    )?;
    if cascade {
        plan.side_requests = ids(
            db,
            "SELECT side.job_id FROM job_side_requests side JOIN conversation_contexts context ON context.id = side.context_id
             WHERE NOT side.validation AND length(side.payload) > 0
             AND NOT EXISTS (SELECT 1 FROM history_deleted_side_payloads WHERE job_id = side.job_id)
             AND EXISTS (
                 SELECT 1 FROM json_each(context.entries) entry
                 WHERE entry.value IN (SELECT value FROM json_each(?1))
             )",
            [&revision_set],
        )?;
    }
    settle_scope(db, plan)
}

fn expand_sources(db: &Connection, plan: &mut DeletionPlan) -> Result<()> {
    plan.revisions = ids(
        db,
        "WITH RECURSIVE copies(id) AS (
             SELECT value FROM json_each(?1)
             UNION SELECT edge.copied_revision_id FROM message_revision_copy_sources edge
                   JOIN copies ON copies.id = edge.source_revision_id
         ) SELECT id FROM copies",
        [serde_json::to_string(&plan.revisions)?],
    )?;
    let mut statement = db.prepare(
        "SELECT DISTINCT a.id, a.source_conversation_id
         FROM conversation_artifact_revisions a JOIN conversation_artifact_sources s ON s.artifact_revision_id = a.id
         WHERE s.message_revision_id IN (SELECT value FROM json_each(?1))
            OR EXISTS (SELECT 1 FROM agent_attempts attempt
               JOIN conversation_contexts output ON output.id = attempt.result_context_id
               WHERE attempt.run_id = a.summary_run_id
                 AND json_extract(output.entries, '$[#-1]') IN (SELECT value FROM json_each(?1)))",
    )?;
    let mut rows = statement.query([serde_json::to_string(&plan.revisions)?])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let owner: String = row.get(1)?;
        if let Some(artifact) = lineage::load_conversation_artifact(db, &owner, &id)? {
            plan.artifacts.insert(id);
            if let ConversationArtifactRevision::Summary { run, .. } = artifact {
                plan.revisions.insert(run.output_revision_id);
                plan.synthetic_input_runs.insert(run.run_id);
            }
        }
    }
    Ok(())
}

fn expand_summary_runs(db: &Connection, plan: &mut DeletionPlan, cascade: bool) -> Result<()> {
    let mut statement = db.prepare(
        "SELECT id, job_id, payload FROM conversation_summary_requests WHERE length(payload) > 0",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let sources = lineage::pending_summary_sources(&row.get::<_, Vec<u8>>(2)?)?;
        if sources.iter().any(|id| plan.revisions.contains(id)) {
            let request: String = row.get(0)?;
            let job: String = row.get(1)?;
            plan.summary_requests.insert(request);
            plan.jobs.insert(job.clone());
            plan.synthetic_input_runs.extend(ids(
                db,
                "SELECT run_id FROM job_attempts WHERE job_id = ?1",
                [&job],
            )?);
        }
    }
    let synthetic = serde_json::to_string(&plan.synthetic_input_runs)?;
    plan.jobs.extend(ids(
        db,
        "SELECT job_id FROM job_attempts WHERE run_id IN (SELECT value FROM json_each(?1))",
        [&synthetic],
    )?);
    // A pending summary cannot be sent using its frozen prompt after deletion.
    // Its request is cancelled in either mode; already settled summaries are
    // kept in selected-only mode including their execution evidence.
    if cascade {
        plan.revisions.extend(ids(
            db,
            "SELECT json_extract(context.entries, '$[#-1]')
             FROM agent_attempts a JOIN conversation_contexts context ON context.id IN (a.context_id, a.result_context_id)
             WHERE a.run_id IN (SELECT value FROM json_each(?1)) AND json_array_length(context.entries) > 0",
            [&synthetic],
        )?);
    }
    plan.answer_runs = answer_runs(db, &serde_json::to_string(&plan.revisions)?)?;
    Ok(())
}

fn answer_runs(db: &Connection, revisions: &str) -> Result<std::collections::BTreeSet<String>> {
    ids(
        db,
        "SELECT a.run_id FROM agent_attempts a
         JOIN conversation_contexts input ON input.id = a.context_id
         JOIN conversation_contexts output ON output.id = a.result_context_id
         JOIN message_revisions revision ON revision.id = json_extract(output.entries, '$[#-1]')
         JOIN messages m ON m.id = revision.message_id AND m.role = 'assistant'
         WHERE json_array_length(output.entries) > json_array_length(input.entries)
           AND revision.id IN (SELECT value FROM json_each(?1))",
        [revisions],
    )
}

fn inspect_dispatches(
    db: &Connection,
    plan: &mut DeletionPlan,
    derivatives: &DeletionPlan,
) -> Result<()> {
    let mut statement = db.prepare(
        "SELECT d.id, s.run_id, s.payload, EXISTS(
           SELECT 1 FROM dispatch_lineage_messages line WHERE line.dispatch_id = d.id
             AND line.revision_id IN (SELECT value FROM json_each(?1))
         ) OR EXISTS(
           SELECT 1 FROM instruction_delivery_attempts i WHERE i.run_id = s.run_id
             AND i.revision_id IN (SELECT value FROM json_each(?1)) AND i.prepared_event_seq IS NOT NULL
         )
         FROM dispatches d JOIN context_snapshots s ON s.id = d.snapshot_id
         WHERE d.ordinal = 1",
    )?;
    let revisions = &derivatives.revisions;
    let encoded_revisions = serde_json::to_string(revisions)?;
    let mut rows = statement.query([&encoded_revisions])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let run: String = row.get(1)?;
        let snapshot: ContextSnapshot = unseal(&row.get::<_, Vec<u8>>(2)?)?;
        let has_copy = row.get::<_, bool>(3)?
            || derivatives.synthetic_input_runs.contains(&run)
            || snapshot.submitted_messages.iter().any(|item| {
                revisions.contains(&item.revision_id)
                    || derivatives.artifacts.contains(&item.revision_id)
            })
            || snapshot
                .attachments
                .iter()
                .any(|item| revisions.contains(&item.message_revision_id))
            || snapshot.conversation_references.iter().any(|item| {
                source_revisions(&item.reference.artifact)
                    .iter()
                    .any(|id| revisions.contains(*id))
            });
        if has_copy {
            plan.retained_runs.insert(run.clone());
            if plan.mode == HistoryDeletionMode::WithDerivedCopies {
                plan.dispatches.insert(id);
            }
        }
        let cursor = match snapshot.native_history {
            NativeHistory::Fresh => continue,
            NativeHistory::ProviderCursor { context_id, .. }
            | NativeHistory::ProviderFork { context_id, .. }
            | NativeHistory::ProviderRecovery { context_id, .. } => context_id,
        };
        let native_depends = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM conversation_contexts c JOIN json_each(c.entries) e
             WHERE c.id = ?1 AND e.value IN (SELECT value FROM json_each(?2)))",
            params![cursor, encoded_revisions],
            |row| row.get::<_, bool>(0),
        )?;
        if native_depends {
            plan.retained_runs.insert(run);
        }
    }
    Ok(())
}

fn settle_scope(db: &Connection, plan: &mut DeletionPlan) -> Result<()> {
    let retained = serde_json::to_string(&plan.retained_runs)?;
    let sessions = serde_json::to_string(&plan.sessions)?;
    let jobs = serde_json::to_string(&plan.jobs)?;
    let side_requests = serde_json::to_string(&plan.side_requests)?;
    plan.unsettled.extend(ids(
        db,
        "SELECT attempts.run_id FROM agent_attempts attempts
         WHERE (
            attempts.run_id IN (SELECT value FROM json_each(?1))
            OR attempts.agent_session_id IN (SELECT value FROM json_each(?2))
            OR EXISTS (SELECT 1 FROM job_attempts j WHERE j.run_id = attempts.run_id
                       AND j.job_id IN (SELECT value FROM json_each(?3)))
         ) AND (EXISTS(SELECT 1 FROM active_agent_attempts WHERE run_id = attempts.run_id)
                OR EXISTS(SELECT 1 FROM unconfirmed_agent_stops WHERE run_id = attempts.run_id))",
        params![retained, sessions, jobs],
    )?);
    plan.unsettled.extend(ids(
        db,
        "SELECT a.run_id FROM job_attempts a JOIN runs r ON r.id = a.run_id
         JOIN job_side_requests side ON side.job_id = a.job_id
         JOIN agent_attempts source ON source.run_id = side.source_run_id
         WHERE r.status IN ('starting', 'running', 'waiting_for_input')
           AND (source.agent_session_id IN (SELECT value FROM json_each(?1))
                OR a.job_id IN (SELECT value FROM json_each(?2)))",
        params![sessions, side_requests],
    )?);
    Ok(())
}

fn diff(db: &Connection, plan: &mut DeletionPlan, owner: &str, run: &str) -> Result<()> {
    let (evidence, deleted): (Vec<u8>, bool) = db.query_row(
        "SELECT c.evidence, EXISTS (SELECT 1 FROM history_deleted_attempt_diffs WHERE run_id = c.run_id)
         FROM job_worktree_candidates c JOIN agent_attempts a ON a.run_id = c.run_id
         WHERE c.run_id = ?1 AND a.conversation_id = ?2",
        params![run, owner], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.context("The recorded diff is outside this conversation")?;
    match unseal::<crate::repository::AttemptWorktreeEvidence>(&evidence)? {
        crate::repository::AttemptWorktreeEvidence::Saved { artifact } => {
            plan.diff_revision = Some(artifact.revision);
        }
        _ => ensure!(
            deleted && plan.mode == HistoryDeletionMode::WithDerivedCopies,
            "The recorded diff is unavailable"
        ),
    }
    plan.target_already_deleted = deleted;
    plan.diffs.insert(run.into());
    plan.retained_runs.insert(run.into());
    if plan.mode == HistoryDeletionMode::WithDerivedCopies {
        plan.candidates = ids(
            db,
            "SELECT id FROM selected_candidate_revisions WHERE capture_run_id = ?1",
            [run],
        )?;
    }
    settle_scope(db, plan)
}
