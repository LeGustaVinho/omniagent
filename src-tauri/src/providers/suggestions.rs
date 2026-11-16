use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    conversations::{MessageRole, SessionScope},
    dispatch::{ContextSnapshot, EventSource},
    jobs::{ComposerSuggestionMode, read_suggestion_mode},
    protection,
    store::Store,
};

use super::{
    Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionMode, TurnOutcome,
};

const MAX_SUGGESTION_BYTES: usize = 2048;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionSource {
    pub provider_session_id: String,
    pub provider_turn_id: Option<String>,
    pub provider_result_id: Option<String>,
    pub message_id: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestionOutcome {
    Available { text: String },
    Empty,
    Failed,
    Unavailable,
    TimedOut,
    Cancelled,
    Ineligible,
}

impl SuggestionOutcome {
    pub(super) fn from_text(text: &Value) -> Self {
        match text.as_str().map(str::trim) {
            Some("") => Self::Empty,
            Some(text) if valid_suggestion(text) => Self::Available { text: text.into() },
            None if text.is_null() => Self::Empty,
            _ => Self::Failed,
        }
    }
}

pub(crate) fn valid_suggestion(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_SUGGESTION_BYTES
        && !text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSuggestion {
    pub source: SuggestionSource,
    pub outcome: SuggestionOutcome,
}

impl SuggestionSource {
    pub(crate) fn settlement(self, outcome: SuggestionOutcome) -> ProviderEvent {
        ProviderEvent {
            location: ProviderLocation {
                session_id: Some(self.provider_session_id.clone()),
                turn_id: self.provider_turn_id.clone(),
                ..Default::default()
            },
            data: ProviderEventData::PromptSuggestion {
                suggestion: PromptSuggestion {
                    source: self,
                    outcome,
                },
            },
        }
    }
}

impl Store {
    pub async fn composer_native_suggestion(
        &self,
        conversation: String,
        context: String,
        run: String,
        scope: SessionScope,
    ) -> Result<SuggestionOutcome> {
        self.execute(move |db| {
            Ok(
                native_suggestion(db, &conversation, &context, &run, &scope)?
                    .unwrap_or(SuggestionOutcome::Ineligible),
            )
        })
        .await
    }

    pub(crate) async fn scheduled_suggestion_enabled(&self, run: String) -> Result<bool> {
        self.execute(move |db| {
            if read_suggestion_mode(db)? == ComposerSuggestionMode::Off { return Ok(false); }
            Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN jobs j ON j.id = a.job_id
                 JOIN queued_conversation_instructions q ON q.job_id = j.id
                 JOIN agent_attempts turn ON turn.run_id = a.run_id AND turn.conversation_id = j.conversation_id
                 WHERE a.run_id = ?1 AND a.kind = 'initial' AND NOT turn.detached_context
                   AND j.origin_run_id IS NULL AND j.task_id IS NULL AND j.mode <> 'plan'
                   AND NOT EXISTS (SELECT 1 FROM goal_revisions g WHERE g.initial_job_id = j.id)
                   AND NOT EXISTS (SELECT 1 FROM goal_continuations g WHERE g.job_id = j.id))",
                [run], |row| row.get(0),
            )?)
        }).await
    }

    pub(crate) async fn scheduled_suggestion_still_wanted(
        &self,
        conversation: String,
    ) -> Result<bool> {
        self.execute(move |db| {
            Ok(read_suggestion_mode(db)? != ComposerSuggestionMode::Off
                && empty_composer(db, &conversation)?)
        })
        .await
    }
}

pub(crate) fn empty_composer(db: &Connection, conversation: &str) -> Result<bool> {
    let draft: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM drafts WHERE kind = 'conversation' AND owner_id = ?1 AND document = ''",
        [conversation], |row| row.get(0),
    ).optional()?;
    if let Some(draft) = draft
        && !std::str::from_utf8(&protection::unprotect(&draft)?)?
            .trim()
            .is_empty()
    {
        return Ok(false);
    }
    Ok(db.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM conversation_reference_drafts WHERE conversation_id = ?1)
           AND NOT EXISTS(SELECT 1 FROM conversation_attachment_drafts WHERE conversation_id = ?1)
           AND NOT EXISTS(SELECT 1 FROM open_agent_questions WHERE conversation_id = ?1)",
        [conversation],
        |row| row.get(0),
    )?)
}

