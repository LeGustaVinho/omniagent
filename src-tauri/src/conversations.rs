use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    attachments::{self, AttachmentPreview, CapturedAttachment},
    dispatch::{Delivery, EventSource},
    protection,
    providers::{
        ApprovalDecision, Provider, ProviderEvent, ProviderEventData, RequestId, SessionIntent,
        SessionMode, TurnOutcome,
    },
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct Conversation {
    pub id: String,
    pub checkout_id: String,
    pub title: String,
    pub context_id: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConversationSession {
    pub id: String,
    pub provider: Provider,
    pub provider_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct MessageRevision {
    pub message_id: String,
    pub revision_id: String,
    pub role: MessageRole,
    pub text: String,
}

/// A resume scope comes from the effective profile and checkout at admission.
/// An account that could not be identified does not qualify for native resume.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScope {
    pub provider: Provider,
    pub profile_id: String,
    pub account_binding: Option<String>,
    pub checkout: PathBuf,
    pub executable: PathBuf,
    pub cli_version: String,
    pub runner: String,
    pub permission_revision: String,
    pub mode: SessionMode,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub struct AttemptPlan {
    pub run: RunRecord,
    pub conversation_id: String,
    pub agent_session_id: String,
    pub previous_run_id: Option<String>,
    pub context_id: String,
    pub resume_cursor_id: Option<String>,
    pub intent: SessionIntent,
    pub scope: SessionScope,
    /// Suffix after the provider cursor, or the full effective context for a fresh session.
    pub input: Vec<MessageRevision>,
    pub(crate) attachments: Vec<(String, CapturedAttachment)>,
}

pub(crate) enum PermissionRoute {
    Native,
    ClaudePrompt,
}

impl PermissionRoute {
    fn as_sql(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::ClaudePrompt => "claude_prompt",
        }
    }
}

impl Store {
    pub async fn conversation(&self, id: String) -> Result<Conversation> {
        self.execute(move |db| load_conversation(db, &id)).await
    }
    pub async fn create_conversation(
        &self,
        checkout_id: String,
        title: String,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let id = new_id();
            tx.execute(
                "INSERT INTO conversations(id, checkout_id, title) VALUES (?1, ?2, ?3)",
                params![id, checkout_id, title],
            )?;
            let context_id = activate_context(&tx, &id, &[])?;
            tx.commit()?;
            Ok(Conversation {
                id,
                checkout_id,
                title,
                context_id,
            })
        })
        .await
    }

    pub async fn conversation_messages(
        &self,
        conversation_id: String,
    ) -> Result<Vec<MessageRevision>> {
        self.execute(move |db| {
            let conversation = load_conversation(db, &conversation_id)?;
            let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
            load_messages(db, &conversation.id, &entries)
        })
        .await
    }

    pub async fn rename_conversation(&self, id: String, title: String) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let updated = tx.execute(
                "UPDATE conversations SET title = ?1 WHERE id = ?2",
                params![title, id],
            )?;
            ensure!(updated == 1, "conversation does not exist");
            let conversation = load_conversation(&tx, &id)?;
            tx.commit()?;
            Ok(conversation)
        })
        .await
    }

    pub async fn conversation_sessions(&self, id: String) -> Result<Vec<ConversationSession>> {
        self.execute(move |db| {
            load_conversation(db, &id)?;
            let mut query = db.prepare(
                "SELECT s.id, json_extract(s.scope, '$.provider'), s.provider_session_id
                 FROM agent_sessions s JOIN agent_attempts a ON a.agent_session_id = s.id
                 WHERE a.conversation_id = ?1 AND s.conversation_id = ?1
                 GROUP BY s.id ORDER BY min(a.ordinal)",
            )?;
            let rows = query.query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            rows.map(|row| {
                let (id, provider, provider_session_id) = row?;
                let provider = serde_json::from_value(serde_json::Value::String(provider))?;
                Ok(ConversationSession {
                    id,
                    provider,
                    provider_session_id,
                })
            })
            .collect()
        })
        .await
    }

    pub async fn select_conversation_context(
        &self,
        conversation_id: String,
        base_id: String,
        revisions: Vec<String>,
    ) -> Result<String> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_idle_base(&tx, &conversation_id, &base_id)?;
            let messages = load_messages(&tx, &conversation_id, &revisions)?;
            let identities: HashSet<_> =
                messages.iter().map(|message| &message.message_id).collect();
            ensure!(
                identities.len() == revisions.len(),
                "context contains repeated messages"
            );
            let context = activate_context(&tx, &conversation_id, &revisions)?;
            tx.commit()?;
            Ok(context)
        })
        .await
    }

    pub async fn replace_conversation_message(
        &self,
        conversation_id: String,
        base_id: String,
        message_id: String,
        text: String,
    ) -> Result<String> {
        ensure!(!text.trim().is_empty(), "message is empty");
        let encrypted = protection::protect(text.as_bytes())?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            check_idle_base(&tx, &conversation_id, &base_id)?;
            let mut entries = context_entries(&tx, &conversation_id, &base_id)?;
            let messages = load_messages(&tx, &conversation_id, &entries)?;
            let index = messages
                .iter()
                .position(|message| message.message_id == message_id)
                .context("message is not in the active context")?;
            let revision = new_id();
            tx.execute(
                "INSERT INTO message_revisions(id, message_id, revision, content)
                 SELECT ?1, ?2, max(revision) + 1, ?3 FROM message_revisions WHERE message_id = ?2",
                params![revision, message_id, encrypted],
            )?;
            tx.execute(
                "INSERT INTO message_attachments(message_revision_id, payload)
                SELECT ?1, payload FROM message_attachments WHERE message_revision_id = ?2",
                params![revision, entries[index]],
            )?;
            entries[index] = revision;
            let context = activate_context(&tx, &conversation_id, &entries)?;
            tx.commit()?;
            Ok(context)
        })
        .await
    }

    pub async fn prepare_agent_attempt(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        text: String,
    ) -> Result<AttemptPlan> {
        self.prepare_agent_attempt_with_attachments(
            conversation_id,
            base_context_id,
            scope,
            text,
            Vec::new(),
        )
        .await
    }

    pub async fn prepare_agent_attempt_with_attachments(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        text: String,
        previews: Vec<AttachmentPreview>,
    ) -> Result<AttemptPlan> {
        ensure!(
            !text.trim().is_empty() || !previews.is_empty(),
            "message is empty"
        );
        let checkout = dunce::canonicalize(&scope.checkout)?;
        ensure!(
            checkout == scope.checkout,
            "admission must use the canonical checkout"
        );
        ensure!(
            scope.executable.is_absolute()
                && !scope.permission_revision.is_empty()
                && !scope.cli_version.is_empty()
                && !scope.runner.is_empty()
                && !scope.profile_id.is_empty(),
            "incomplete session scope"
        );
        let serialized = serde_json::to_string(&scope)?;
        let encrypted = protection::protect(text.as_bytes())?;
        let frozen = if previews.is_empty() {
            None
        } else {
            let conversation = self.conversation(conversation_id.clone()).await?;
            let layout = crate::repository::verify_checkout(
                self,
                conversation.checkout_id,
                checkout.clone(),
            )
            .await?;
            let root = layout.root;
            Some(
                tokio::task::spawn_blocking(move || -> Result<_> {
                    let captured = attachments::capture(&root, previews)?;
                    let payload = protection::protect(&serde_json::to_vec(&captured)?)?;
                    Ok((root, payload))
                })
                .await??,
            )
        };
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = check_idle_base(&tx, &conversation_id, &base_context_id)?;
            let registered: String = tx.query_row(
                "SELECT root FROM checkouts WHERE id = ?1", [&conversation.checkout_id], |row| row.get(0),
            )?;
            ensure!(checkout.starts_with(Path::new(&registered)), "working directory is outside the conversation checkout");
            if let Some((root, _)) = &frozen {
                ensure!(root == Path::new(&registered), "attachment checkout changed after admission");
            }
            let mut entries = context_entries(&tx, &conversation_id, &base_context_id)?;
            let previous = previous_attempt(&tx, &conversation_id)?;
            let continuity = match &previous {
                Some(prior) if prior.scope == serialized && scope.account_binding.as_ref().is_some_and(|id| !id.is_empty()) =>
                    append_only_cursor(&tx, &conversation_id, &entries, prior)?,
                _ => None,
            };

            let (agent_session_id, resume_cursor_id, intent, consumed) = if let Some((prior, cursor, consumed)) = continuity {
                let provider_session_id = prior.provider_session_id.clone().expect("checked native ID");
                (prior.session_id.clone(), Some(cursor), SessionIntent::Resume { provider_session_id }, consumed)
            } else {
                let id = new_id();
                tx.execute("INSERT INTO agent_sessions(id, conversation_id, scope) VALUES (?1, ?2, ?3)",
                    params![id, conversation_id, serialized])?;
                (id, None, SessionIntent::Start, 0)
            };

            let revision = insert_message(&tx, &conversation_id, MessageRole::User, encrypted)?;
            if let Some((_, payload)) = frozen {
                tx.execute("INSERT INTO message_attachments(message_revision_id, payload) VALUES (?1, ?2)", params![revision, payload])?;
            }
            entries.push(revision);
            let context_id = activate_context(&tx, &conversation_id, &entries)?;
            let input = load_messages(&tx, &conversation_id, &entries[consumed..])?;
            let mut attachments = Vec::new();
            for message in &input {
                let payload = tx.query_row("SELECT payload FROM message_attachments WHERE message_revision_id = ?1",
                    [&message.revision_id], |row| row.get::<_, Vec<u8>>(0)).optional()?;
                if let Some(payload) = payload {
                    let items: Vec<CapturedAttachment> = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                    attachments.extend(items.into_iter().map(|item| (message.revision_id.clone(), item)));
                }
            }
            attachments::check_limits(attachments.iter().map(|(_, item)| item.metadata.size))?;
            let (run, _) = runs::insert_run(&tx, conversation.checkout_id, "agent")?;
            let ordinal = previous.as_ref().map_or(1, |prior| prior.ordinal + 1);
            let previous_run_id = previous.map(|prior| prior.run_id);
            tx.execute(
                "INSERT INTO agent_attempts(run_id, turn_id, conversation_id, ordinal, agent_session_id,
                 previous_run_id, context_id, resume_cursor_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![run.id, run.turn_id, conversation_id, ordinal, agent_session_id,
                    previous_run_id, context_id, resume_cursor_id],
            )?;
            tx.execute("INSERT INTO active_agent_attempts VALUES (?1, ?2)", params![conversation_id, run.id])?;
            tx.commit()?;
            Ok(AttemptPlan { run, conversation_id, agent_session_id, previous_run_id, context_id,
                resume_cursor_id, intent, scope, input, attachments })
        }).await
    }

    pub async fn record_agent_observation(
        &self,
        run_id: String,
        source: EventSource,
        event: ProviderEvent,
    ) -> Result<()> {
        let payload = protection::protect(&serde_json::to_vec(&event)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active = active_attempt(&tx, &run_id)?;
            let primary = source == EventSource::Provider && event.location.parent_item_id.is_none()
                && !matches!(event.data, ProviderEventData::Diagnostic { .. });
            if primary && let Some(id) = &event.location.session_id {
                let old: Option<String> = tx.query_row(
                    "SELECT provider_session_id FROM agent_sessions WHERE id = ?1", [&active.session_id], |row| row.get(0),
                )?;
                ensure!(old.as_ref().is_none_or(|old| old == id), "provider session changed during the attempt");
                tx.execute("UPDATE agent_sessions SET provider_session_id = ?1 WHERE id = ?2", params![id, active.session_id])?;
            }
            if primary && let Some(id) = &event.location.turn_id {
                let old: Option<String> = tx.query_row(
                    "SELECT provider_turn_id FROM agent_attempts WHERE run_id = ?1", [&run_id], |row| row.get(0),
                )?;
                ensure!(old.as_ref().is_none_or(|old| old == id), "provider turn changed during the attempt");
                tx.execute("UPDATE agent_attempts SET provider_turn_id = ?1 WHERE run_id = ?2", params![id, run_id])?;
            }
            match &event.data {
                ProviderEventData::SessionReady { model, effort } | ProviderEventData::ModelObserved { model, effort } if primary => {
                    tx.execute("UPDATE agent_attempts SET reported_model = coalesce(?1, reported_model), reported_effort = coalesce(?2, reported_effort) WHERE run_id = ?3",
                        params![model, effort, run_id])?;
                }
                ProviderEventData::TurnFinished { outcome } if primary => {
                    let state = match outcome {
                        TurnOutcome::Completed => "completed",
                        TurnOutcome::Failed => "failed",
                        TurnOutcome::Interrupted => "interrupted",
                    };
                    tx.execute("UPDATE agent_attempts SET provider_terminal = ?1 WHERE run_id = ?2", params![state, run_id])?;
                }
                _ => {}
            }
            tx.execute("INSERT INTO agent_events(run_id, source, payload) VALUES (?1, ?2, ?3)", params![run_id, source.as_sql(), payload])?;
            if source == EventSource::Provider && let ProviderEventData::Approval { request_id, prompt, .. } = &event.data {
                let route = if prompt.is_some() { PermissionRoute::ClaudePrompt } else { PermissionRoute::Native };
                if prompt.is_some() {
                    let pending: u32 = tx.query_row(
                        "SELECT count(*) FROM agent_permissions WHERE run_id = ?1 AND route = 'claude_prompt' AND delivery IN ('pending', 'prepared')",
                        [&run_id], |row| row.get(0),
                    )?;
                    ensure!(pending < 16, "too many active Claude permission prompts");
                }
                tx.execute("INSERT INTO agent_permissions(request_event_seq, run_id, turn_id, request_key, delivery, route)
                    VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                    params![tx.last_insert_rowid(), run_id, active.turn_id, permission_key(request_id)?, route.as_sql()])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub(crate) async fn prepare_permission_decision(
        &self,
        run_id: String,
        turn_id: String,
        request_id: RequestId,
        decision: ApprovalDecision,
        route: PermissionRoute,
    ) -> Result<i64> {
        let key = permission_key(&request_id)?;
        let decision = match decision {
            ApprovalDecision::AllowOnce => "allow_once",
            ApprovalDecision::Deny => "deny",
            ApprovalDecision::Cancel => "cancel",
        };
        self.execute(move |db| {
            db.query_row(
                "UPDATE agent_permissions SET delivery = 'prepared', decision = ?1
                 WHERE run_id = ?2 AND turn_id = ?3 AND request_key = ?4 AND delivery = 'pending' AND route = ?5
                   AND EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = ?2)
                 RETURNING request_event_seq",
                params![decision, run_id, turn_id, key, route.as_sql()],
                |row| row.get(0),
            )
            .context("permission request is absent or stale for this turn")
        })
        .await
    }

    pub(crate) async fn settle_permission_decision(
        &self,
        request_event_seq: i64,
        delivery: Delivery,
    ) -> Result<()> {
        ensure!(
            delivery != Delivery::Prepared,
            "decision must have a delivery result"
        );
        self.execute(move |db| {
            let updated = db.execute(
                "UPDATE agent_permissions SET delivery = ?1 WHERE request_event_seq = ?2 AND delivery = 'prepared'",
                params![delivery.as_sql(), request_event_seq],
            )?;
            ensure!(updated == 1, "permission decision is no longer prepared");
            Ok(())
        }).await
    }

    pub(crate) async fn pending_claude_permission(
        &self,
        run_id: String,
        turn_id: String,
        id: RequestId,
    ) -> Result<Option<(i64, Option<ApprovalDecision>)>> {
        let key = permission_key(&id)?;
        self.execute(move |db| {
            let row = db.query_row(
                "SELECT request_event_seq, decision FROM agent_permissions WHERE run_id = ?1 AND turn_id = ?2
                    AND request_key = ?3 AND route = 'claude_prompt' AND delivery IN ('pending', 'prepared')",
                params![run_id, turn_id, key],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            ).optional()?;
            row.map(|(seq, decision)| {
                let decision = decision.map(|value| serde_json::from_value(serde_json::Value::String(value))).transpose()?;
                Ok((seq, decision))
            }).transpose()
        }).await
    }

    pub async fn mark_agent_running(&self, run_id: String) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            active_attempt(&tx, &run_id)?;
            let run = runs::load_run(&tx, &run_id)?;
            runs::advance_run(&tx, &run, RunStatus::Running)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn finish_agent_attempt(
        &self,
        run_id: String,
        outcome: RunStatus,
        response: Option<String>,
    ) -> Result<()> {
        ensure!(outcome.is_terminal(), "attempt outcome must be terminal");
        let encrypted = response
            .filter(|text| !text.is_empty())
            .map(|text| protection::protect(text.as_bytes()))
            .transpose()?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active = active_attempt(&tx, &run_id)?;
            let run = runs::load_run(&tx, &run_id)?;
            runs::advance_run(&tx, &run, outcome)?;
            let context_id = if let Some(encrypted) = encrypted {
                let mut entries = context_entries(&tx, &active.conversation_id, &active.context_id)?;
                entries.push(insert_message(&tx, &active.conversation_id, MessageRole::Assistant, encrypted)?);
                activate_context(&tx, &active.conversation_id, &entries)?
            } else {
                active.context_id
            };
            if outcome == RunStatus::Succeeded {
                tx.execute("UPDATE agent_sessions SET cursor_context_id = ?1 WHERE id = ?2 AND provider_session_id IS NOT NULL
                    AND EXISTS(SELECT 1 FROM agent_attempts a
                        JOIN context_snapshots s ON s.run_id = a.run_id
                        JOIN dispatches d ON d.snapshot_id = s.id
                        WHERE a.run_id = ?3 AND a.provider_terminal = 'completed' AND d.delivery = 'submitted')",
                    params![context_id, active.session_id, run_id])?;
            }
            close_permissions(&tx, &run_id)?;
            tx.execute("DELETE FROM active_agent_attempts WHERE run_id = ?1", [&run_id])?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn recover_agent_attempts_after_restart(&self) -> Result<()> {
        self.execute(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active = {
                let mut query = tx.prepare("SELECT run_id FROM active_agent_attempts")?;
                query
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            for id in active {
                let run = runs::load_run(&tx, &id)?;
                runs::advance_run(&tx, &run, RunStatus::Interrupted)?;
                close_permissions(&tx, &id)?;
            }
            tx.execute(
                "UPDATE dispatches SET delivery = 'unconfirmed' WHERE delivery = 'prepared'",
                [],
            )?;
            tx.execute("DELETE FROM active_agent_attempts", [])?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn load_conversation(db: &Connection, id: &str) -> Result<Conversation> {
    Ok(db.query_row(
        "SELECT id, checkout_id, title, active_context_id FROM conversations WHERE id = ?1",
        [id],
        |row| {
            Ok(Conversation {
                id: row.get(0)?,
                checkout_id: row.get(1)?,
                title: row.get(2)?,
                context_id: row.get(3)?,
            })
        },
    )?)
}

fn check_idle_base(db: &Connection, id: &str, requested: &str) -> Result<Conversation> {
    let conversation = load_conversation(db, id)?;
    ensure!(
        conversation.context_id == requested,
        "conversation context changed"
    );
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    ensure!(!active, "conversation already has an active turn");
    Ok(conversation)
}

fn context_entries(
    db: &Connection,
    conversation_id: &str,
    context_id: &str,
) -> Result<Vec<String>> {
    let encoded: String = db.query_row(
        "SELECT entries FROM conversation_contexts WHERE id = ?1 AND conversation_id = ?2",
        params![context_id, conversation_id],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&encoded)?)
}

fn activate_context(db: &Connection, conversation_id: &str, entries: &[String]) -> Result<String> {
    let id = new_id();
    db.execute(
        "INSERT INTO conversation_contexts(id, conversation_id, entries) VALUES (?1, ?2, ?3)",
        params![id, conversation_id, serde_json::to_string(entries)?],
    )?;
    db.execute(
        "UPDATE conversations SET active_context_id = ?1 WHERE id = ?2",
        params![id, conversation_id],
    )?;
    Ok(id)
}

fn insert_message(
    db: &Connection,
    conversation_id: &str,
    role: MessageRole,
    payload: Vec<u8>,
) -> Result<String> {
    let message = new_id();
    let revision = new_id();
    let role = match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    };
    db.execute(
        "INSERT INTO messages(id, conversation_id, role) VALUES (?1, ?2, ?3)",
        params![message, conversation_id, role],
    )?;
    db.execute(
        "INSERT INTO message_revisions(id, message_id, revision, content) VALUES (?1, ?2, 1, ?3)",
        params![revision, message, payload],
    )?;
    Ok(revision)
}

fn load_messages(
    db: &Connection,
    conversation_id: &str,
    entries: &[String],
) -> Result<Vec<MessageRevision>> {
    let mut statement = db.prepare(
        "SELECT m.id, m.role, r.content FROM message_revisions r JOIN messages m ON m.id = r.message_id
         WHERE r.id = ?1 AND m.conversation_id = ?2",
    )?;
    entries
        .iter()
        .map(|revision_id| {
            let (message_id, role, content): (String, String, Vec<u8>) = statement
                .query_row(params![revision_id, conversation_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            Ok(MessageRevision {
                message_id,
                revision_id: revision_id.clone(),
                role: if role == "user" {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                text: String::from_utf8(protection::unprotect(&content)?)?,
            })
        })
        .collect()
}

struct PreviousAttempt {
    run_id: String,
    ordinal: i64,
    session_id: String,
    scope: String,
    provider_session_id: Option<String>,
    cursor: Option<String>,
}

fn previous_attempt(db: &Connection, conversation: &str) -> Result<Option<PreviousAttempt>> {
    Ok(db
        .query_row(
            "SELECT a.run_id, a.ordinal, s.id, s.scope, s.provider_session_id, s.cursor_context_id
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE a.conversation_id = ?1 ORDER BY a.ordinal DESC LIMIT 1",
            [conversation],
            |row| {
                Ok(PreviousAttempt {
                    run_id: row.get(0)?,
                    ordinal: row.get(1)?,
                    session_id: row.get(2)?,
                    scope: row.get(3)?,
                    provider_session_id: row.get(4)?,
                    cursor: row.get(5)?,
                })
            },
        )
        .optional()?)
}

fn append_only_cursor<'a>(
    db: &Connection,
    conversation: &str,
    entries: &[String],
    prior: &'a PreviousAttempt,
) -> Result<Option<(&'a PreviousAttempt, String, usize)>> {
    let (Some(cursor), Some(_)) = (&prior.cursor, &prior.provider_session_id) else {
        return Ok(None);
    };
    let known = context_entries(db, conversation, cursor)?;
    Ok(entries
        .starts_with(&known)
        .then(|| (prior, cursor.clone(), known.len())))
}

struct ActiveAttempt {
    conversation_id: String,
    session_id: String,
    context_id: String,
    turn_id: String,
}

fn active_attempt(db: &Connection, id: &str) -> Result<ActiveAttempt> {
    db.query_row(
        "SELECT a.conversation_id, a.agent_session_id, a.context_id, a.turn_id
         FROM agent_attempts a JOIN active_agent_attempts l ON l.run_id = a.run_id WHERE a.run_id = ?1",
        [id], |row| Ok(ActiveAttempt { conversation_id:row.get(0)?, session_id:row.get(1)?, context_id:row.get(2)?, turn_id:row.get(3)? }),
    ).context("agent attempt is no longer active")
}

fn permission_key(id: &RequestId) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(id)?).into())
}

fn close_permissions(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE agent_permissions SET delivery = CASE delivery WHEN 'pending' THEN 'closed' ELSE 'unconfirmed' END
         WHERE run_id = ?1 AND delivery IN ('pending', 'prepared')",
        [run_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dispatch::{ContextSnapshot, Delivery, NativeHistory},
        process::TerminalOutcome,
        process_io::{CapturedProcess, OutputFrame, ProcessInput, Stream},
        providers::{
            Capability, ProviderLocation,
            capabilities::{CapabilitySubject, EvidenceOrigin, ObservedResult},
            runtime::start_managed_turn,
        },
        runner::{DirectWindowsRunner, ExecutionPlatform, LaunchRequest, RunControl, Runner},
    };
    use serde_json::{Value, json};
    use std::{
        io,
        process::Command,
        sync::{Arc, Mutex, mpsc},
        time::Duration,
    };

    async fn setup() -> Result<(tempfile::TempDir, Store, Conversation, SessionScope)> {
        let directory = tempfile::tempdir()?;
        let checkout = dunce::canonicalize(directory.path())?;
        let store = Store::open(checkout.join("store.sqlite"))?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&checkout)
                .status()?
                .success(),
            "fixture checkout could not initialize"
        );
        let registered = crate::repository::register(&store, checkout.clone()).await?;
        let conversation = store
            .create_conversation(registered.checkout_id, "Scoped chat".into())
            .await?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: "local-profile".into(),
            account_binding: Some("account-a".into()),
            executable: checkout.join("codex.exe"),
            checkout,
            cli_version: "1.2.3".into(),
            runner: "fixture-runner".into(),
            permission_revision: "scope-1".into(),
            mode: SessionMode::ReadOnly,
            model: Some("requested-model".into()),
            effort: Some("low".into()),
        };
        Ok((directory, store, conversation, scope))
    }

    async fn prepare(
        store: &Store,
        conversation: &str,
        scope: SessionScope,
        text: &str,
    ) -> Result<AttemptPlan> {
        let current = store.conversation(conversation.to_owned()).await?;
        store
            .prepare_agent_attempt(current.id, current.context_id, scope, text.into())
            .await
    }

    #[tokio::test]
    async fn permission_decisions_are_linked_to_the_request_and_its_active_turn() -> Result<()> {
        let (_directory, store, chat, scope) = setup().await?;
        let other = store
            .create_conversation(chat.checkout_id, "Other turn".into())
            .await?;
        let first = prepare(&store, &chat.id, scope.clone(), "first").await?.run;
        let second = prepare(&store, &other.id, scope, "second").await?.run;
        let request = RequestId::Number(3);
        for run in [&first, &second] {
            store.mark_agent_running(run.id.clone()).await?;
            store
                .record_agent_observation(
                    run.id.clone(),
                    EventSource::Provider,
                    ProviderEvent {
                        location: ProviderLocation::default(),
                        data: ProviderEventData::Approval {
                            request_id: request.clone(),
                            operation: "shell".into(),
                            description: None,
                            prompt: None,
                        },
                    },
                )
                .await?;
        }
        assert!(
            store
                .prepare_permission_decision(
                    first.id.clone(),
                    second.turn_id.clone(),
                    request.clone(),
                    ApprovalDecision::AllowOnce,
                    PermissionRoute::Native,
                )
                .await
                .is_err()
        );
        assert!(
            store
                .prepare_permission_decision(
                    first.id.clone(),
                    first.turn_id.clone(),
                    RequestId::Text("3".into()),
                    ApprovalDecision::AllowOnce,
                    PermissionRoute::Native,
                )
                .await
                .is_err()
        );
        let first_seq = store
            .prepare_permission_decision(
                first.id.clone(),
                first.turn_id.clone(),
                request.clone(),
                ApprovalDecision::Deny,
                PermissionRoute::Native,
            )
            .await?;
        store
            .settle_permission_decision(first_seq, Delivery::Submitted)
            .await?;
        assert!(
            store
                .prepare_permission_decision(
                    first.id.clone(),
                    first.turn_id.clone(),
                    request.clone(),
                    ApprovalDecision::AllowOnce,
                    PermissionRoute::Native,
                )
                .await
                .is_err()
        );
        let second_seq = store
            .prepare_permission_decision(
                second.id.clone(),
                second.turn_id.clone(),
                request.clone(),
                ApprovalDecision::Cancel,
                PermissionRoute::Native,
            )
            .await?;
        store.recover_agent_attempts_after_restart().await?;
        assert!(
            store
                .prepare_permission_decision(
                    second.id.clone(),
                    second.turn_id.clone(),
                    request,
                    ApprovalDecision::AllowOnce,
                    PermissionRoute::Native,
                )
                .await
                .is_err()
        );

        for (seq, run, expected_decision, expected_delivery) in [
            (first_seq, first, "deny", "submitted"),
            (second_seq, second, "cancel", "unconfirmed"),
        ] {
            let linked = store.execute(move |db| {
                Ok(db.query_row(
                    "SELECT e.run_id, p.turn_id, p.decision, p.delivery FROM agent_permissions p
                     JOIN agent_events e ON e.seq = p.request_event_seq AND e.run_id = p.run_id
                     WHERE e.seq = ?1 AND e.source = 'provider'",
                    [seq], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
                )?)
            }).await?;
            assert_eq!(
                linked,
                (
                    run.id,
                    run.turn_id,
                    expected_decision.into(),
                    expected_delivery.into()
                )
            );
        }
        Ok(())
    }

    async fn confirm(store: &Store, attempt: &AttemptPlan) -> Result<()> {
        store.mark_agent_running(attempt.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(store, attempt).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some("native-thread".into()),
                        turn_id: Some("native-turn".into()),
                        ..Default::default()
                    },
                    data: ProviderEventData::SessionReady {
                        model: Some("observed-model".into()),
                        effort: Some("high".into()),
                    },
                },
            )
            .await?;
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::TurnFinished {
                        outcome: TurnOutcome::Completed,
                    },
                },
            )
            .await?;
        store
            .finish_agent_attempt(
                attempt.run.id.clone(),
                RunStatus::Succeeded,
                Some("answer".into()),
            )
            .await
    }

    async fn prepare_fixture_dispatch(store: &Store, attempt: &AttemptPlan) -> Result<String> {
        let native_history = match (&attempt.intent, &attempt.resume_cursor_id) {
            (
                SessionIntent::Resume {
                    provider_session_id,
                },
                Some(context_id),
            ) => NativeHistory::ProviderCursor {
                session_id: provider_session_id.clone(),
                context_id: context_id.clone(),
            },
            _ => NativeHistory::Fresh,
        };
        store
            .prepare_dispatch(
                attempt.run.id.clone(),
                ContextSnapshot {
                    version: 1,
                    context_id: attempt.context_id.clone(),
                    scope: attempt.scope.clone(),
                    base_commit: None,
                    submitted_messages: attempt.input.clone(),
                    native_history,
                    capabilities: None,
                    filesystem_skills: Vec::new(),
                    reported_skills: None,
                    instructions: None,
                    scoped_launch: None,
                    requested_skill: None,
                    plugin_inventory: None,
                    attachments: Vec::new(),
                },
                vec![b"{\"fixture\":true}\n".to_vec()],
            )
            .await
    }

    #[tokio::test]
    async fn renaming_keeps_the_saved_native_session_and_checkout_binding() -> Result<()> {
        let (_dir, store, conversation, scope) = setup().await?;
        let first = prepare(&store, &conversation.id, scope.clone(), "confirmed").await?;
        confirm(&store, &first).await?;
        let original = store.conversation(conversation.id.clone()).await?;
        let unrelated = store
            .create_conversation(original.checkout_id.clone(), original.title.clone())
            .await?;
        let links = store.conversation_sessions(original.id.clone()).await?;
        assert_eq!(
            links,
            [ConversationSession {
                id: first.agent_session_id.clone(),
                provider: Provider::Codex,
                provider_session_id: Some("native-thread".into()),
            }]
        );

        let renamed = store
            .rename_conversation(original.id.clone(), "Nova conversa".into())
            .await?;
        assert_eq!(renamed.id, original.id);
        assert_eq!(renamed.checkout_id, original.checkout_id);
        assert_eq!(renamed.context_id, original.context_id);
        assert_eq!(renamed.title, "Nova conversa");
        assert_eq!(
            store.conversation(unrelated.id.clone()).await?.title,
            original.title
        );
        assert!(store.conversation_sessions(unrelated.id).await?.is_empty());
        assert_eq!(
            store.conversation_sessions(original.id.clone()).await?,
            links
        );

        let next = prepare(&store, &original.id, scope, "continue").await?;
        assert_eq!(next.agent_session_id, first.agent_session_id);
        assert!(matches!(next.intent, SessionIntent::Resume { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn changed_scope_seeds_a_fresh_session_and_keeps_attempt_lineage() -> Result<()> {
        for change in [
            |scope: &mut SessionScope| scope.account_binding = Some("different".into()),
            |scope: &mut SessionScope| scope.account_binding = None,
            |scope: &mut SessionScope| scope.runner = "different-runner".into(),
            |scope: &mut SessionScope| scope.permission_revision = "changed-permissions".into(),
            |scope: &mut SessionScope| scope.provider = Provider::Claude,
        ] {
            let (_dir, store, conversation, mut scope) = setup().await?;
            let first = prepare(&store, &conversation.id, scope.clone(), "old").await?;
            confirm(&store, &first).await?;
            change(&mut scope);
            let next = prepare(&store, &conversation.id, scope, "new").await?;
            assert!(matches!(next.intent, SessionIntent::Start));
            assert_eq!(next.previous_run_id.as_deref(), Some(first.run.id.as_str()));
            assert_ne!(next.agent_session_id, first.agent_session_id);
            assert_eq!(
                next.input
                    .iter()
                    .map(|message| message.text.as_str())
                    .collect::<Vec<_>>(),
                ["old", "answer", "new"]
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn changed_or_reordered_messages_invalidate_the_native_cursor() -> Result<()> {
        for mutation in ["replace", "remove", "reorder"] {
            let (_dir, store, conversation, scope) = setup().await?;
            let first = prepare(&store, &conversation.id, scope.clone(), "original").await?;
            confirm(&store, &first).await?;
            let active = store.conversation(conversation.id.clone()).await?;
            let messages = store.conversation_messages(active.id.clone()).await?;
            if mutation == "replace" {
                store
                    .replace_conversation_message(
                        active.id.clone(),
                        active.context_id,
                        messages[0].message_id.clone(),
                        "edited".into(),
                    )
                    .await?;
                let revised = store.conversation_messages(active.id.clone()).await?;
                assert_eq!(messages[0].message_id, revised[0].message_id);
                assert_ne!(messages[0].revision_id, revised[0].revision_id);
            } else {
                let mut entries = messages
                    .iter()
                    .map(|item| item.revision_id.clone())
                    .collect::<Vec<_>>();
                if mutation == "remove" {
                    entries.remove(0);
                } else {
                    entries.reverse();
                }
                store
                    .select_conversation_context(active.id.clone(), active.context_id, entries)
                    .await?;
            }
            let next = prepare(&store, &active.id, scope, "again").await?;
            assert!(matches!(next.intent, SessionIntent::Start), "{mutation}");
            assert_ne!(next.agent_session_id, first.agent_session_id);
        }
        Ok(())
    }

    #[tokio::test]
    async fn active_turn_is_unique_and_unconfirmed_history_is_seeded_after_a_restart() -> Result<()>
    {
        let (_dir, store, conversation, scope) = setup().await?;
        let first = prepare(&store, &conversation.id, scope.clone(), "pending").await?;
        assert!(
            prepare(&store, &conversation.id, scope.clone(), "concurrent")
                .await
                .is_err()
        );
        store.recover_agent_attempts_after_restart().await?;
        let status = store
            .execute(move |db| Ok(runs::load_run(db, &first.run.id)?.status))
            .await?;
        assert_eq!(status, RunStatus::Interrupted);
        let reopened = prepare(&store, &conversation.id, scope, "after restart").await?;
        assert!(matches!(reopened.intent, SessionIntent::Start));
        assert_eq!(
            reopened
                .input
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["pending", "after restart"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn stopped_before_dispatch_can_resume_from_the_last_verified_cursor() -> Result<()> {
        let (_dir, store, conversation, scope) = setup().await?;
        let first = prepare(&store, &conversation.id, scope.clone(), "confirmed").await?;
        confirm(&store, &first).await?;
        let stopped = prepare(&store, &conversation.id, scope.clone(), "unsent").await?;
        store
            .finish_agent_attempt(stopped.run.id.clone(), RunStatus::Interrupted, None)
            .await?;
        let resumed = prepare(&store, &conversation.id, scope, "continue").await?;
        assert!(
            matches!(&resumed.intent, SessionIntent::Resume { provider_session_id } if provider_session_id == "native-thread")
        );
        assert_eq!(
            resumed.previous_run_id.as_deref(),
            Some(stopped.run.id.as_str())
        );
        assert_eq!(
            resumed
                .input
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["unsent", "continue"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn restart_marks_a_prepared_dispatch_unconfirmed_and_invalidates_resume() -> Result<()> {
        let (_dir, store, conversation, scope) = setup().await?;
        let first = prepare(&store, &conversation.id, scope.clone(), "confirmed").await?;
        confirm(&store, &first).await?;
        let lost = prepare(&store, &conversation.id, scope.clone(), "possibly sent").await?;
        assert!(matches!(lost.intent, SessionIntent::Resume { .. }));
        store.mark_agent_running(lost.run.id.clone()).await?;
        prepare_fixture_dispatch(&store, &lost).await?;

        store.recover_agent_attempts_after_restart().await?;
        let dispatch = store
            .agent_dispatch(conversation.id.clone(), lost.run.id)
            .await?
            .unwrap();
        assert_eq!(dispatch.delivery, Delivery::Unconfirmed);
        let next = prepare(&store, &conversation.id, scope, "continue").await?;
        assert!(matches!(next.intent, SessionIntent::Start));
        Ok(())
    }

    struct CompletedProcess;

    impl RunControl for CompletedProcess {
        fn cancel(&self) -> io::Result<bool> {
            Ok(false)
        }
        fn wait(&self) -> io::Result<TerminalOutcome> {
            Ok(TerminalOutcome::Exited(0))
        }
    }

    #[derive(Clone)]
    struct WireLog {
        store: Store,
        runtime: tokio::runtime::Handle,
        conversation: String,
        frames: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl io::Write for WireLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let frame: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
            if frame["method"] == "turn/start" {
                let evidence = self.runtime.block_on(async {
                    let owner = self.conversation.clone();
                    let run_id = self.store.execute(move |db| Ok(db.query_row(
                        "SELECT run_id FROM active_agent_attempts WHERE conversation_id = ?1", [owner], |row| row.get::<_, String>(0),
                    )?)).await?;
                    self.store.agent_dispatch(self.conversation.clone(), run_id).await
                }).map_err(io::Error::other)?.ok_or_else(|| io::Error::other("turn wrote before dispatch evidence was committed"))?;
                assert_eq!(evidence.delivery, Delivery::Prepared);
                assert_eq!(evidence.frames, [bytes]);
                for attachment in &evidence.snapshot.attachments {
                    if let Some(path) = &attachment.input.path {
                        assert!(std::fs::File::open(path).is_ok());
                        assert!(std::fs::OpenOptions::new().write(true).open(path).is_err());
                    }
                }
            }
            self.frames.lock().unwrap().push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FixtureRunner(WireLog);

    impl Runner for FixtureRunner {
        fn identity(&self) -> &str {
            "fixture-runner"
        }
        fn execution_platform(&self) -> Option<ExecutionPlatform> {
            DirectWindowsRunner.execution_platform()
        }
        fn executable_revision(&self, _: &std::path::Path) -> Option<String> {
            Some("scripted-codex-build".into())
        }
        fn attachment_path(&self, path: &std::path::Path) -> Option<PathBuf> {
            DirectWindowsRunner.attachment_path(path)
        }
        fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
            let (write, output) = mpsc::channel();
            if request.arguments == ["plugin", "list", "--json"].map(std::ffi::OsStr::new) {
                write.send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: json!({"installed":[{"pluginId":"fixture@local","enabled":false,"version":"1.0"}]}).to_string(),
                })).unwrap();
                return Ok(CapturedProcess {
                    input: ProcessInput::new(io::sink()),
                    output,
                    control: Arc::new(CompletedProcess),
                });
            }
            for data in [
                json!({"id":1,"result":{"userAgent":"fixture"}}),
                json!({"id":2,"result":{"thread":{"id":"native-session","turns":[{"id":"history"}]},"model":"observed","cwd":request.checkout,
                    "sandbox":{"type":"readOnly"}}}),
                json!({"id":3,"result":{"data":[]}}),
                json!({"id":4,"result":{"config":{"approval_policy":"never","sandbox_mode":"read-only"}}}),
                json!({"id":5,"result":{"data":[{"cwd":request.checkout,
                    "skills":[
                        {"name":"fixture","path":request.checkout.join(".agents/skills/fixture/SKILL.md"),"scope":"repo","enabled":true},
                        {"name":"fixture","path":request.checkout.join("other-profile/skills/fixture/SKILL.md"),"scope":"user","enabled":false}],
                    "errors":[]}]}}),
                json!({"method":"item/agentMessage/delta","params":{"threadId":"native-session","turnId":"diagnostic-only","itemId":"bad","delta":15}}),
                json!({"id":6,"result":{"turn":{"id":"native-turn"}}}),
                json!({"method":"thread/settings/updated","params":{"threadId":"native-session",
                    "threadSettings":{"model":"observed-later","effort":"high"}}}),
                json!({"method":"hook/completed","params":{"threadId":"native-session","turnId":"native-turn",
                    "run":{"id":"fixture-hook-run","eventName":"preToolUse","status":"completed","sourcePath":request.checkout.join("hooks.json")}}}),
                json!({"method":"item/completed","params":{"threadId":"native-session","turnId":"native-turn",
                    "item":{"type":"commandExecution","id":"read","status":"completed","exitCode":0,
                      "commandActions":[{"type":"read","path":"test.txt"}]}}}),
                json!({"method":"item/completed","params":{"threadId":"native-session","turnId":"native-turn",
                    "item":{"type":"contextCompaction","id":"compaction"}}}),
                json!({"method":"thread/tokenUsage/updated","params":{"threadId":"native-session","turnId":"native-turn",
                    "tokenUsage":{"last":{"inputTokens":5,"outputTokens":3,"cachedInputTokens":1}}}}),
                json!({"method":"item/completed","params":{"threadId":"native-session","turnId":"native-turn",
                    "item":{"type":"agentMessage","id":"output","text":"Fixture answer"}}}),
                json!({"method":"turn/completed","params":{"threadId":"native-session",
                    "turn":{"id":"native-turn","status":"completed"}}}),
            ] {
                write
                    .send(Ok(OutputFrame {
                        stream: Stream::Stdout,
                        data: data.to_string(),
                    }))
                    .unwrap();
            }
            Ok(CapturedProcess {
                input: ProcessInput::new(self.0.clone()),
                output,
                control: Arc::new(CompletedProcess),
            })
        }
    }

    #[tokio::test]
    async fn native_attachments_submit_qualified_frozen_bytes_and_a_read_only_path() -> Result<()> {
        use crate::attachments::{
            AttachmentFormat, AttachmentPreview, AttachmentSource, NativeAttachmentEncoding,
        };
        use crate::providers::capabilities::CapabilityStatus;
        use base64::{Engine, engine::general_purpose::STANDARD};

        let (_dir, store, chat, scope) = setup().await?;
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: chat.id.clone(),
            frames: Arc::default(),
        }));
        let first = prepare(&store, &chat.id, scope.clone(), "Establish fixture profile").await?;
        let first_run = first.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner.clone(), first, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let profile = store
            .agent_dispatch(chat.id.clone(), first_run)
            .await?
            .unwrap()
            .snapshot
            .capabilities
            .unwrap();
        let subject = CapabilitySubject::NativeAttachment {
            format: AttachmentFormat::Png,
            encoding: NativeAttachmentEncoding::CodexLocalImage,
        };
        let route = profile
            .entries
            .iter()
            .find(|entry| entry.descriptor.subject == subject)
            .unwrap();
        assert_eq!(route.status(), CapabilityStatus::Unverified);
        let qualification = store
            .record_capability_evidence(
                profile.scope.clone(),
                route.descriptor.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                "simulated-local-image-qualification".into(),
            )
            .await?;
        let mut changed = profile.scope.clone();
        changed.permission_revision = "other-permissions".into();
        let resolved = store
            .resolve_capability_assessment(changed, vec![route.descriptor.clone()])
            .await?;
        assert!(resolved.entries[0].evidence.is_none());

        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ9sAAAAASUVORK5CYII=";
        let file = scope.checkout.join("picture.txt");
        std::fs::write(&file, STANDARD.decode(PNG)?)?;
        let request = AttachmentSource::Checkout {
            relative_path: "picture.txt".into(),
        };
        let preview = AttachmentPreview::read(&scope.checkout, request.clone())?;
        assert_eq!(preview.metadata().format, AttachmentFormat::Png);
        let current = store.conversation(chat.id.clone()).await?;
        std::fs::write(&file, b"saved after preview")?;
        assert!(
            store
                .prepare_agent_attempt_with_attachments(
                    chat.id.clone(),
                    current.context_id.clone(),
                    scope.clone(),
                    "Inspect the picture".into(),
                    vec![preview]
                )
                .await
                .is_err()
        );
        assert_eq!(
            store.conversation(chat.id.clone()).await?.context_id,
            current.context_id
        );

        std::fs::write(&file, STANDARD.decode(PNG)?)?;
        let preview = AttachmentPreview::read(&scope.checkout, request)?;
        let expected = preview.metadata().clone();
        let admitted = store
            .prepare_agent_attempt_with_attachments(
                chat.id.clone(),
                current.context_id,
                scope.clone(),
                "Inspect the picture".into(),
                vec![preview],
            )
            .await?;
        let run = admitted.run.id.clone();
        std::fs::write(&file, b"saved after admission")?;
        assert_eq!(
            start_managed_turn(store.clone(), runner, admitted, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let committed = store.agent_dispatch(chat.id.clone(), run).await?.unwrap();
        let attachment = &committed.snapshot.attachments[0];
        assert_eq!(attachment.metadata, expected);
        assert_eq!(attachment.qualification_id, qualification);
        assert_eq!(attachment.input.content, PNG);
        assert_eq!(
            attachment.message_revision_id,
            committed.snapshot.submitted_messages[0].revision_id
        );
        let frame: Value = serde_json::from_slice(&committed.frames[0])?;
        assert_eq!(
            frame["params"]["input"][1],
            json!({"type":"localImage","path":attachment.input.path})
        );
        assert!(!attachment.input.path.as_ref().unwrap().exists());

        let next = prepare(&store, &chat.id, scope.clone(), "Continue").await?;
        assert!(next.attachments.is_empty());
        store
            .finish_agent_attempt(next.run.id, RunStatus::Cancelled, None)
            .await?;
        let mut different = scope;
        different.account_binding = Some("another-account".into());
        let fresh = prepare(&store, &chat.id, different, "Read the retained input").await?;
        assert!(matches!(fresh.intent, SessionIntent::Start));
        assert_eq!(fresh.attachments.len(), 1);
        assert_eq!(fresh.attachments[0].1.metadata, expected);
        Ok(())
    }

    #[tokio::test]
    async fn managed_worker_reuses_only_native_suffix_and_records_selected_and_observed_models()
    -> Result<()> {
        let (_dir, store, conversation, scope) = setup().await?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.id.clone(),
            frames: log.clone(),
        }));
        let first_prompt = "$fixture Primeira mensagem ☕\nsegunda linha";
        let guide = scope.checkout.join("AGENTS.md");
        std::fs::write(&guide, "Fixture repository instructions")?;
        let fixture_skill = scope
            .checkout
            .join(".agents")
            .join("skills")
            .join("fixture");
        std::fs::create_dir_all(&fixture_skill)?;
        std::fs::write(fixture_skill.join("SKILL.md"), "fixture skill")?;
        let first = prepare(&store, &conversation.id, scope.clone(), first_prompt).await?;
        let first_session = first.agent_session_id.clone();
        let first_run = first.run.id.clone();
        let running = start_managed_turn(store.clone(), runner.clone(), first, None);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), running.wait()).await??,
            RunStatus::Succeeded
        );
        std::fs::write(&guide, "Revised fixture repository instructions")?;
        let resumed = prepare(&store, &conversation.id, scope.clone(), "Second prompt").await?;
        assert_eq!(resumed.agent_session_id, first_session);
        assert_eq!(resumed.previous_run_id.as_deref(), Some(first_run.as_str()));
        assert!(
            matches!(&resumed.intent, SessionIntent::Resume { provider_session_id } if provider_session_id == "native-session")
        );
        assert_eq!(resumed.input.len(), 1);
        assert_eq!(resumed.input[0].text, "Second prompt");
        let resumed_run = resumed.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner.clone(), resumed, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let first_dispatch = store
            .agent_dispatch(conversation.id.clone(), first_run.clone())
            .await?
            .unwrap();
        assert_eq!(first_dispatch.delivery, Delivery::Submitted);
        assert_eq!(first_dispatch.snapshot.native_history, NativeHistory::Fresh);
        assert_eq!(
            first_dispatch.snapshot.submitted_messages[0].text,
            first_prompt
        );
        let assessment = first_dispatch.snapshot.capabilities.as_ref().unwrap();
        let instructions = first_dispatch.snapshot.instructions.as_ref().unwrap();
        assert!(instructions.sources.iter().any(|source| {
            source.path == guide
                && source.revision.is_some()
                && source.selection
                    == crate::providers::configuration::InstructionSelection::Expected
        }));
        let skills = first_dispatch.snapshot.reported_skills.as_ref().unwrap();
        assert!(skills.reported);
        assert_eq!(skills.entries[0].name, "fixture");
        assert_eq!(skills.entries[1].name, "fixture");
        assert_ne!(skills.entries[0].id, skills.entries[1].id);
        let local_skill = first_dispatch
            .snapshot
            .filesystem_skills
            .iter()
            .find(|skill| skill.path == fixture_skill.join("SKILL.md"))
            .unwrap();
        assert_eq!(local_skill.id, skills.entries[0].id);
        assert_ne!(local_skill.source, skills.entries[0].source);
        let requested = first_dispatch.snapshot.requested_skill.as_ref().unwrap();
        assert_eq!(requested.name, "fixture");
        assert_eq!(requested.matching_ids.len(), 2);
        assert!(requested.matching_ids.contains(&local_skill.id));
        let first_plugins = first_dispatch.snapshot.plugin_inventory.as_ref().unwrap();
        assert_eq!(first_plugins.report.plugins[0].enabled, Some(false));
        assert!(!first_plugins.report.load_telemetry_reported);
        assert_eq!(assessment.scope.checkout.id, conversation.checkout_id);
        assert_eq!(assessment.scope.reported_model.as_deref(), Some("observed"));
        assert!(assessment.scope.configuration.provider_revision.is_some());
        let second_dispatch = store
            .agent_dispatch(conversation.id.clone(), resumed_run.clone())
            .await?
            .unwrap();
        assert!(
            matches!(second_dispatch.snapshot.native_history, NativeHistory::ProviderCursor { session_id, .. } if session_id == "native-session")
        );
        let second_plugins = second_dispatch.snapshot.plugin_inventory.as_ref().unwrap();
        assert_ne!(first_plugins.id, second_plugins.id);
        assert!(second_dispatch.snapshot.requested_skill.is_none());
        let second_instructions = second_dispatch.snapshot.instructions.as_ref().unwrap();
        assert_ne!(instructions.sources, second_instructions.sources);
        assert!(
            store
                .agent_dispatch("unrelated".into(), first_run.clone())
                .await?
                .is_none()
        );
        let observations = store
            .agent_observations(conversation.id.clone(), resumed_run, 0)
            .await?;
        assert!(observations.iter().any(|observation| {
            observation.source == EventSource::Provider
                && observation.event.location.item_id.as_deref() == Some("fixture-hook-run")
                && matches!(&observation.event.data, ProviderEventData::HookObserved { trigger, .. } if trigger == "preToolUse")
        }));
        assert!(!observations.iter().any(|observation| matches!(
            observation.event.data,
            ProviderEventData::SkillUsed { .. }
                | ProviderEventData::SessionCustomizationsReported { .. }
        )));
        assert!(observations.iter().any(|observation| {
            observation.source == EventSource::Application
                && matches!(&observation.event.data, ProviderEventData::CapabilityAssessmentUpdated { assessment }
                    if assessment.scope.reported_model.as_deref() == Some("observed-later"))
        }));
        assert!(observations.iter().any(|observation| {
            observation.source == EventSource::Provider
                && observation.event.location.item_id.as_deref() == Some("bad")
                && matches!(observation.event.data, ProviderEventData::Diagnostic { .. })
        }));
        for expected in [
            ProviderEventData::HistoryReported {
                turns: Some(1),
                item_kind: None,
            },
            ProviderEventData::ContextRead {
                path: "test.txt".into(),
                tool: "commandExecution".into(),
            },
            ProviderEventData::Compacted,
            ProviderEventData::Usage {
                input: Some(5),
                output: Some(3),
                cached_input: Some(1),
            },
        ] {
            assert!(
                observations
                    .iter()
                    .any(|observation| observation.source == EventSource::Provider
                        && observation.event.data == expected)
            );
        }
        assert!(observations.iter().any(|observation| observation.source
            == EventSource::Application
            && observation.event.data
                == ProviderEventData::DispatchSubmitted {
                    dispatch_id: second_dispatch.id.clone()
                }));
        let subject = CapabilitySubject::Protocol {
            capability: Capability::ModelInventory,
        };
        let descriptor = assessment
            .entries
            .iter()
            .find(|entry| entry.descriptor.subject == subject)
            .unwrap()
            .descriptor
            .clone();
        let evidence = store
            .record_capability_evidence(
                assessment.scope.clone(),
                descriptor,
                EvidenceOrigin::ProtocolFixture,
                ObservedResult::Succeeded,
                first_run.clone(),
            )
            .await?;
        std::fs::write(&guide, "Fixture repository instructions")?;
        let third = prepare(&store, &conversation.id, scope, "Third prompt").await?;
        let third_run = third.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner, third, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let third = store
            .agent_dispatch(conversation.id.clone(), third_run.clone())
            .await?
            .unwrap();
        let attached_evidence =
            |assessment: &crate::providers::capabilities::CapabilityAssessment| {
                assessment
                    .entries
                    .iter()
                    .find(|entry| entry.descriptor.subject == subject)
                    .and_then(|entry| entry.evidence.as_ref())
                    .map(|evidence| evidence.id.clone())
            };
        assert_eq!(
            attached_evidence(third.snapshot.capabilities.as_ref().unwrap()),
            Some(evidence)
        );
        let third_events = store
            .agent_observations(conversation.id.clone(), third_run, 0)
            .await?;
        assert!(third_events.iter().any(|entry| matches!(&entry.event.data,
            ProviderEventData::CapabilityAssessmentUpdated {assessment} if
            assessment.scope.reported_model.as_deref() == Some("observed-later") && attached_evidence(assessment).is_none())));
        let active = store.conversation(conversation.id.clone()).await?;
        store
            .replace_conversation_message(
                active.id,
                active.context_id,
                first_dispatch.snapshot.submitted_messages[0]
                    .message_id
                    .clone(),
                "revised".into(),
            )
            .await?;
        let original = store
            .agent_dispatch(conversation.id.clone(), first_run.clone())
            .await?
            .unwrap();
        assert_eq!(original.snapshot.submitted_messages[0].text, first_prompt);
        assert_eq!(original.frames, first_dispatch.frames);
        assert_eq!(
            original.snapshot.capabilities,
            first_dispatch.snapshot.capabilities
        );
        let state = store.execute(move |db| {
            let (snapshot, frames): (Vec<u8>, Vec<u8>) = db.query_row(
                "SELECT s.payload, d.frames FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id WHERE s.run_id = ?1",
                [&first_run], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert!(serde_json::from_slice::<Value>(&snapshot).is_err());
            assert!(serde_json::from_slice::<Value>(&frames).is_err());
            let values: (String, String, String) = db.query_row(
                "SELECT json_extract(s.scope, '$.model'), a.reported_model, a.provider_turn_id
                 FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id WHERE a.run_id = ?1",
                [&first_run], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            Ok(values)
        }).await?;
        assert_eq!(
            state,
            (
                "requested-model".into(),
                "observed-later".into(),
                "native-turn".into()
            )
        );
        let attached = log.lock().unwrap();
        let frames = attached
            .iter()
            .map(|bytes| serde_json::from_slice::<Value>(bytes))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let prompts = frames
            .iter()
            .filter(|frame| frame["method"] == "turn/start")
            .map(|frame| frame["params"]["input"][0]["text"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(prompts, [first_prompt, "Second prompt", "Third prompt"]);
        assert!(
            frames
                .iter()
                .any(|frame| frame["method"] == "thread/resume")
        );
        Ok(())
    }
}
