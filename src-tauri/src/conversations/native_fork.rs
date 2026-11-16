use super::{
    Conversation, MessageRevision, SessionScope, compaction, context_entries, load_conversation,
    load_messages, previous_attempt,
};
use crate::{
    dispatch::{ContextSnapshot, NativeHistory, omit_deleted_dispatch_messages},
    jobs::{SubagentAllocation, conversation_goal_transition, native_goal_fork_is_safe},
    protection,
    providers::{ForkBoundary, Provider, SessionIntent},
    runner::ExecutionPlatform,
    store::Store,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeForkSeed {
    pub source_run_id: String,
    pub source_session_id: String,
    pub source_turn_id: String,
    pub source_context_id: String,
    pub fork_context_id: String,
    pub executable_revision: String,
    pub platform: Option<ExecutionPlatform>,
}

impl NativeForkSeed {
    pub(super) fn intent(&self) -> SessionIntent {
        SessionIntent::Fork {
            provider_session_id: self.source_session_id.clone(),
            boundary: ForkBoundary::CompletedTurn {
                turn_id: self.source_turn_id.clone(),
            },
        }
    }

    pub(crate) fn history(&self) -> NativeHistory {
        NativeHistory::ProviderFork {
            session_id: self.source_session_id.clone(),
            source_run_id: self.source_run_id.clone(),
            source_turn_id: self.source_turn_id.clone(),
            context_id: self.source_context_id.clone(),
        }
    }
}

pub(super) fn source_session(db: &Connection, conversation: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT source_session_id FROM conversation_forks WHERE conversation_id = ?1",
            [conversation],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

pub(super) fn require_independent_session(
    db: &Connection,
    local_session: &str,
    provider_session: &str,
) -> Result<()> {
    let belongs_to_another_session: bool = db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM agent_sessions current JOIN agent_sessions other ON other.id <> current.id
           WHERE current.id = ?1 AND other.provider_session_id = ?2
             AND json_extract(other.scope, '$.provider') = json_extract(current.scope, '$.provider')
             AND json_extract(other.scope, '$.accountBinding') IS json_extract(current.scope, '$.accountBinding')
         )",
        params![local_session, provider_session],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        !belongs_to_another_session,
        "Codex returned an existing provider session for a fork"
    );
    Ok(())
}

pub(super) fn qualify(
    db: &Connection,
    fork: &Conversation,
    scope: &SessionScope,
    allocation: &SubagentAllocation,
    entries: &[String],
) -> Result<Option<NativeForkSeed>> {
    if allocation.native_allowance != Some(0) {
        return Ok(None);
    }
    qualify_seed(db, fork, &fork.context_id, scope, entries)
}

pub(super) fn continuation_source_session(
    db: &Connection,
    fork: &Conversation,
    scope: &SessionScope,
    entries: &[String],
) -> Result<Option<String>> {
    if qualify_seed(db, fork, &fork.context_id, scope, entries)?.is_some()
        && crate::jobs::direct_subagent_allocation(db, &fork.id, None, scope)?.native_allowance
            == Some(0)
    {
        return source_session(db, &fork.id);
    }
    Ok(None)
}