pub(super) fn adapter_route_unavailable(scope: &SessionScope) -> bool {
    scope.provider == Provider::Antigravity
        || (scope.provider == Provider::Claude && scope.mode != SessionMode::ApprovedWrite)
}

pub(crate) fn native_suggestion(
    db: &Connection,
    conversation: &str,
    context: &str,
    run: &str,
    scope: &SessionScope,
) -> Result<Option<SuggestionOutcome>> {
    if read_suggestion_mode(db)? == ComposerSuggestionMode::Off
        || scope.mode == SessionMode::Plan
        || !empty_composer(db, conversation)?
    {
        return Ok(None);
    }
    let boundary: Option<(Vec<u8>, String, Option<String>)> = db.query_row(
        "SELECT snapshot.payload, session.provider_session_id, attempt.provider_turn_id
         FROM conversations c JOIN agent_attempts attempt ON attempt.conversation_id = c.id
         JOIN runs r ON r.id = attempt.run_id AND r.checkout_id = c.checkout_id
         JOIN agent_sessions session ON session.id = attempt.agent_session_id
         JOIN context_snapshots snapshot ON snapshot.run_id = r.id
         JOIN dispatches dispatch ON dispatch.snapshot_id = snapshot.id AND dispatch.ordinal = 1
         WHERE c.id = ?1 AND c.active_context_id = ?2 AND attempt.result_context_id = ?2 AND r.id = ?3
           AND NOT attempt.detached_context AND r.status = 'succeeded' AND attempt.provider_terminal = 'completed'
           AND session.provider_session_id IS NOT NULL AND dispatch.delivery = 'submitted'
           AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE conversation_id = c.id)
           AND NOT EXISTS (SELECT 1 FROM history_revoked_sessions WHERE session_id = session.id)
           AND NOT EXISTS (SELECT 1 FROM agent_attempts later WHERE later.conversation_id = c.id
                           AND NOT later.detached_context AND later.ordinal > attempt.ordinal)",
        params![conversation, context, run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((snapshot, provider_session, turn)) = boundary else {
        return Ok(None);
    };
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
    if &snapshot.scope != scope || snapshot.answered_question.is_some() {
        return Ok(None);
    }
    let Some(message) = snapshot
        .submitted_messages
        .last()
        .filter(|message| message.role == MessageRole::User)
    else {
        return Ok(None);
    };

    let mut query = db.prepare(
        "SELECT source, payload FROM agent_events WHERE run_id = ?1 ORDER BY seq DESC LIMIT 128",
    )?;
    let mut rows = query.query([run])?;
    let mut settlement = None;
    while let Some(row) = rows.next()? {
        let authority: String = row.get(0)?;
        let event: ProviderEvent =
            serde_json::from_slice(&protection::unprotect(row.get_ref(1)?.as_blob()?)?)?;
        if event.location.parent_item_id.is_some()
            || event.location.session_id.as_deref() != Some(&provider_session)
            || (turn.is_some() && event.location.turn_id != turn)
        {
            continue;
        }
        match event.data {
            ProviderEventData::PromptSuggestion { suggestion }
                if suggestion.source.provider_session_id == provider_session
                    && suggestion.source.message_id == message.message_id
                    && suggestion.source.provider_turn_id == event.location.turn_id
                    && (authority == EventSource::Provider.as_sql()
                        || (authority == EventSource::Supervisor.as_sql()
                            && matches!(
                                suggestion.outcome,
                                SuggestionOutcome::Failed
                                    | SuggestionOutcome::TimedOut
                                    | SuggestionOutcome::Cancelled
                            ))
                        || (authority == EventSource::Application.as_sql()
                            && suggestion.outcome == SuggestionOutcome::Unavailable
                            && adapter_route_unavailable(scope))) =>
            {
                if settlement.replace(suggestion).is_some() {
                    return Ok(None);
                }
            }
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Completed,
            } if authority == EventSource::Provider.as_sql() => {
                return Ok(settlement
                    .filter(|suggestion| {
                        suggestion.source.provider_result_id == event.location.item_id
                            && suggestion.source.provider_turn_id == event.location.turn_id
                    })
                    .map(|suggestion| match suggestion.outcome {
                        SuggestionOutcome::Available { text } => {
                            SuggestionOutcome::from_text(&Value::String(text))
                        }
                        other => other,
                    }));
            }
            _ => {}
        }
    }
    Ok(None)
}
