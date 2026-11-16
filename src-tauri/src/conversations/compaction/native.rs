use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    diagnostics,
    dispatch::ContextSnapshot,
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, SessionMode, TurnOutcome,
        capabilities::CapabilityScope, profiles::DESKTOP_PERMISSION_REVISION,
    },
    runner::DirectWindowsRunner,
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{Conversation, SessionScope, busy, check_idle_base, recent_request, reconcile};

// Authenticated Phase 02 App Server fixture, including request acknowledgment,
// the compaction item, and completion for the same thread and turn.
const VERIFIED_CODEX_BUILD: &str = "0.169.0-alpha.11.1";
const VERIFIED_MODEL: &str = "gpt-6-luna";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NativeStatus {
    eligible: bool,
    reason: Option<&'static str>,
    request: Option<NativeRequest>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeRequest {
    run_id: String,
    source_context_id: String,
    status: RunStatus,
    detail: Option<String>,
    boundary: Option<NativeBoundary>,
    usage: Option<NativeUsage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeBoundary {
    turn_id: String,
    item_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeUsage {
    input: Option<u64>,
    output: Option<u64>,
    cached_input: Option<u64>,
    reasoning_output: Option<u64>,
}

pub(crate) struct NativeCompactionPlan {
    pub run: RunRecord,
    pub scope: SessionScope,
    pub provider_session_id: String,
    pub provider_turn_id: String,
    pub evidence: CapabilityScope,
}

struct NativeSession {
    source_run: String,
    session_id: String,
    provider_session_id: String,
    provider_turn_id: String,
    evidence: CapabilityScope,
}

fn qualified(scope: &SessionScope, evidence: &CapabilityScope) -> bool {
    scope.provider == Provider::Codex
        && scope.runner == DirectWindowsRunner::IDENTITY
        && scope.cli_version == VERIFIED_CODEX_BUILD
        && scope.model.as_deref() == Some(VERIFIED_MODEL)
        && scope.mode == SessionMode::ReadOnly
        && scope.permission_revision == DESKTOP_PERMISSION_REVISION
        && scope
            .account_binding
            .as_ref()
            .is_some_and(|binding| !binding.is_empty())
        && evidence.executable_revision.is_some()
        && evidence
            .platform
            .as_ref()
            .is_some_and(|platform| platform.guest.is_none())
        && evidence
            == &CapabilityScope::from_dispatch(
                scope,
                evidence.checkout.id.clone(),
                evidence.executable_revision.clone(),
                evidence.platform.clone(),
                evidence.configuration.clone(),
                Some(VERIFIED_MODEL.into()),
                evidence.reported_effort.clone(),
            )
        && scope
            .effort
            .as_ref()
            .is_none_or(|effort| evidence.reported_effort.as_ref() == Some(effort))
}

fn candidate(
    db: &Connection,
    chat: &Conversation,
    matches: impl Fn(&SessionScope) -> bool,
) -> Result<Option<NativeSession>> {
    let Some(prior) = super::super::previous_attempt(db, &chat.id)? else {
        return Ok(None);
    };
    let Some(provider_session_id) = prior.provider_session_id else {
        return Ok(None);
    };
    if prior.cursor.as_deref() != Some(&chat.context_id) || prior.native_scope_reserved {
        return Ok(None);
    }
    let scope: SessionScope = serde_json::from_str(&prior.scope)?;
    if !matches(&scope) {
        return Ok(None);
    }
    let source: Option<(Vec<u8>, String)> = db
        .query_row(
            "SELECT s.payload, coalesce(
                (SELECT c.provider_turn_id FROM conversation_native_compactions op
                 JOIN agent_attempts c ON c.run_id = op.run_id
                 JOIN runs run ON run.id = c.run_id
                 WHERE op.submitted AND run.status = 'succeeded' AND c.agent_session_id = a.agent_session_id
                   AND c.context_id = a.result_context_id AND c.provider_turn_id IS NOT NULL
                 ORDER BY c.ordinal DESC LIMIT 1), a.provider_turn_id)
         FROM context_snapshots s
         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
         JOIN agent_attempts a ON a.run_id = s.run_id
         JOIN runs r ON r.id = s.run_id
         WHERE s.run_id = ?1 AND d.delivery = 'submitted' AND r.status = 'succeeded'
           AND a.result_context_id = ?2 AND a.provider_terminal = 'completed' AND a.provider_turn_id IS NOT NULL",
            params![prior.run_id, chat.context_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((snapshot, provider_turn_id)) = source else {
        return Ok(None);
    };
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
    let Some(assessment) = snapshot.capabilities else {
        return Ok(None);
    };
    if snapshot.scope != scope
        || snapshot.session_id.as_deref() != Some(&prior.session_id)
        || assessment.scope.checkout.id != chat.checkout_id
        || !qualified(&scope, &assessment.scope)
        || snapshot
            .scoped_launch
            .is_none_or(|launch| launch.candidate_checkout.is_some())
    {
        return Ok(None);
    }
    Ok(Some(NativeSession {
        source_run: prior.run_id,
        session_id: prior.session_id,
        provider_session_id,
        provider_turn_id,
        evidence: assessment.scope,
    }))
}

fn blocker(db: &Connection, chat: &Conversation) -> Result<Option<&'static str>> {
    if busy(db, &chat.id)?
        || recent_request(db, &chat.id)?.is_some_and(|request| request.status == "queued")
    {
        return Ok(Some(
            "Wait until the conversation and its queued work are idle.",
        ));
    }
    Ok(None)
}

pub(super) fn status(
    db: &Connection,
    chat: &Conversation,
    matches: impl Fn(&SessionScope) -> bool,
) -> Result<NativeStatus> {
    let reason = match blocker(db, chat)? {
        Some(reason) => Some(reason),
        None if candidate(db, chat, matches)?.is_some() => None,
        None => Some("A matching, qualified idle Codex session is required."),
    };
    let latest: Option<(String, String, String, Option<String>)> = db
        .query_row(
            "SELECT c.run_id, a.context_id, r.status, c.detail
         FROM conversation_native_compactions c
         JOIN agent_attempts a ON a.run_id = c.run_id
         JOIN runs r ON r.id = c.run_id
         WHERE a.conversation_id = ?1
         ORDER BY a.ordinal DESC LIMIT 1",
            [&chat.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let request = latest
        .map(|(run_id, source_context_id, status, detail)| {
            let (boundary, usage, _) = observations(db, &run_id)?;
            Ok::<_, anyhow::Error>(NativeRequest {
                run_id,
                source_context_id,
                status: RunStatus::from_sql(&status)?,
                detail,
                boundary,
                usage,
            })
        })
        .transpose()?;
    Ok(NativeStatus {
        eligible: reason.is_none(),
        reason,
        request,
    })
}

fn observations(
    db: &Connection,
    run: &str,
) -> Result<(Option<NativeBoundary>, Option<NativeUsage>, bool)> {
    if crate::dispatch::revoked_history_run(db, run)? {
        return Ok((None, None, false));
    }
    let mut query = db.prepare_cached(
        "SELECT payload FROM agent_events WHERE run_id = ?1 AND source = 'provider' ORDER BY seq",
    )?;
    let mut rows = query.query([run])?;
    let mut boundary = None;
    let mut last_usage = None;
    let mut reasoning_usage = None;
    let mut completion = None;
    while let Some(row) = rows.next()? {
        let encrypted: Vec<u8> = row.get(0)?;
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
        let Some(turn_id) = event.location.turn_id else {
            continue;
        };
        match event.data {
            ProviderEventData::Compacted => {
                boundary = Some(NativeBoundary {
                    turn_id,
                    item_id: event.location.item_id,
                });
            }
            ProviderEventData::Usage {
                input,
                output,
                cached_input,
            } => {
                last_usage = Some((
                    turn_id,
                    NativeUsage {
                        input,
                        output,
                        cached_input,
                        reasoning_output: None,
                    },
                ));
            }
            ProviderEventData::ReasoningUsage { tokens } => {
                reasoning_usage = Some((turn_id, tokens))
            }
            ProviderEventData::NativeCompactionFinished {
                outcome: TurnOutcome::Completed,
            } => completion = Some(turn_id),
            _ => {}
        }
    }
    let mut usage = last_usage.and_then(|(turn, usage)| {
        boundary
            .as_ref()
            .is_some_and(|boundary| boundary.turn_id == turn)
            .then_some(usage)
    });
    if let Some((turn, tokens)) = reasoning_usage
        && boundary
            .as_ref()
            .is_some_and(|boundary| boundary.turn_id == turn)
    {
        usage
            .get_or_insert(NativeUsage {
                input: None,
                output: None,
                cached_input: None,
                reasoning_output: None,
            })
            .reasoning_output = Some(tokens);
    }
    let succeeded = boundary
        .as_ref()
        .is_some_and(|boundary| completion.as_ref() == Some(&boundary.turn_id));
    Ok((boundary, usage, succeeded))
}

impl Store {
    pub(crate) async fn reserve_native_compaction(
        &self,
        conversation: String,
        expected_context: String,
        scope: SessionScope,
    ) -> Result<NativeCompactionPlan> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            reconcile(&tx, &conversation)?;
            let chat = check_idle_base(&tx, &conversation, &expected_context)?;
            if let Some(reason) = blocker(&tx, &chat)? {
                anyhow::bail!("{reason}");
            }
            let source = candidate(&tx, &chat, |saved| saved == &scope)?
                .context("A matching, qualified idle Codex session is required")?;
            let (run, _) = runs::insert_run(&tx, chat.checkout_id, "agent")?;
            tx.execute(
                "INSERT INTO agent_attempts(run_id, turn_id, conversation_id, ordinal, agent_session_id,
                 previous_run_id, context_id, resume_cursor_id, detached_context, subagent_conversation_id)
                 SELECT ?1, ?2, ?3, coalesce(max(ordinal), 0) + 1, ?4, ?5, ?6, ?6, 1, ?3
                 FROM agent_attempts WHERE conversation_id = ?3",
                params![run.id, run.turn_id, chat.id, source.session_id, source.source_run, chat.context_id],
            )?;
            tx.execute("INSERT INTO active_agent_attempts VALUES (?1, ?2)", params![chat.id, run.id])?;
            tx.execute("INSERT INTO conversation_native_compactions(run_id) VALUES (?1)", [&run.id])?;
            tx.commit()?;
            Ok(NativeCompactionPlan {
                run, scope, provider_session_id: source.provider_session_id,
                provider_turn_id: source.provider_turn_id, evidence: source.evidence
            })
        }).await
    }

    /// A lost reply can mean the provider changed its history. Keep the old
    /// mapping unavailable from the moment we may write the request.
    pub(crate) async fn start_native_compaction_submission(&self, run: String) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "UPDATE agent_sessions SET cursor_context_id = NULL
                 WHERE EXISTS (SELECT 1 FROM agent_attempts a
                    JOIN active_agent_attempts active ON active.run_id = a.run_id
                    JOIN conversations chat ON chat.id = a.conversation_id
                    WHERE a.run_id = ?1 AND a.agent_session_id = agent_sessions.id
                      AND a.context_id = chat.active_context_id AND a.context_id = agent_sessions.cursor_context_id)
                   AND NOT EXISTS (SELECT 1 FROM history_revoked_sessions WHERE session_id = agent_sessions.id)",
                [&run],
            )?;
            ensure!(changed == 1, "The provider session or conversation context changed");
            let changed = tx.execute(
                "UPDATE conversation_native_compactions SET submitted = 1 WHERE run_id = ?1 AND NOT submitted",
                [&run],
            )?;
            ensure!(changed == 1, "Native compaction was already submitted");
            tx.commit()?;
            Ok(())
        }).await
    }

    pub(crate) async fn finish_native_compaction(
        &self,
        run: String,
        outcome: RunStatus,
        detail: Option<String>,
        stopped: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            settle(&tx, &run, outcome, detail.as_deref(), stopped)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }
}