fn qualify_seed(
    db: &Connection,
    fork: &Conversation,
    fork_context: &str,
    scope: &SessionScope,
    entries: &[String],
) -> Result<Option<NativeForkSeed>> {
    if entries.is_empty()
        || scope.provider != Provider::Codex
        || scope.account_binding.as_ref().is_none_or(String::is_empty)
        || compaction::load_overlay(db, fork_context)?.is_some()
    {
        return Ok(None);
    }
    let origin: Option<(String, String, String)> = db
        .query_row(
            "SELECT f.source_conversation_id, f.source_revision_id, f.source_session_id
             FROM conversation_forks f JOIN conversations s ON s.id = f.source_conversation_id
             WHERE f.conversation_id = ?1 AND s.checkout_id = ?2 AND f.source_session_id IS NOT NULL
               AND f.source_revision_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM active_agent_attempts a WHERE a.conversation_id = s.id)
               AND NOT EXISTS (SELECT 1 FROM unconfirmed_agent_stops a WHERE a.conversation_id = s.id)",
            params![fork.id, fork.checkout_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((source, revision, session)) = origin else {
        return Ok(None);
    };
    let Some(prior) = previous_attempt(db, &source)? else {
        return Ok(None);
    };
    let (Some(cursor), Some(provider_session)) = (prior.cursor, prior.provider_session_id) else {
        return Ok(None);
    };
    if prior.session_id != session
        || prior.scope != serde_json::to_string(scope)?
        || prior.native_subagent_limit != Some(0)
        || prior.native_scope_reserved
        || compaction::load_overlay(db, &cursor)?.is_some()
        || compaction::native_session_compacted(db, &session)?
        || !native_goal_fork_is_safe(db, &session)?
        || conversation_goal_transition(db, &source)?.is_some()
        || conversation_goal_transition(db, &fork.id)?.is_some()
    {
        return Ok(None);
    }

    // The copied seed must still be an exact prefix of the verified session.
    let retained = context_entries(db, &source, &cursor)?;
    let Some(source_entries) = retained.get(..entries.len()) else {
        return Ok(None);
    };
    if source_entries.last() != Some(&revision) {
        return Ok(None);
    }
    let mut copy_matches = db.prepare(
        "SELECT EXISTS (
            SELECT 1 FROM message_revision_copy_sources c
            WHERE c.copied_revision_id = ?1 AND c.source_revision_id = ?2
            AND NOT EXISTS (SELECT 1 FROM history_deleted_revisions h WHERE h.revision_id IN (?1, ?2))
            AND NOT EXISTS (SELECT 1 FROM message_attachments a WHERE a.message_revision_id IN (?1, ?2))
            AND NOT EXISTS (SELECT 1 FROM message_conversation_references r WHERE r.message_revision_id IN (?1, ?2))
        )",
    )?;
    for (copy, original) in entries.iter().zip(source_entries) {
        if !copy_matches.query_row(params![copy, original], |row| row.get::<_, bool>(0))? {
            return Ok(None);
        }
    }

    let boundary: Option<(String, String, String, Vec<u8>)> = db
        .query_row(
            "SELECT a.run_id, a.provider_turn_id, c.id, s.payload
             FROM agent_attempts a JOIN runs r ON r.id = a.run_id
             JOIN conversation_contexts c ON c.id = a.result_context_id
             JOIN context_snapshots s ON s.run_id = a.run_id
             JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
             WHERE a.conversation_id = ?1 AND c.entries = ?2 AND a.agent_session_id = ?3
               AND a.detached_context = 0
               AND a.provider_turn_id IS NOT NULL AND a.provider_terminal = 'completed'
               AND r.status = 'succeeded' AND d.delivery = 'submitted'
               AND NOT EXISTS (SELECT 1 FROM history_deleted_dispatch_payloads h WHERE h.dispatch_id = d.id)
               AND NOT EXISTS (SELECT 1 FROM native_subagent_observations child WHERE child.session_id = ?3)
             ORDER BY a.ordinal DESC LIMIT 1",
            params![source, serde_json::to_string(source_entries)?, session],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((source_run_id, turn, source_context_id, payload)) = boundary else {
        return Ok(None);
    };
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
    let Some(assessment) = snapshot.capabilities else {
        return Ok(None);
    };
    let Some(executable_revision) = assessment.scope.executable_revision else {
        return Ok(None);
    };
    if snapshot.session_id.as_deref() != Some(session.as_str())
        || assessment.scope.checkout.id != fork.checkout_id
        || snapshot.scope != *scope
        || snapshot.task_context.is_some()
        || snapshot
            .scoped_launch
            .as_ref()
            .is_some_and(|launch| launch.candidate_checkout.is_some())
    {
        return Ok(None);
    }
    Ok(Some(NativeForkSeed {
        source_run_id,
        source_session_id: provider_session,
        source_turn_id: turn,
        source_context_id,
        fork_context_id: fork_context.into(),
        executable_revision,
        platform: assessment.scope.platform,
    }))
}

impl Store {
    pub(crate) async fn fresh_fork_input(
        &self,
        conversation: String,
        attempt_context: String,
    ) -> Result<Vec<MessageRevision>> {
        self.execute(move |db| replay_fork(db, &conversation, &attempt_context))
            .await
    }

    /// Recheck the source before opening a provider session. A changed source
    /// does not invalidate the fork's own independent, protected history.
    pub(crate) async fn recheck_native_fork(
        &self,
        conversation: String,
        attempt_context: String,
        scope: SessionScope,
        seed: NativeForkSeed,
        executable_revision: Option<String>,
        platform: Option<ExecutionPlatform>,
    ) -> Result<Option<Vec<MessageRevision>>> {
        self.execute(move |db| {
            let fork = load_conversation(db, &conversation)?;
            let entries = context_entries(db, &conversation, &seed.fork_context_id)?;
            if executable_revision.as_ref() == Some(&seed.executable_revision)
                && platform == seed.platform
                && qualify_seed(db, &fork, &seed.fork_context_id, &scope, &entries)?.as_ref()
                    == Some(&seed)
            {
                return Ok(None);
            }
            Ok(Some(replay_fork(db, &conversation, &attempt_context)?))
        })
        .await
    }
}

fn replay_fork(db: &Connection, conversation: &str, context: &str) -> Result<Vec<MessageRevision>> {
    let full = context_entries(db, conversation, context)?;
    let input = load_messages(db, conversation, &full)?;
    omit_deleted_dispatch_messages(db, input)
}
