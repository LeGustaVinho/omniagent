use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    jobs::detached_question_context,
    protection,
    providers::{
        ProviderEvent, ProviderEventData, RequestId, capabilities::ReportedCapabilities,
        configuration,
    },
    repository,
    store::Store,
};

use super::{
    Conversation, ConversationHistory, MessageRole, SessionScope, activate_context,
    check_idle_base, context_entries, continuation::fork_history,
    fork_checkout::require_implicit_reuse, load_conversation, load_messages, new_id,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHistoryInspection {
    previous: Vec<PreviousConversationContext>,
    fork: Option<ConversationForkOrigin>,
    retained_questions: Vec<RetainedQuestion>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviousConversationContext {
    context_id: String,
    superseded_by: String,
    replaced_at: String,
    message_count: i64,
    action: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationForkOrigin {
    source: Option<Conversation>,
    source_context_id: Option<String>,
    source_revision_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RetainedQuestion {
    run_id: String,
    request_id: RequestId,
}

pub(super) fn retained_prefix(
    db: &Connection,
    conversation: &str,
    context: &str,
    through: Option<&str>,
) -> Result<Vec<String>> {
    let mut entries = context_entries(db, conversation, context)?;
    if let Some(revision) = through {
        let index = entries
            .iter()
            .position(|entry| entry == revision)
            .context("The selected message is outside the current conversation history")?;
        entries.truncate(index + 1);
    }
    Ok(entries)
}

pub(super) fn require_user_revision(
    db: &Connection,
    source: &Conversation,
    revision: &str,
) -> Result<()> {
    let selected = load_messages(db, &source.id, &[revision.into()])?
        .pop()
        .context("message is missing")?;
    ensure!(
        selected.role == MessageRole::User,
        "Only a user message can be edited in a fork"
    );
    Ok(())
}

pub(super) fn edit_fork_message(
    db: &Connection,
    fork: &mut Conversation,
    encrypted: Vec<u8>,
) -> Result<()> {
    let mut copied = context_entries(db, &fork.id, &fork.context_id)?;
    let original = copied.pop().context("the fork has no selected message")?;
    let revised = new_id();
    db.execute(
        "INSERT INTO message_revisions(id, message_id, revision, content)
         SELECT ?1, message_id, revision + 1, ?3 FROM message_revisions WHERE id = ?2",
        params![revised, original, encrypted],
    )?;
    db.execute(
        "INSERT INTO message_attachments(message_revision_id, payload)
         SELECT ?1, payload FROM message_attachments WHERE message_revision_id = ?2",
        params![revised, original],
    )?;
    super::reference_delivery::copy_references(db, &original, &revised)?;
    copied.push(revised);
    fork.context_id = activate_context(db, &fork.id, &copied)?;
    Ok(())
}

pub(super) fn record_history_replacement(
    db: &Connection,
    conversation: &str,
    previous: &str,
    next: &str,
    action: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO conversation_history_journal (conversation_id, previous_context_id, next_context_id, action)
         VALUES (?1, ?2, ?3, ?4)",
        params![conversation, previous, next, action],
    )?;
    Ok(())
}

impl Store {
    pub async fn inspect_conversation_history(
        &self,
        conversation_id: String,
    ) -> Result<ConversationHistoryInspection> {
        self.execute(move |db| {
            load_conversation(db, &conversation_id)?;
            let previous = db.prepare(
                "SELECT log.previous_context_id, log.next_context_id, log.replaced_at, json_array_length(context.entries), log.action
                 FROM conversation_history_journal log JOIN conversation_contexts context ON context.id = log.previous_context_id
                 WHERE log.conversation_id = ?1 ORDER BY log.sequence DESC",
            )?.query_map([&conversation_id], |row| Ok(PreviousConversationContext {
                context_id: row.get(0)?, superseded_by: row.get(1)?, replaced_at: row.get(2)?, message_count: row.get(3)?, action: row.get(4)?,
            }))?.collect::<rusqlite::Result<_>>()?;
            let fork = db.query_row(
                "SELECT source_conversation_id, source_context_id, source_revision_id FROM conversation_forks WHERE conversation_id = ?1",
                [&conversation_id], |row| Ok((row.get::<_, Option<String>>(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?.map(|(source, source_context_id, source_revision_id)| -> Result<_> {
                Ok(ConversationForkOrigin {
                    source: source.map(|id| load_conversation(db, &id)).transpose()?, source_context_id, source_revision_id,
                })
            }).transpose()?;
            let mut statement = db.prepare(
                "SELECT q.request_event_seq, q.run_id, event.payload FROM agent_questions q
                 JOIN agent_attempts a ON a.run_id = q.run_id
                 JOIN agent_events event ON event.seq = q.request_event_seq
                 LEFT JOIN native_question_waits w ON w.request_event_seq = q.request_event_seq
                 WHERE a.conversation_id = ?1 AND (q.delivery = 'pending' OR w.state IN ('saved', 'needs_review'))
                 ORDER BY q.request_event_seq",
            )?;
            let questions = statement.query_map([&conversation_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?))
            })?;
            let mut retained_questions = Vec::new();
            for question in questions {
                let (sequence, run_id, request) = question?;
                if detached_question_context(db, &conversation_id, &run_id, sequence)?.is_some() {
                    let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&request)?)?;
                    if let ProviderEventData::Question { request_id, .. } = event.data {
                        retained_questions.push(RetainedQuestion { run_id, request_id });
                    }
                }
            }
            Ok(ConversationHistoryInspection { previous, fork, retained_questions })
        }).await
    }

    pub async fn conversation_history_revision(
        &self,
        conversation_id: String,
        context_id: String,
    ) -> Result<ConversationHistory> {
        self.execute(move |db| {
            let exposed: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1 AND active_context_id = ?2
                 UNION ALL SELECT 1 FROM conversation_history_journal WHERE conversation_id = ?1 AND previous_context_id = ?2
                 UNION ALL SELECT 1 FROM conversation_forks WHERE source_conversation_id = ?1 AND source_context_id = ?2)",
                params![conversation_id, context_id], |row| row.get(0),
            )?;
            ensure!(exposed, "The selected context is outside the visible conversation history");
            let entries = context_entries(db, &conversation_id, &context_id)?;
            Ok(ConversationHistory { messages: load_messages(db, &conversation_id, &entries)?, context_id })
        }).await
    }

    pub async fn conversation_next_prompt_instructions(
        &self,
        conversation_id: String,
        scope: SessionScope,
    ) -> Result<configuration::InstructionInventory> {
        let conversation = self.conversation(conversation_id).await?;
        let layout =
            repository::verify_checkout(self, conversation.checkout_id, scope.checkout.clone())
                .await?;
        tokio::task::spawn_blocking(move || {
            let (_, _, instructions) =
                configuration::inspect(&scope, &layout, &ReportedCapabilities::default())?;
            Ok(instructions)
        })
        .await?
    }

    pub async fn fork_conversation_with_message_edit(
        &self,
        conversation_id: String,
        expected_context_id: String,
        through_revision: String,
        title: String,
        text: String,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        ensure!(!text.trim().is_empty(), "message is empty");
        let encrypted = protection::protect(text.as_bytes())?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source = check_idle_base(&tx, &conversation_id, &expected_context_id)?;
            require_implicit_reuse(&tx, &source)?;
            let entries = retained_prefix(
                &tx,
                &source.id,
                &expected_context_id,
                Some(&through_revision),
            )?;
            require_user_revision(&tx, &source, &through_revision)?;
            let mut fork = fork_history(&tx, &source, &entries, title)?;
            edit_fork_message(&tx, &mut fork, encrypted)?;
            tx.commit()?;
            Ok(fork)
        })
        .await
    }

    pub async fn fork_retained_conversation(
        &self,
        conversation_id: String,
        expected_context_id: String,
        source_context_id: String,
        through_revision: String,
        title: String,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut source = check_idle_base(&tx, &conversation_id, &expected_context_id)?;
            require_implicit_reuse(&tx, &source)?;
            let retained: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM conversation_history_journal
                   WHERE conversation_id = ?1 AND previous_context_id = ?2)",
                params![conversation_id, source_context_id],
                |row| row.get(0),
            )?;
            ensure!(
                retained,
                "Only a retained conversation revision can be forked here"
            );
            let entries =
                retained_prefix(&tx, &source.id, &source_context_id, Some(&through_revision))?;
            source.context_id = source_context_id;
            let fork = fork_history(&tx, &source, &entries, title)?;
            tx.commit()?;
            Ok(fork)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