fn settle(
    db: &Connection,
    run: &str,
    mut outcome: RunStatus,
    mut detail: Option<&str>,
    stopped: bool,
) -> Result<()> {
    let active = super::super::active_attempt(db, run)?;
    let record = runs::load_run(db, run)?;
    if outcome == RunStatus::Succeeded && stopped {
        if !observations(db, run)?.2 {
            outcome = RunStatus::Failed;
            detail = Some("The provider did not confirm the exact compaction boundary.");
        } else {
            let changed = db.execute(
                "UPDATE agent_sessions SET cursor_context_id = ?1
                 WHERE id = ?2 AND cursor_context_id IS NULL
                   AND EXISTS (SELECT 1 FROM conversations WHERE id = ?3 AND active_context_id = ?1)
                   AND EXISTS (SELECT 1 FROM conversation_native_compactions WHERE run_id = ?4 AND submitted)
                   AND NOT EXISTS (SELECT 1 FROM history_revoked_sessions WHERE session_id = ?2)",
                params![active.context_id, active.session_id, active.conversation_id, run],
            )?;
            if changed != 1 {
                outcome = RunStatus::Failed;
                detail = Some("The exact compacted provider cursor could not be restored.");
            }
        }
    }
    if !record.status.is_terminal() {
        runs::advance_run(db, &record, outcome)?;
    }
    db.execute(
        "UPDATE conversation_native_compactions SET detail = ?2 WHERE run_id = ?1",
        params![run, detail.map(diagnostics::preview)],
    )?;
    db.execute(
        "UPDATE agent_attempts SET result_context_id = context_id WHERE run_id = ?1",
        [run],
    )?;
    if stopped {
        db.execute("DELETE FROM active_agent_attempts WHERE run_id = ?1", [run])?;
    }
    Ok(())
}

pub(in crate::conversations) fn recover(db: &Connection) -> Result<()> {
    let attempts = {
        let mut query = db.prepare(
            "SELECT c.run_id FROM conversation_native_compactions c JOIN active_agent_attempts a ON a.run_id = c.run_id",
        )?;
        query
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for run in attempts {
        settle(
            db,
            &run,
            RunStatus::Interrupted,
            Some("The app restarted during this provider operation."),
            true,
        )?;
    }
    Ok(())
}

pub(crate) fn native_session_compacted(db: &Connection, session: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM conversation_native_compactions c JOIN agent_attempts a ON a.run_id = c.run_id
            WHERE a.agent_session_id = ?1 AND c.submitted
        )",
        [session],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests;
