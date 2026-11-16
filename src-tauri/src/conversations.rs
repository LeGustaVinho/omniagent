use std::{
    collections::{BTreeMap, HashSet},
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
    jobs::{SubagentAllocation, SubagentAuditRoute},
    protection,
    providers::{
        ApprovalDecision, Provider, ProviderEvent, ProviderEventData, RequestId, SessionIntent,
        SessionMode, TurnOutcome,
        questions::{QuestionAction, QuestionForm, QuestionOrigin, QuestionReply, QuestionRoute},
    },
    runs::{self, RunRecord, RunStatus},
    store::{DraftKey, Store},
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

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConversationHistory {
    pub context_id: String,
    pub messages: Vec<MessageRevision>,
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
    pub(crate) question_route: QuestionRoute,
    pub(crate) answered_question: Option<QuestionOrigin>,
    pub(crate) requires_bridge: bool,
    pub(crate) native_job_recovery: Option<NativeJobRecovery>,
    pub(crate) native_goal_seed: Option<crate::jobs::NativeGoalSeed>,
    pub(crate) native_subagent_limit: Option<u32>,
    pub(crate) subagent_audit: SubagentAuditRoute,
}

pub(crate) struct NativeJobRecovery {
    pub source_run_id: String,
    pub provider_session_id: String,
    pub provider_turn_id: Option<String>,
    pub context_id: String,
    pub executable_revision: String,
    pub approved_customizations: BTreeMap<String, String>,
}

enum AttemptInput {
    Message {
        text: String,
        previews: Vec<AttachmentPreview>,
        question_route: QuestionRoute,
        consume_draft: bool,
    },
    StructuredReply {
        origin: QuestionOrigin,
        reply: QuestionReply,
    },
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
    pub async fn list_conversations(&self, checkout_id: String) -> Result<Vec<Conversation>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT id, checkout_id, title, active_context_id FROM conversations
                 WHERE checkout_id = ?1 AND background = 0 ORDER BY title COLLATE NOCASE, id",
            )?;
            let rows = statement.query_map([checkout_id], |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    checkout_id: row.get(1)?,
                    title: row.get(2)?,
                    context_id: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn conversation_draft(&self, conversation_id: String) -> Result<String> {
        self.conversation(conversation_id.clone()).await?;
        let saved = self
            .load_draft(DraftKey::Conversation(conversation_id))
            .await?;
        String::from_utf8(saved.unwrap_or_default()).context("conversation draft is not UTF-8")
    }

    pub async fn save_conversation_draft(
        &self,
        conversation_id: String,
        text: String,
    ) -> Result<()> {
        self.conversation(conversation_id.clone()).await?;
        let key = DraftKey::Conversation(conversation_id);
        if text.is_empty() {
            self.delete_draft(key).await
        } else {
            self.save_draft(key, text.into_bytes()).await
        }
    }

    pub async fn latest_conversation_run(
        &self,
        conversation_id: String,
    ) -> Result<Option<RunRecord>> {
        self.execute(move |db| {
            let latest: Option<String> = db.query_row(
                "SELECT run_id FROM agent_attempts WHERE conversation_id = ?1 AND detached_context = 0
                 ORDER BY ordinal DESC LIMIT 1",
                [conversation_id],
                |row| row.get(0),
            ).optional()?;
            latest.map(|id| runs::load_run(db, &id)).transpose()
        }).await
    }

    pub async fn pending_conversation_inputs(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Vec<ProviderEvent>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT e.payload FROM agent_events e
                 JOIN active_agent_attempts active ON active.run_id = e.run_id
                 WHERE active.conversation_id = ?1 AND e.run_id = ?2 AND (
                    EXISTS (SELECT 1 FROM agent_permissions p WHERE p.request_event_seq = e.seq AND p.delivery = 'pending')
                    OR EXISTS (SELECT 1 FROM open_agent_questions q WHERE q.request_event_seq = e.seq AND q.state = 'live')
                 ) ORDER BY e.seq",
            )?;
            let rows = statement.query_map(params![conversation_id, run_id], |row| row.get::<_, Vec<u8>>(0))?;
            rows.map(|row| Ok(serde_json::from_slice(&protection::unprotect(&row?)?)?)).collect()
        }).await
    }

    pub async fn create_conversation(
        &self,
        checkout_id: String,
        title: String,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = insert_conversation(&tx, checkout_id, title, false)?;
            tx.commit()?;
            Ok(conversation)
        })
        .await
    }

    pub async fn conversation_messages(
        &self,
        conversation_id: String,
    ) -> Result<Vec<MessageRevision>> {
        Ok(self.conversation_history(conversation_id).await?.messages)
    }

    pub async fn conversation_history(
        &self,
        conversation_id: String,
    ) -> Result<ConversationHistory> {
        self.execute(move |db| {
            let conversation = load_conversation(db, &conversation_id)?;
            let entries = context_entries(db, &conversation.id, &conversation.context_id)?;
            Ok(ConversationHistory {
                messages: load_messages(db, &conversation.id, &entries)?,
                context_id: conversation.context_id,
            })
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

    /// Creates a branch with its own messages and session lineage on the current checkout.
    pub async fn fork_conversation(
        &self,
        conversation_id: String,
        expected_context_id: String,
        through_revision: Option<String>,
        title: String,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source = check_idle_base(&tx, &conversation_id, &expected_context_id)?;
            let mut entries = context_entries(&tx, &source.id, &expected_context_id)?;
            if let Some(revision) = &through_revision {
                let index = entries.iter().position(|entry| entry == revision).context(
                    "The selected message is outside the current conversation history",
                )?;
                entries.truncate(index + 1);
            }
            let source_session: Option<String> = tx.query_row(
                "SELECT a.agent_session_id FROM agent_attempts a
                 JOIN agent_sessions s ON s.id = a.agent_session_id
                 JOIN conversation_contexts c ON c.id = a.result_context_id
                 WHERE a.conversation_id = ?1 AND c.entries = ?2 AND s.provider_session_id IS NOT NULL
                 ORDER BY a.ordinal DESC LIMIT 1",
                params![source.id, serde_json::to_string(&entries)?], |row| row.get(0),
            ).optional()?;
            let mut fork = insert_conversation(&tx, source.checkout_id, title, false)?;
            let mut copied = Vec::with_capacity(entries.len());
            let mut read = tx.prepare(
                "SELECT m.role, r.content FROM messages m JOIN message_revisions r ON r.message_id = m.id
                 WHERE m.conversation_id = ?1 AND r.id = ?2",
            )?;
            for revision in &entries {
                let (role, payload): (String, Vec<u8>) = read.query_row(
                    params![source.id, revision], |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let role = if role == "user" { MessageRole::User } else { MessageRole::Assistant };
                let copy = insert_message(&tx, &fork.id, role, payload)?;
                tx.execute(
                    "INSERT INTO message_attachments (message_revision_id, payload)
                     SELECT ?1, payload FROM message_attachments WHERE message_revision_id = ?2",
                    params![copy, revision],
                )?;
                copied.push(copy);
            }
            drop(read);
            if !copied.is_empty() {
                fork.context_id = activate_context(&tx, &fork.id, &copied)?;
            }
            tx.execute(
                "INSERT INTO conversation_forks (conversation_id, source_conversation_id,
                    source_context_id, source_revision_id, source_session_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![fork.id, source.id, expected_context_id, entries.last(), source_session],
            )?;
            tx.commit()?;
            Ok(fork)
        }).await
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
            for message in identities {
                let queued_only: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM queued_conversation_instructions i WHERE i.message_id = ?1
                        AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = i.job_id)
                        AND NOT EXISTS (
                            SELECT 1 FROM borrowed_instruction_deliveries
                            WHERE job_id = i.job_id AND (route = 'continuation' OR delivered)
                        ))",
                    [message], |row| row.get(0),
                )?;
                ensure!(!queued_only, "An unclaimed instruction is outside conversation history");
            }
            let context = switch_history(&tx, &conversation_id, &revisions)?;
            tx.commit()?;
            Ok(context)
        })
        .await
        .inspect(|_| {
            self.job_signal.send_replace(());
        })
    }

    pub async fn rewind_conversation_history(
        &self,
        conversation_id: String,
        base_id: String,
        through_revision: Option<String>,
    ) -> Result<String> {
        let context = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                check_idle_base(&tx, &conversation_id, &base_id)?;
                let mut entries = Vec::new();
                if let Some(revision) = through_revision {
                    entries = context_entries(&tx, &conversation_id, &base_id)?;
                    let index = entries.iter().position(|id| *id == revision).context(
                        "The selected message is outside the current conversation history",
                    )?;
                    entries.truncate(index + 1);
                }
                let context = switch_history(&tx, &conversation_id, &entries)?;
                tx.commit()?;
                Ok(context)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(context)
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
        self.admit_agent_attempt(
            conversation_id,
            base_context_id,
            scope,
            AttemptInput::Message {
                text,
                previews,
                question_route: QuestionRoute::Native,
                consume_draft: false,
            },
        )
        .await
    }

    pub async fn prepare_conversation_draft_attempt(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        text: String,
    ) -> Result<AttemptPlan> {
        self.admit_agent_attempt(
            conversation_id,
            base_context_id,
            scope,
            AttemptInput::Message {
                text,
                previews: Vec::new(),
                question_route: QuestionRoute::Native,
                consume_draft: true,
            },
        )
        .await
    }

    async fn admit_agent_attempt(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        input: AttemptInput,
    ) -> Result<AttemptPlan> {
        let (text, previews, question_route, answer, consume_draft) = match input {
            AttemptInput::Message {
                text,
                previews,
                question_route,
                consume_draft,
            } => (text, previews, question_route, None, consume_draft),
            AttemptInput::StructuredReply { origin, reply } => {
                ensure!(
                    reply.action == QuestionAction::Accept,
                    "continuing a structured question requires an accepted answer"
                );
                (
                    String::new(),
                    Vec::new(),
                    QuestionRoute::StructuredTurn,
                    Some((origin, reply)),
                    false,
                )
            }
        };
        ensure!(
            question_route.is_native() || scope.mode == SessionMode::ReadOnly,
            "structured planning needs a read-only profile"
        );
        ensure!(
            answer.is_some() || !text.trim().is_empty() || !previews.is_empty(),
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
        let encrypted = answer
            .is_none()
            .then(|| protection::protect(text.as_bytes()))
            .transpose()?;
        let frozen = if previews.is_empty() {
            None
        } else {
            let conversation = self.conversation(conversation_id.clone()).await?;
            let layout = crate::repository::verify_checkout(
                self,
                conversation.checkout_id.clone(),
                checkout.clone(),
            )
            .await?;
            let limits = self
                .checkout_attachment_limits(conversation.checkout_id)
                .await?;
            let root = layout.root;
            Some(
                tokio::task::spawn_blocking(move || -> Result<_> {
                    let captured = attachments::capture(&root, previews, limits)?;
                    let payload = protection::protect(&serde_json::to_vec(&captured)?)?;
                    Ok((root, payload))
                })
                .await??,
            )
        };
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source = answer.as_ref().map(|(origin, _)| origin.run_id.as_str());
            let allocation =
                crate::jobs::direct_subagent_allocation(&tx, &conversation_id, source, &scope)?;
            if consume_draft {
                consume_matching_draft(&tx, &conversation_id, &text)?;
            }
            let plan = insert_attempt(
                &tx,
                PreparedAttempt {
                    conversation_id,
                    base_context_id,
                    scope,
                    encrypted,
                    frozen,
                    question_route,
                    answer,
                    history: AttemptHistory::Automatic,
                    recorded: None,
                    instruction_revision: None,
                    allocation,
                },
            )?;
            tx.commit()?;
            Ok(plan)
        })
        .await
    }

    pub async fn prepare_structured_planning_attempt(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        text: String,
        previews: Vec<AttachmentPreview>,
    ) -> Result<AttemptPlan> {
        self.admit_agent_attempt(
            conversation_id,
            base_context_id,
            scope,
            AttemptInput::Message {
                text,
                previews,
                question_route: QuestionRoute::StructuredTurn,
                consume_draft: false,
            },
        )
        .await
    }

    pub async fn prepare_structured_planning_reply(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        origin: QuestionOrigin,
        reply: QuestionReply,
    ) -> Result<AttemptPlan> {
        self.admit_agent_attempt(
            conversation_id,
            base_context_id,
            scope,
            AttemptInput::StructuredReply { origin, reply },
        )
        .await
    }

    pub async fn dismiss_structured_planning_question(
        &self,
        conversation_id: String,
        base_context_id: String,
        scope: SessionScope,
        origin: QuestionOrigin,
        action: QuestionAction,
    ) -> Result<()> {
        ensure!(
            action != QuestionAction::Accept,
            "an accepted answer needs a continuation"
        );
        let scope = serde_json::to_string(&scope)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = check_idle_base(&tx, &conversation_id, &base_context_id)?;
            let reply = QuestionReply {
                action,
                values: Default::default(),
            };
            let (seq, _) = validate_structured_reply(&tx, &conversation, &scope, &origin, &reply)?;
            let event = ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::StructuredQuestionDismissed {
                    origin: origin.clone(),
                    action,
                },
            };
            save_question_event(&tx, &origin.run_id, event)?;
            close_saved_question(&tx, seq)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn record_agent_observation(
        &self,
        run_id: String,
        source: EventSource,
        event: ProviderEvent,
    ) -> Result<()> {
        let wakes_notices = matches!(
            event.data,
            ProviderEventData::Approval { .. }
                | ProviderEventData::Question { .. }
                | ProviderEventData::QuestionClosed { .. }
                | ProviderEventData::QuestionReplySettled { .. }
                | ProviderEventData::SteeringSettled { .. }
        );
        let payload = protection::protect(&serde_json::to_vec(&event)?)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active = match active_attempt(&tx, &run_id) {
                Ok(active) => active,
                Err(error) if matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::QueryReturnedNoRows))
                    && crate::jobs::late_native_goal_evidence(&tx, &run_id, source, &event)? =>
                {
                    tx.execute(
                        "INSERT INTO agent_events(run_id, source, payload) VALUES (?1, ?2, ?3)",
                        params![run_id, source.as_sql(), payload],
                    )?;
                    tx.commit()?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let primary = source == EventSource::Provider && event.location.parent_item_id.is_none()
                && !matches!(event.data, ProviderEventData::Diagnostic { .. });
            let native_goal = primary && crate::jobs::is_native_goal_run(&tx, &run_id)?;
            if primary && let Some(id) = &event.location.session_id {
                let old: Option<String> = tx.query_row(
                    "SELECT provider_session_id FROM agent_sessions WHERE id = ?1", [&active.session_id], |row| row.get(0),
                )?;
                ensure!(old.as_ref().is_none_or(|old| old == id), "provider session changed during the attempt");
                tx.execute("UPDATE agent_sessions SET provider_session_id = ?1 WHERE id = ?2", params![id, active.session_id])?;
            }
            if primary && !native_goal && let Some(id) = &event.location.turn_id {
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
                ProviderEventData::TurnFinished { outcome } if primary && !native_goal => {
                    let state = match outcome {
                        TurnOutcome::Completed => "completed",
                        TurnOutcome::Failed => "failed",
                        TurnOutcome::Interrupted => "interrupted",
                    };
                    tx.execute("UPDATE agent_attempts SET provider_terminal = ?1 WHERE run_id = ?2", params![state, run_id])?;
                }
                _ => {}
            }
            let diagnostic = event.location.parent_item_id.is_none()
                && matches!(&event.data, ProviderEventData::Diagnostic { .. } | ProviderEventData::Error { .. });
            tx.execute("INSERT INTO agent_events(run_id, source, payload, inspect_diagnostic) VALUES (?1, ?2, ?3, ?4)",
                params![run_id, source.as_sql(), payload, diagnostic])?;
            let event_seq = tx.last_insert_rowid();
            if native_goal {
                crate::jobs::record_native_goal_turn(&tx, &run_id, event_seq, &event)?;
            }
            crate::jobs::record_native_goal_control(&tx, &run_id, event_seq, source, &event)?;
            crate::jobs::record_steering_event(&tx, &active.conversation_id, &run_id, source, event_seq, &event.data)?;
            if source == EventSource::Provider {
                crate::jobs::record_native_subagent(&tx, &run_id, event_seq, &event.data)?;
            }
            if primary && let ProviderEventData::GoalObserved { observation } = &event.data {
                crate::jobs::record_native_goal(&tx, &run_id, event_seq, observation)?;
            }
            if source == EventSource::Application && let ProviderEventData::CapabilityAssessmentUpdated { assessment } = &event.data {
                crate::runtime_inspection::save_capability_summary(&tx, &run_id, assessment)?;
            }
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
                    params![event_seq, run_id, active.turn_id, permission_key(request_id)?, route.as_sql()])?;
            }
            match &event.data {
                ProviderEventData::Question { request_id, normalized: Some(_), route, .. }
                    if (primary && route.is_native()) || (source == EventSource::Application
                        && event.location.parent_item_id.is_none() && !route.is_native()) =>
                {
                    let request_event_seq = event_seq;
                    let inserted = tx.execute(
                        "INSERT INTO agent_questions(request_event_seq, run_id, turn_id, request_key, route, delivery)
                         SELECT ?1, ?2, ?3, ?4, ?5, 'pending'
                         WHERE NOT EXISTS (SELECT 1 FROM agent_questions WHERE run_id = ?2 AND request_key = ?4)",
                        params![request_event_seq, run_id, active.turn_id, permission_key(request_id)?,
                            if route.is_native() { "native" } else { "structured_turn" }],
                    )?;
                    ensure!(inserted == 1, "The provider reused a question identity in this turn");
                    if route.is_native() {
                        crate::jobs::open_native_question(&tx, &run_id, request_event_seq)?;
                    }
                }
                ProviderEventData::QuestionReplyPrepared { request_id, reply, .. } if source == EventSource::Application => {
                    let key = permission_key(request_id)?;
                    let open: Vec<u8> = tx.query_row(
                        "SELECT e.payload FROM agent_questions q JOIN agent_events e ON e.seq = q.request_event_seq
                         JOIN native_question_waits w ON w.request_event_seq = q.request_event_seq
                         WHERE q.run_id = ?1 AND q.turn_id = ?2 AND q.request_key = ?3 AND q.route = 'native'
                           AND q.delivery = 'pending' AND w.state = 'live'",
                        params![run_id, active.turn_id, key],
                        |row| row.get(0),
                    ).context("question request is absent or stale for this turn")?;
                    let question: ProviderEvent = serde_json::from_slice(&protection::unprotect(&open)?)?;
                    let ProviderEventData::Question { normalized: Some(form), .. } = question.data else {
                        anyhow::bail!("saved native question has no validated form");
                    };
                    form.validate(reply)?;
                    tx.execute(
                        "UPDATE agent_questions SET delivery = 'prepared', reply_event_seq = ?4
                         WHERE run_id = ?1 AND turn_id = ?2 AND request_key = ?3 AND route = 'native' AND delivery = 'pending'",
                        params![run_id, active.turn_id, key, event_seq],
                    )?;
                }
                ProviderEventData::QuestionReplySettled { request_id, delivery, .. } if source == EventSource::Application => {
                    ensure!(*delivery != Delivery::Prepared, "question reply must have a delivery result");
                    let changed = tx.execute(
                        "UPDATE agent_questions SET delivery = ?4
                         WHERE run_id = ?1 AND turn_id = ?2 AND request_key = ?3 AND route = 'native' AND delivery = 'prepared'",
                        params![run_id, active.turn_id, permission_key(request_id)?, delivery.as_sql()],
                    )?;
                    ensure!(changed == 1, "question reply is no longer prepared");
                    crate::jobs::close_live_question(&tx, &run_id, &permission_key(request_id)?)?;
                }
                ProviderEventData::QuestionClosed { request_id } if primary => {
                    crate::jobs::stage_provider_expiry(&tx, &run_id, &permission_key(request_id)?)?;
                }
                _ => {}
            }
            tx.commit()?;
            Ok(())
        }).await?;
        if wakes_notices {
            self.job_signal.send_replace(());
        }
        Ok(())
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
            crate::jobs::record_process_launch(&tx, &run_id)?;
            runs::advance_run(&tx, &run, RunStatus::Running)?;
            crate::jobs::refresh_live_question_status(&tx, &run_id)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn record_agent_launch_intent(&self, run_id: String) -> Result<bool> {
        let accepted = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                active_attempt(&tx, &run_id)?;
                let allowed = crate::jobs::record_launch_intent(&tx, &run_id)?;
                tx.commit()?;
                Ok(allowed)
            })
            .await?;
        accepted.context("Job authorization changed before process launch")
    }

    pub async fn finish_agent_attempt(
        &self,
        run_id: String,
        outcome: RunStatus,
        response: Option<String>,
    ) -> Result<()> {
        self.finish_observed_agent_attempt(run_id, outcome, response, None, false)
            .await
            .map(|_| ())
    }

    pub(crate) async fn finish_observed_agent_attempt(
        &self,
        run_id: String,
        outcome: RunStatus,
        response: Option<String>,
        failure: Option<crate::jobs::FailureObservation>,
        quiescence_confirmed: bool,
    ) -> Result<RunStatus> {
        ensure!(outcome.is_terminal(), "attempt outcome must be terminal");
        let has_response = response
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty());
        let encrypted = response
            .filter(|text| !text.is_empty())
            .map(|text| protection::protect(text.as_bytes()))
            .transpose()?;
        let outcome = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let active = active_attempt(&tx, &run_id)?;
            let run = runs::load_run(&tx, &run_id)?;
            let incomplete = crate::jobs::settle_subagent_audit(&tx, &run_id, has_response)?;
            let (outcome, failure) = if outcome == RunStatus::Succeeded
                && (crate::jobs::pause_revoked_goal(&tx, &run_id)?
                    || crate::jobs::delegated_goal_authority_revoked(&tx, &run_id)?)
            {
                (RunStatus::Interrupted, Some(crate::jobs::FailureObservation {
                    phase: crate::jobs::FailurePhase::Execution,
                    cause: Some(crate::jobs::FailureCause::GoalAuthorityRevoked),
                    process: failure.and_then(|observed| observed.process),
                }))
            } else if incomplete && outcome == RunStatus::Succeeded {
                (RunStatus::Failed, Some(crate::jobs::FailureObservation {
                    phase: crate::jobs::FailurePhase::EvidenceCapture,
                    cause: Some(crate::jobs::FailureCause::EvidenceUnavailable),
                    process: failure.and_then(|observed| observed.process),
                }))
            } else {
                (outcome, failure)
            };
            runs::advance_run(&tx, &run, outcome)?;
            crate::jobs::record_job_failure(&tx, &run_id, outcome, failure)?;
            let context_id = seal_attempt_context(&tx, &run_id, &active, encrypted)?;
            tx.execute("UPDATE agent_attempts SET result_context_id = ?1 WHERE run_id = ?2", params![context_id, run_id])?;
            crate::jobs::release_settled_writer(&tx, &run_id)?;
            let saved_input = crate::jobs::settle_question_waits(&tx, &run_id, &context_id, quiescence_confirmed)?;
            crate::jobs::settle_native_subagents(&tx, &run_id, quiescence_confirmed)?;
            crate::jobs::settle_native_goal_executor(&tx, &run_id, quiescence_confirmed)?;
            let suspended = crate::jobs::settle_parent_wait(
                &tx, &run_id, &context_id, outcome, quiescence_confirmed,
            )?;
            crate::jobs::settle_goal_stop(&tx, &run_id, quiescence_confirmed)?;
            if outcome == RunStatus::Succeeded && !suspended && !saved_input {
                if quiescence_confirmed {
                    crate::jobs::accept_goal_checkpoint(&tx, &run_id, &context_id)?;
                }
                tx.execute("UPDATE agent_sessions SET cursor_context_id = ?1 WHERE id = ?2 AND provider_session_id IS NOT NULL
                    AND EXISTS(SELECT 1 FROM agent_attempts a
                        JOIN context_snapshots s ON s.run_id = a.run_id
                        JOIN dispatches d ON d.snapshot_id = s.id
                        WHERE a.run_id = ?3 AND a.provider_terminal = 'completed' AND d.delivery = 'submitted')
                    AND NOT EXISTS (SELECT 1 FROM borrowed_instruction_deliveries WHERE run_id = ?3 AND uncertain)
                    AND NOT EXISTS (SELECT 1 FROM native_goal_executors WHERE run_id = ?3)",
                    params![context_id, active.session_id, run_id])?;
                tx.execute(
                    "INSERT INTO structured_question_continuations(request_event_seq, context_id)
                     SELECT request_event_seq, ?1 FROM agent_questions
                     WHERE run_id = ?2 AND route = 'structured_turn' AND delivery = 'pending'",
                    params![context_id, run_id],
                )?;
            } else {
                close_unfinished_structured_questions(&tx, &run_id)?;
            }
            close_permissions(&tx, &run_id)?;
            close_native_questions(&tx, &run_id)?;
            tx.execute("DELETE FROM active_agent_attempts WHERE run_id = ?1", [&run_id])?;
            if outcome == RunStatus::Interrupted && !suspended && !saved_input {
                crate::jobs::requeue_unlaunched_job(&tx, &run_id)?;
                if quiescence_confirmed {
                    crate::jobs::requeue_rejected_native_goal(&tx, &run_id)?;
                }
            }
            crate::jobs::observe_child_run(&tx, &run_id)?;
            crate::jobs::settle_owned_goal_cancellations(&tx)?;
            tx.commit()?;
            Ok(outcome)
        }).await?;
        self.job_signal.send_replace(());
        Ok(outcome)
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
                let attempt = active_attempt(&tx, &id)?;
                let run = runs::load_run(&tx, &id)?;
                runs::advance_run(&tx, &run, RunStatus::Interrupted)?;
                crate::jobs::record_job_failure(
                    &tx,
                    &id,
                    RunStatus::Interrupted,
                    Some(crate::jobs::FailureObservation {
                        phase: crate::jobs::FailurePhase::Restart,
                        cause: Some(crate::jobs::FailureCause::RuntimeRestart),
                        process: None,
                    }),
                )?;
                close_permissions(&tx, &id)?;
                close_unfinished_structured_questions(&tx, &id)?;
                crate::jobs::settle_subagent_audit(&tx, &id, false)?;
                let output = crate::jobs::recover_native_goal_output(&tx, &id)?;
                let context = seal_attempt_context(&tx, &id, &attempt, output)?;
                tx.execute(
                    "UPDATE agent_attempts SET result_context_id = ?1 WHERE run_id = ?2",
                    params![context, id],
                )?;
                crate::jobs::release_settled_writer(&tx, &id)?;
                crate::jobs::restart_question_waits(&tx, &id, &context)?;
                close_native_questions(&tx, &id)?;
                if !crate::jobs::recover_waiting_parent(&tx, &id)? {
                    crate::jobs::requeue_unlaunched_job(&tx, &id)?;
                }
                crate::jobs::settle_goal_stop(&tx, &id, false)?;
                crate::jobs::observe_child_run(&tx, &id)?;
            }
            tx.execute(
                "UPDATE dispatches SET delivery = 'unconfirmed' WHERE delivery = 'prepared'",
                [],
            )?;
            tx.execute("DELETE FROM active_agent_attempts", [])?;
            crate::jobs::recover_native_goal_executors(&tx)?;
            crate::jobs::recover_question_stop_reservations(&tx)?;
            crate::jobs::recover_native_subagents(&tx)?;
            crate::jobs::recover_managed_goal_stops(&tx)?;
            crate::jobs::recover_commands(&tx)?;
            crate::jobs::recover_side_requests(&tx)?;
            crate::jobs::settle_owned_goal_cancellations(&tx)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }
}

fn seal_attempt_context(
    db: &Connection,
    run: &str,
    active: &ActiveAttempt,
    response: Option<Vec<u8>>,
) -> Result<String> {
    crate::jobs::settle_steering_at_stop(db, run)?;
    let steering = crate::jobs::accepted_steering_revisions(db, run)?;
    if response.is_none() && steering.is_empty() {
        return Ok(active.context_id.clone());
    }
    let mut entries = context_entries(db, &active.conversation_id, &active.context_id)?;
    entries.extend(steering);
    if let Some(response) = response {
        entries.push(insert_message(
            db,
            &active.conversation_id,
            MessageRole::Assistant,
            response,
        )?);
    }
    write_context(
        db,
        &active.conversation_id,
        &entries,
        !active.detached_context,
    )
}

struct PreparedAttempt {
    conversation_id: String,
    base_context_id: String,
    scope: SessionScope,
    encrypted: Option<Vec<u8>>,
    frozen: Option<(PathBuf, Vec<u8>)>,
    question_route: QuestionRoute,
    answer: Option<(QuestionOrigin, QuestionReply)>,
    history: AttemptHistory,
    recorded: Option<RecordedAttempt>,
    instruction_revision: Option<String>,
    allocation: SubagentAllocation,
}

pub(crate) struct RecordedAttempt {
    pub context_id: String,
    pub previous_run_id: Option<String>,
    pub question_route: QuestionRoute,
}

pub(crate) enum AttemptHistory {
    Automatic,
    Fresh,
    QueuedInstruction { revision_id: String },
    Retry { input_context_id: String },
    Resume { native: Option<NativeJobRecovery> },
    Question { origin_run_id: String },
    Downstream,
}

pub(crate) fn consume_matching_draft(
    db: &Connection,
    conversation_id: &str,
    text: &str,
) -> Result<()> {
    let draft: Vec<u8> = db.query_row(
        "SELECT payload FROM drafts WHERE kind = 'conversation' AND owner_id = ?1 AND document = ''",
        [conversation_id], |row| row.get(0),
    ).optional()?.context("The conversation draft no longer exists")?;
    ensure!(
        protection::unprotect(&draft)? == text.as_bytes(),
        "The conversation draft changed before sending"
    );
    db.execute(
        "DELETE FROM drafts WHERE kind = 'conversation' AND owner_id = ?1 AND document = ''",
        [conversation_id],
    )?;
    Ok(())
}

pub(crate) fn insert_job_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    text: &str,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    insert_job_with_history(
        db,
        conversation,
        scope,
        text,
        allocation,
        AttemptHistory::Automatic,
    )
}

pub(crate) fn insert_job_with_history(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    text: &str,
    allocation: SubagentAllocation,
    history: AttemptHistory,
) -> Result<AttemptPlan> {
    insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted: Some(protection::protect(text.as_bytes())?),
            frozen: None,
            question_route: QuestionRoute::Native,
            answer: None,
            history,
            recorded: None,
            instruction_revision: None,
            allocation,
        },
    )
}

pub(crate) fn insert_instruction_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    revision_id: String,
    recorded: Option<RecordedAttempt>,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted: None,
            frozen: None,
            question_route: QuestionRoute::Native,
            answer: None,
            history: AttemptHistory::QueuedInstruction { revision_id },
            recorded,
            instruction_revision: None,
            allocation,
        },
    )
}

pub(crate) fn insert_job_recovery_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    history: AttemptHistory,
    recorded: Option<RecordedAttempt>,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    let question_route = recorded
        .as_ref()
        .map_or(QuestionRoute::Native, |r| r.question_route);
    let encrypted = if matches!(history, AttemptHistory::Resume { .. }) {
        Some(protection::protect(b"Continue the interrupted task from the recorded result. Check the work already completed before taking further action, and finish the remaining work.")?)
    } else {
        None
    };
    insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted,
            frozen: None,
            question_route,
            answer: None,
            history,
            recorded,
            instruction_revision: None,
            allocation,
        },
    )
}

pub(crate) fn insert_downstream_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    recorded: Option<RecordedAttempt>,
    input: &str,
    instruction_revision: Option<String>,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted: Some(protection::protect(input.as_bytes())?),
            frozen: None,
            question_route: recorded
                .as_ref()
                .map_or(QuestionRoute::Native, |recorded| recorded.question_route),
            answer: None,
            history: AttemptHistory::Downstream,
            recorded,
            instruction_revision,
            allocation,
        },
    )
}

pub(crate) fn insert_recorded_job_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    recorded: RecordedAttempt,
    text: &str,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted: Some(protection::protect(text.as_bytes())?),
            frozen: None,
            question_route: recorded.question_route,
            answer: None,
            history: AttemptHistory::Downstream,
            recorded: Some(recorded),
            instruction_revision: None,
            allocation,
        },
    )
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordedQuestionAnswer {
    pub origin: QuestionOrigin,
    pub form: QuestionForm,
    pub schema: serde_json::Value,
    pub reply: QuestionReply,
}

pub(crate) struct LinkedQuestionInput {
    pub context: String,
    pub route: QuestionRoute,
    pub answers: Vec<(i64, RecordedQuestionAnswer)>,
    pub earlier_replies: Vec<RecordedQuestionAnswer>,
    pub downstream: Option<String>,
    pub instruction_revision: Option<String>,
}

pub(crate) fn insert_linked_question_job_attempt(
    db: &Connection,
    conversation: Conversation,
    scope: SessionScope,
    input: LinkedQuestionInput,
    allocation: SubagentAllocation,
) -> Result<AttemptPlan> {
    let LinkedQuestionInput {
        context,
        route,
        answers,
        earlier_replies,
        downstream,
        instruction_revision,
    } = input;
    let (_, first) = answers.first().context("Missing recorded answers")?;
    let origin = first.origin.clone();
    let detached = context != conversation.context_id;
    let recorded = detached.then(|| RecordedAttempt {
        context_id: context,
        previous_run_id: Some(origin.run_id.clone()),
        question_route: route,
    });
    let sets = serde_json::json!({
        "earlierReplies": earlier_replies,
        "answers": answers.iter().map(|(_, answer)| answer).collect::<Vec<_>>(),
    });
    let mut prompt = format!(
        "The user answered questions from the original turn. Continue the original task from the saved context using these answers.\n{}",
        serde_json::to_string(&sets)?
    );
    if let Some(downstream) = downstream {
        prompt.push_str("\n\n");
        prompt.push_str(&downstream);
    }
    let mut plan = insert_attempt(
        db,
        PreparedAttempt {
            conversation_id: conversation.id,
            base_context_id: conversation.context_id,
            scope,
            encrypted: Some(protection::protect(prompt.as_bytes())?),
            frozen: None,
            question_route: route,
            answer: None,
            history: AttemptHistory::Question {
                origin_run_id: origin.run_id.clone(),
            },
            recorded,
            instruction_revision,
            allocation,
        },
    )?;
    for (seq, answer) in answers {
        link_question_reply(db, &plan.run.id, seq, &answer.origin, answer.reply)?;
    }
    plan.answered_question = Some(origin);
    Ok(plan)
}

pub(crate) fn attempt_output(
    db: &Connection,
    run: &str,
    context: &str,
) -> Result<Option<MessageRevision>> {
    let origin: Option<(String, String)> = db.query_row(
        "SELECT conversation_id, context_id FROM agent_attempts WHERE run_id = ?1 AND result_context_id = ?2",
        params![run, context], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    let Some((conversation, input_context)) = origin else {
        return Ok(None);
    };
    if input_context == context {
        return Ok(None);
    }
    let input = context_entries(db, &conversation, &input_context)?;
    let result = context_entries(db, &conversation, context)?;
    let Some(output) = result.strip_prefix(input.as_slice()) else {
        return Ok(None);
    };
    Ok(load_messages(db, &conversation, output)?
        .into_iter()
        .find(|message| message.role == MessageRole::Assistant))
}

fn insert_attempt(db: &Connection, prepared: PreparedAttempt) -> Result<AttemptPlan> {
    let PreparedAttempt {
        conversation_id,
        base_context_id,
        scope,
        encrypted,
        frozen,
        question_route,
        answer,
        history,
        mut recorded,
        instruction_revision,
        allocation,
    } = prepared;
    let serialized = serde_json::to_string(&scope)?;
    let checkout = &scope.checkout;
    let conversation = check_idle_base(db, &conversation_id, &base_context_id)?;
    let answered_request = answer
        .as_ref()
        .map(|(origin, reply)| {
            validate_structured_reply(db, &conversation, &serialized, origin, reply)
        })
        .transpose()?;
    if let (Some((origin, _)), Some((seq, _))) = (&answer, &answered_request) {
        if let Some(context_id) =
            crate::jobs::detached_question_context(db, &conversation_id, &origin.run_id, *seq)?
        {
            recorded = Some(RecordedAttempt {
                context_id,
                previous_run_id: Some(origin.run_id.clone()),
                question_route: QuestionRoute::StructuredTurn,
            });
        }
    }
    let registered: String = db.query_row(
        "SELECT root FROM checkouts WHERE id = ?1",
        [&conversation.checkout_id],
        |row| row.get(0),
    )?;
    ensure!(
        checkout.starts_with(Path::new(&registered)),
        "working directory is outside the conversation checkout"
    );
    if let Some((root, _)) = &frozen {
        ensure!(
            root == Path::new(&registered),
            "attachment checkout changed after admission"
        );
    }
    let input_context_id = match &history {
        AttemptHistory::Retry { input_context_id } => input_context_id,
        _ => recorded
            .as_ref()
            .map_or(&base_context_id, |recorded| &recorded.context_id),
    };
    let mut entries = context_entries(db, &conversation_id, input_context_id)?;
    let previous = previous_attempt(db, &conversation_id)?;
    let eligible = previous.as_ref().filter(|prior| {
        !prior.native_scope_reserved && prior.native_subagent_limit == allocation.native_allowance
    });
    let eligible = eligible.filter(|prior| allocation.audit == prior.audit);
    let continuity = match (&history, eligible, &recorded) {
        (
            AttemptHistory::Automatic | AttemptHistory::QueuedInstruction { .. },
            Some(prior),
            None,
        ) if prior.scope == serialized
            && scope
                .account_binding
                .as_ref()
                .is_some_and(|id| !id.is_empty()) =>
        {
            append_only_cursor(db, &conversation_id, &entries, prior)?
        }
        (
            AttemptHistory::Resume {
                native: Some(recovery),
            },
            Some(prior),
            None,
        ) => {
            ensure!(
                prior.run_id == recovery.source_run_id
                    && prior.provider_session_id.as_ref() == Some(&recovery.provider_session_id),
                "The provider session changed before recovery"
            );
            let checkpoint = context_entries(db, &conversation_id, &recovery.context_id)?;
            ensure!(
                entries.starts_with(&checkpoint),
                "The saved input is outside the recovery context"
            );
            Some((prior, recovery.context_id.clone(), checkpoint.len()))
        }
        _ => None,
    };

    let (agent_session_id, resume_cursor_id, intent, consumed) = if let Some((
        prior,
        cursor,
        consumed,
    )) = continuity
    {
        let provider_session_id = prior
            .provider_session_id
            .clone()
            .expect("checked native ID");
        (
            prior.session_id.clone(),
            Some(cursor),
            SessionIntent::Resume {
                provider_session_id,
            },
            consumed,
        )
    } else {
        let id = new_id();
        db.execute(
                "INSERT INTO agent_sessions(id, conversation_id, scope, native_subagent_limit) VALUES (?1, ?2, ?3, ?4)",
                params![id, conversation_id, serialized, allocation.native_allowance],
            )?;
        (id, None, SessionIntent::Start, 0)
    };

    let context_id = if let AttemptHistory::Retry { input_context_id } = &history {
        if recorded.is_none() {
            db.execute(
                "UPDATE conversations SET active_context_id = ?1 WHERE id = ?2",
                params![input_context_id, conversation_id],
            )?;
        }
        input_context_id.clone()
    } else if let AttemptHistory::QueuedInstruction { revision_id } = &history {
        entries.push(revision_id.clone());
        write_context(db, &conversation_id, &entries, recorded.is_none())?
    } else {
        let encrypted = if let (Some((_, reply)), Some((_, form))) = (&answer, &answered_request) {
            let values = serde_json::json!({"question": form, "answer": reply.values});
            let text = format!(
                "The user answered this saved planning question. Continue the original planning task with the supplied values.\n{}",
                serde_json::to_string(&values)?
            );
            protection::protect(text.as_bytes())?
        } else {
            encrypted.context("missing admitted message content")?
        };
        let revision = insert_message(db, &conversation_id, MessageRole::User, encrypted)?;
        if let Some((_, payload)) = frozen {
            db.execute(
                "INSERT INTO message_attachments(message_revision_id, payload) VALUES (?1, ?2)",
                params![revision, payload],
            )?;
        }
        entries.push(revision);
        if let Some(instruction) = instruction_revision {
            ensure!(
                !entries.contains(&instruction),
                "The claimed instruction is already in the continuation context"
            );
            entries.push(instruction);
        }
        write_context(db, &conversation_id, &entries, recorded.is_none())?
    };
    let input = load_messages(db, &conversation_id, &entries[consumed..])?;
    let mut attachments = Vec::new();
    for message in &input {
        attachments.extend(
            attachments::captured_for_revision(db, &message.revision_id)?
                .into_iter()
                .map(|item| (message.revision_id.clone(), item)),
        );
    }
    // The queued revision has passed admission. The runtime checks the entire
    // assembled prompt against current limits before recording its dispatch.
    if !matches!(history, AttemptHistory::QueuedInstruction { .. }) && !attachments.is_empty() {
        let limits = attachments::limits_for_checkout(db, &conversation.checkout_id)?;
        attachments::check_limits(
            limits,
            attachments
                .iter()
                .map(|(_, item)| (&item.metadata, item.delivery())),
        )?;
    }
    let (run, _) = runs::insert_run(db, conversation.checkout_id, "agent")?;
    let ordinal: i64 = db.query_row(
        "SELECT coalesce(max(ordinal), 0) + 1 FROM agent_attempts WHERE conversation_id = ?1",
        [&conversation_id],
        |row| row.get(0),
    )?;
    let previous_run_id = match (&recorded, &history) {
        (_, AttemptHistory::Question { origin_run_id }) => Some(origin_run_id.clone()),
        (Some(recorded), _) => recorded.previous_run_id.clone(),
        (None, _) => previous.map(|prior| prior.run_id),
    };
    db.execute(
        "INSERT INTO agent_attempts(run_id, turn_id, conversation_id, ordinal, agent_session_id,
         previous_run_id, context_id, resume_cursor_id, detached_context, subagent_conversation_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            run.id,
            run.turn_id,
            conversation_id,
            ordinal,
            agent_session_id,
            previous_run_id,
            context_id,
            resume_cursor_id,
            recorded.is_some(),
            allocation
                .conversation_id
                .as_deref()
                .unwrap_or(&conversation_id),
        ],
    )?;
    db.execute(
        "INSERT INTO active_agent_attempts VALUES (?1, ?2)",
        params![conversation_id, run.id],
    )?;
    if allocation.native_allowance.is_some() {
        crate::jobs::reserve_native_subagents(
            db,
            &run.id,
            &agent_session_id,
            allocation.native_requested,
            allocation.strict_total,
        )?;
    }
    crate::jobs::pin_attempt_audit(db, &run.id, &allocation.audit)?;
    let mut answered_question = None;
    if let (Some((origin, reply)), Some((request_event_seq, _))) = (answer, answered_request) {
        link_question_reply(db, &run.id, request_event_seq, &origin, reply)?;
        answered_question = Some(origin);
    }
    let native_job_recovery = match history {
        AttemptHistory::Resume { native } if resume_cursor_id.is_some() => native,
        _ => None,
    };
    Ok(AttemptPlan {
        run,
        conversation_id,
        agent_session_id,
        previous_run_id,
        context_id,
        resume_cursor_id,
        intent,
        scope,
        input,
        attachments,
        question_route,
        answered_question,
        requires_bridge: false,
        native_job_recovery,
        native_goal_seed: None,
        native_subagent_limit: allocation.native_allowance,
        subagent_audit: allocation.audit,
    })
}

fn link_question_reply(
    db: &Connection,
    continuation: &str,
    seq: i64,
    origin: &QuestionOrigin,
    reply: QuestionReply,
) -> Result<()> {
    let event = ProviderEvent {
        location: Default::default(),
        data: ProviderEventData::StructuredQuestionReplyLinked {
            origin: origin.clone(),
            reply,
            continuation_run_id: continuation.into(),
        },
    };
    let event_seq = save_question_event(db, &origin.run_id, event)?;
    let changed = db.execute(
        "UPDATE structured_question_continuations SET continuation_run_id = ?1, reply_event_seq = ?2
         WHERE request_event_seq = ?3 AND continuation_run_id IS NULL",
        params![continuation, event_seq, seq],
    )?;
    ensure!(changed == 1, "This question already has a continuation");
    close_saved_question(db, seq)
}

pub(crate) fn save_question_event(db: &Connection, run: &str, event: ProviderEvent) -> Result<i64> {
    let payload = protection::protect(&serde_json::to_vec(&event)?)?;
    db.execute(
        "INSERT INTO agent_events(run_id, source, payload) VALUES (?1, ?2, ?3)",
        params![run, EventSource::Application.as_sql(), payload],
    )?;
    Ok(db.last_insert_rowid())
}

pub(crate) fn close_saved_question(db: &Connection, seq: i64) -> Result<()> {
    let changed = db.execute(
        "UPDATE agent_questions SET delivery = 'closed'
         WHERE request_event_seq = ?1 AND (delivery = 'pending' OR EXISTS (
            SELECT 1 FROM native_question_waits w WHERE w.request_event_seq = ?1 AND w.state = 'saved'
         ))",
        [seq],
    )?;
    ensure!(changed == 1, "This saved question is no longer pending");
    crate::jobs::archive_saved_question(db, seq)
}

pub(crate) fn recovery_checkpoint(
    db: &Connection,
    conversation_id: &str,
    source_run: &str,
    input_context_id: &str,
    sealed_result: Option<&str>,
) -> Result<Option<String>> {
    let current = load_conversation(db, conversation_id)?.context_id;
    if previous_attempt(db, conversation_id)?.is_none_or(|prior| prior.run_id != source_run) {
        return Ok(None);
    }
    if let Some(result) = sealed_result {
        return Ok((current == result).then_some(current));
    }
    // Legacy attempts did not store the output context. Recognize only the old
    // settlement shape: the exact sent context followed by one assistant revision.
    let sent = context_entries(db, conversation_id, input_context_id)?;
    let entries = context_entries(db, conversation_id, &current)?;
    if entries == sent {
        return Ok(Some(current));
    }
    if !entries.starts_with(&sent) || entries.len() != sent.len() + 1 {
        return Ok(None);
    }
    let tail = load_messages(db, conversation_id, &entries[sent.len()..])?;
    Ok((tail[0].role == MessageRole::Assistant).then_some(current))
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn insert_conversation(
    db: &Connection,
    checkout_id: String,
    title: String,
    background: bool,
) -> Result<Conversation> {
    let id = new_id();
    db.execute(
        "INSERT INTO conversations(id, checkout_id, title, background) VALUES (?1, ?2, ?3, ?4)",
        params![id, checkout_id, title, background],
    )?;
    let context_id = activate_context(db, &id, &[])?;
    Ok(Conversation {
        id,
        checkout_id,
        title,
        context_id,
    })
}

fn validate_structured_reply(
    db: &Connection,
    conversation: &Conversation,
    scope: &str,
    origin: &QuestionOrigin,
    reply: &QuestionReply,
) -> Result<(i64, serde_json::Value)> {
    let selected = validate_saved_question(db, &conversation.id, origin, reply)?;
    ensure!(
        selected.scope == scope,
        "the planning question belongs to another profile or checkout"
    );
    let queued: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM recorded_question_answers WHERE request_event_seq = ?1)",
        [selected.seq],
        |row| row.get(0),
    )?;
    ensure!(
        !queued,
        "This recorded answer already belongs to the job queue"
    );
    Ok((selected.seq, selected.answer.schema))
}

pub(crate) struct ValidatedSavedQuestion {
    pub seq: i64,
    pub context: String,
    pub scope: String,
    pub route: QuestionRoute,
    pub answer: RecordedQuestionAnswer,
}

pub(crate) fn validate_saved_question(
    db: &Connection,
    conversation: &str,
    origin: &QuestionOrigin,
    reply: &QuestionReply,
) -> Result<ValidatedSavedQuestion> {
    let (seq, payload, scope, context): (i64, Vec<u8>, String, String) = db
        .query_row(
            "SELECT q.request_event_seq, e.payload, s.scope, link.context_id
         FROM agent_questions q
         JOIN structured_question_continuations link ON link.request_event_seq = q.request_event_seq
         JOIN agent_events e ON e.seq = q.request_event_seq
         JOIN agent_attempts a ON a.run_id = q.run_id
         JOIN agent_sessions s ON s.id = a.agent_session_id
         JOIN open_agent_questions pending ON pending.request_event_seq = q.request_event_seq
         WHERE q.run_id = ?1 AND q.request_key = ?2
           AND pending.state = 'saved' AND a.conversation_id = ?3
           AND link.continuation_run_id IS NULL",
            params![
                origin.run_id,
                permission_key(&origin.request_id)?,
                conversation,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .context("The saved question is absent, answered, or stale for this context")?;
    ensure!(
        crate::jobs::saved_checkpoint_is_current(db, &origin.run_id, seq)?,
        "The saved question needs checkpoint or authorization review"
    );
    let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
    let ProviderEventData::Question {
        request_id,
        form: schema,
        normalized: Some(normalized),
        route,
    } = event.data
    else {
        anyhow::bail!("saved structured question has no validated form")
    };
    ensure!(
        request_id == origin.request_id,
        "saved structured question identity changed"
    );
    normalized.validate(reply)?;
    Ok(ValidatedSavedQuestion {
        seq,
        context,
        scope,
        route,
        answer: RecordedQuestionAnswer {
            origin: origin.clone(),
            form: normalized,
            schema,
            reply: reply.clone(),
        },
    })
}

pub(crate) fn load_conversation(db: &Connection, id: &str) -> Result<Conversation> {
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
        "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1
         UNION ALL SELECT 1 FROM unconfirmed_agent_stops WHERE conversation_id = ?1)",
        [id],
        |row| row.get(0),
    )?;
    ensure!(!active, "conversation already has an active turn");
    Ok(conversation)
}

pub(crate) fn context_entries(
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
    write_context(db, conversation_id, entries, true)
}

fn switch_history(db: &Connection, conversation_id: &str, entries: &[String]) -> Result<String> {
    crate::jobs::apply_queue_history_policy(db, conversation_id)?;
    db.execute(
        "UPDATE conversations SET history_generation = history_generation + 1 WHERE id = ?1",
        [conversation_id],
    )?;
    db.execute(
        "UPDATE agent_sessions SET cursor_context_id = NULL WHERE conversation_id = ?1",
        [conversation_id],
    )?;
    activate_context(db, conversation_id, entries)
}

fn write_context(
    db: &Connection,
    conversation_id: &str,
    entries: &[String],
    activate: bool,
) -> Result<String> {
    let id = new_id();
    db.execute(
        "INSERT INTO conversation_contexts(id, conversation_id, entries, history_generation)
         SELECT ?1, id, ?3, history_generation FROM conversations WHERE id = ?2",
        params![id, conversation_id, serde_json::to_string(entries)?],
    )?;
    if activate {
        db.execute(
            "UPDATE conversations SET active_context_id = ?1 WHERE id = ?2",
            params![id, conversation_id],
        )?;
    }
    Ok(id)
}

pub(crate) fn insert_message(
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

pub(crate) fn load_messages(
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
    session_id: String,
    scope: String,
    provider_session_id: Option<String>,
    cursor: Option<String>,
    native_subagent_limit: Option<u32>,
    native_scope_reserved: bool,
    audit: SubagentAuditRoute,
}

fn previous_attempt(db: &Connection, conversation: &str) -> Result<Option<PreviousAttempt>> {
    Ok(db
        .query_row(
            "SELECT a.run_id, s.id, s.scope, s.provider_session_id, s.cursor_context_id, s.native_subagent_limit,
                (EXISTS (SELECT 1 FROM native_subagent_reservations WHERE session_id = s.id AND state <> 'released')
                 OR EXISTS (SELECT 1 FROM native_goal_executors WHERE run_id = a.run_id)),
                audit.route, audit.native_evidence_id
         FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         LEFT JOIN attempt_subagent_audit audit ON audit.run_id = a.run_id
         WHERE a.conversation_id = ?1 AND a.detached_context = 0 ORDER BY a.ordinal DESC LIMIT 1",
            [conversation],
            |row| {
                Ok(PreviousAttempt {
                    run_id: row.get(0)?,
                    session_id: row.get(1)?,
                    scope: row.get(2)?,
                    provider_session_id: row.get(3)?,
                    cursor: row.get(4)?,
                    native_subagent_limit: row.get(5)?,
                    native_scope_reserved: row.get(6)?,
                    audit: match row.get::<_, Option<String>>(7)?.as_deref() {
                        Some("managed") => SubagentAuditRoute::Managed,
                        Some("native") => SubagentAuditRoute::Native { evidence_id: row.get(8)? },
                        _ => SubagentAuditRoute::Available,
                    },
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
    detached_context: bool,
}

fn active_attempt(db: &Connection, id: &str) -> Result<ActiveAttempt> {
    db.query_row(
        "SELECT a.conversation_id, a.agent_session_id, a.context_id, a.turn_id, a.detached_context
         FROM agent_attempts a JOIN active_agent_attempts l ON l.run_id = a.run_id WHERE a.run_id = ?1",
        [id], |row| Ok(ActiveAttempt { conversation_id:row.get(0)?, session_id:row.get(1)?, context_id:row.get(2)?, turn_id:row.get(3)?, detached_context:row.get(4)? }),
    ).context("agent attempt is no longer active")
}

fn permission_key(id: &RequestId) -> Result<[u8; 32]> {
    Ok(Sha256::digest(serde_json::to_vec(id)?).into())
}

fn close_native_questions(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE agent_questions
         SET delivery = CASE delivery WHEN 'prepared' THEN 'unconfirmed' ELSE 'closed' END
         WHERE run_id = ?1 AND route = 'native' AND delivery IN ('pending', 'prepared')",
        [run_id],
    )?;
    Ok(())
}

fn close_unfinished_structured_questions(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE agent_questions SET delivery = 'closed'
         WHERE run_id = ?1 AND route = 'structured_turn' AND delivery = 'pending'",
        [run_id],
    )?;
    Ok(())
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
    async fn dismissing_a_saved_planning_question_keeps_the_completed_turn_closed() -> Result<()> {
        use crate::providers::questions::QuestionForm;

        let (_dir, store, chat, scope) = setup().await?;
        let attempt = store
            .prepare_structured_planning_attempt(
                chat.id.clone(),
                chat.context_id.clone(),
                scope.clone(),
                "Plan a report".into(),
                Vec::new(),
            )
            .await?;
        let origin = QuestionOrigin {
            run_id: attempt.run.id.clone(),
            request_id: RequestId::Text(format!("structured:{}", attempt.run.turn_id)),
        };
        let schema = json!({"type":"object","properties":{"summary":{"type":"boolean"}},"required":["summary"]});
        let message = "Include a summary?";
        store.mark_agent_running(origin.run_id.clone()).await?;
        store
            .record_agent_observation(
                origin.run_id.clone(),
                EventSource::Application,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::Question {
                        request_id: origin.request_id.clone(),
                        form: json!({"message":message,"schema":schema}),
                        normalized: Some(QuestionForm::schema(message.into(), &schema)?),
                        route: QuestionRoute::StructuredTurn,
                    },
                },
            )
            .await?;
        store
            .finish_agent_attempt(
                origin.run_id.clone(),
                RunStatus::Succeeded,
                Some(message.into()),
            )
            .await?;
        let base = store.conversation(chat.id.clone()).await?.context_id;
        assert!(
            store
                .dismiss_structured_planning_question(
                    chat.id.clone(),
                    chat.context_id,
                    scope.clone(),
                    origin.clone(),
                    QuestionAction::Cancel,
                )
                .await
                .is_err()
        );
        store
            .dismiss_structured_planning_question(
                chat.id.clone(),
                base.clone(),
                scope.clone(),
                origin.clone(),
                QuestionAction::Cancel,
            )
            .await?;
        assert!(
            store
                .prepare_structured_planning_reply(
                    chat.id.clone(),
                    base.clone(),
                    scope,
                    origin.clone(),
                    QuestionReply {
                        action: QuestionAction::Accept,
                        values: [("summary".into(), json!(true))].into_iter().collect()
                    },
                )
                .await
                .is_err()
        );
        let events = store
            .agent_observations(chat.id.clone(), origin.run_id.clone(), 0)
            .await?;
        assert!(events.iter().any(|entry| matches!(
            &entry.event.data, ProviderEventData::StructuredQuestionDismissed { origin: saved, action: QuestionAction::Cancel }
                if saved == &origin
        )));
        assert_eq!(store.conversation(chat.id.clone()).await?.context_id, base);
        let attempts: i64 = store
            .execute(move |db| {
                Ok(db.query_row(
                    "SELECT count(*) FROM agent_attempts WHERE conversation_id = ?1",
                    [chat.id],
                    |row| row.get(0),
                )?)
            })
            .await?;
        assert_eq!(attempts, 1);
        Ok(())
    }

    #[tokio::test]
    async fn question_deadlines_preserve_their_original_requests() -> Result<()> {
        use crate::{jobs::QuestionTimer, providers::questions::QuestionForm};

        let (_dir, store, chat, scope) = setup().await?;
        let attempt = prepare(&store, &chat.id, scope, "Multiple questions").await?;
        let run = attempt.run.id.clone();
        let project = attempt.run.project_id.clone();
        store.mark_agent_running(run.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &attempt).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;

        let record = |id| {
            let store = &store;
            let run = run.clone();
            async move {
                let raw = json!([{"id":"target","question":"Which target?","options":[{"label":"Desktop"}]}]);
                store
                    .record_agent_observation(
                        run,
                        EventSource::Provider,
                        ProviderEvent {
                            location: Default::default(),
                            data: ProviderEventData::Question {
                                request_id: id,
                                normalized: Some(QuestionForm::codex(&raw)?),
                                form: raw,
                                route: QuestionRoute::Native,
                            },
                        },
                    )
                    .await
            }
        };
        assert_eq!(store.project_question_timeout(project.clone()).await?, 10);
        record(RequestId::Number(7)).await?;
        store
            .set_project_question_timeout(project.clone(), 1)
            .await?;
        assert!(
            store
                .set_project_question_timeout(project, 0)
                .await
                .is_err()
        );
        record(RequestId::Text("7".into())).await?;
        let deadlines = store.execute(|db| {
            let mut query = db.prepare("SELECT opened_ms, deadline_ms FROM native_question_waits ORDER BY request_event_seq")?;
            Ok(query.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        }).await?;
        assert_eq!(deadlines.len(), 2);
        assert_eq!(deadlines[0].1 - deadlines[0].0, 600_000);
        assert_eq!(deadlines[1].1 - deadlines[1].0, 60_000);
        let due = deadlines[1].1;
        assert_eq!(
            store
                .pending_conversation_inputs(chat.id.clone(), run.clone())
                .await?
                .len(),
            2
        );
        assert!(
            matches!(store.poll_question_timer(run.clone(), due - 1).await?, QuestionTimer::WakeAt(at) if at == due)
        );
        assert!(matches!(
            store.poll_question_timer(run.clone(), due).await?,
            QuestionTimer::Stop
        ));
        assert!(
            store
                .pending_conversation_inputs(chat.id.clone(), run.clone())
                .await?
                .is_empty()
        );
        store
            .finish_observed_agent_attempt(run.clone(), RunStatus::Interrupted, None, None, true)
            .await?;
        let saved = store.saved_conversation_questions(chat.id, run).await?;
        assert_eq!(saved.len(), 2);
        assert_eq!(
            (&*saved[0].state, &*saved[0].reason),
            ("saved", "turn_stopped")
        );
        assert_eq!((&*saved[1].state, &*saved[1].reason), ("saved", "timeout"));
        assert!(matches!(
            &saved[0].event.data,
            ProviderEventData::Question {
                request_id: RequestId::Number(7),
                ..
            }
        ));
        assert!(
            matches!(&saved[1].event.data, ProviderEventData::Question { request_id: RequestId::Text(id), .. } if id == "7")
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_question_replies_are_scoped_and_recover_as_unconfirmed() -> Result<()> {
        use crate::providers::questions::{QuestionAction, QuestionForm, QuestionReply};

        let (_dir, store, chat, scope) = setup().await?;
        let other = store
            .create_conversation(chat.checkout_id.clone(), "Another run".into())
            .await?;
        let first = prepare(&store, &chat.id, scope.clone(), "First question").await?;
        let second = prepare(&store, &other.id, scope, "Another question").await?;
        let id = RequestId::Text("provider-question".into());
        let raw = json!([{"id":"choice","question":"Which option?","options":[{"label":"Bound answer"}]}]);
        let question = ProviderEvent {
            location: ProviderLocation::default(),
            data: ProviderEventData::Question {
                request_id: id.clone(),
                normalized: Some(QuestionForm::codex(&raw)?),
                form: raw,
                route: QuestionRoute::Native,
            },
        };
        store
            .record_agent_observation(
                first.run.id.clone(),
                EventSource::Provider,
                question.clone(),
            )
            .await?;
        let prepared = ProviderEvent {
            location: ProviderLocation::default(),
            data: ProviderEventData::QuestionReplyPrepared {
                request_id: id,
                reply: QuestionReply {
                    action: QuestionAction::Accept,
                    values: serde_json::from_value(json!({"choice":"Bound answer"}))?,
                },
                frame: b"{\"id\":\"provider-question\",\"result\":{\"answers\":{\"choice\":{\"answers\":[\"Bound answer\"]}}}}\n".to_vec(),
            },
        };
        assert!(
            store
                .record_agent_observation(
                    second.run.id.clone(),
                    EventSource::Application,
                    prepared.clone(),
                )
                .await
                .is_err()
        );
        store
            .record_agent_observation(second.run.id.clone(), EventSource::Provider, question)
            .await?;
        store
            .record_agent_observation(
                first.run.id.clone(),
                EventSource::Application,
                prepared.clone(),
            )
            .await?;
        assert!(
            store
                .record_agent_observation(first.run.id.clone(), EventSource::Application, prepared)
                .await
                .is_err()
        );
        store.recover_agent_attempts_after_restart().await?;
        let decisions = store.execute(move |db| {
            let row = |run: &str| db.query_row(
                "SELECT q.delivery, e.payload FROM agent_questions q LEFT JOIN agent_events e ON e.seq = q.reply_event_seq WHERE q.run_id = ?1",
                [run], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
            );
            Ok((row(&first.run.id)?, row(&second.run.id)?))
        }).await?;
        assert_eq!(decisions.0.0, "unconfirmed");
        let encrypted = decisions.0.1.unwrap();
        assert!(
            !encrypted
                .windows(b"Bound answer".len())
                .any(|range| range == b"Bound answer")
        );
        assert_eq!(decisions.1, ("closed".into(), None));
        Ok(())
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
        prepare_fixture_dispatch_with_capabilities(store, attempt, None).await
    }

    async fn prepare_fixture_dispatch_with_capabilities(
        store: &Store,
        attempt: &AttemptPlan,
        capabilities: Option<crate::providers::capabilities::CapabilityAssessment>,
    ) -> Result<String> {
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
                    session_id: None,
                    job_policy_revision: None,
                    role_context: None,
                    base_commit: None,
                    submitted_messages: attempt.input.clone(),
                    native_history,
                    capabilities,
                    filesystem_skills: Vec::new(),
                    reported_skills: None,
                    instructions: None,
                    scoped_launch: None,
                    requested_skill: None,
                    plugin_inventory: None,
                    attachments: Vec::new(),
                    task_context: None,
                    repository_markdown: None,
                    question_route: QuestionRoute::Native,
                    answered_question: None,
                },
                vec![b"{\"fixture\":true}\n".to_vec()],
            )
            .await
    }

    #[tokio::test]
    async fn steering_preserves_receipts_and_requires_review_for_uncertain_delivery() -> Result<()>
    {
        use crate::{
            dispatch::{InstructionDispatchMode, InstructionDispatchStatus, InstructionReceipt},
            jobs::{AdmissionEnvironment, InstructionDelivery, JobContext},
            providers::{
                capabilities::{
                    Availability, CapabilityAssessment, CapabilityDescriptor, CapabilityScope,
                    DeclaredSource, EvidenceBoundary, ResolvedCapability, Verification,
                },
                steering::{SteerOutcome, SteerTarget},
            },
        };
        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let enqueue = |store: Store, text: &'static str| {
            let (conversation, scope, env) = (conversation.id.clone(), scope.clone(), env.clone());
            async move {
                let current = store.conversation(conversation.clone()).await?;
                store
                    .save_conversation_draft(conversation.clone(), text.into())
                    .await?;
                let job = store
                    .enqueue_conversation_turn(
                        JobContext {
                            conversation_id: conversation.clone(),
                            context_id: current.context_id,
                        },
                        scope,
                        text.into(),
                    )
                    .await?;
                let pending = store
                    .pending_conversation_instructions(conversation, env)
                    .await?;
                pending
                    .into_iter()
                    .find(|entry| entry.id == job.id)
                    .context("Queued instruction disappeared")
            }
        };
        enqueue(store.clone(), "Initial prompt").await?;
        let original = store
            .claim_next_job(env.clone())
            .await?
            .context("Initial turn was not admitted")?;
        store.mark_agent_running(original.run.id.clone()).await?;
        let capability_scope = CapabilityScope::from_dispatch(
            &scope,
            conversation.checkout_id.clone(),
            None,
            None,
            serde_json::from_value(
                json!({"detected":[],"unresolved":[],"providerRevision":null,"effectiveRevision":null}),
            )?,
            None,
            None,
        );
        let dispatch = prepare_fixture_dispatch_with_capabilities(
            &store,
            &original,
            Some(CapabilityAssessment {
                version: 1,
                scope: capability_scope,
                entries: vec![ResolvedCapability {
                    descriptor: CapabilityDescriptor {
                        subject: CapabilitySubject::Protocol {
                            capability: Capability::LiveSteering,
                        },
                        source: DeclaredSource::Provider,
                        availability: Availability::Available,
                        observed: None,
                        blocker: None,
                        boundary: EvidenceBoundary::ExactCheckout,
                    },
                    evidence: Some(Verification {
                        id: "observed-steer".into(),
                        checked_at: "fixture".into(),
                        artifact_id: "verified-recording".into(),
                        origin: EvidenceOrigin::AuthenticatedFixture,
                        result: ObservedResult::Succeeded,
                        attachment_limits: None,
                    }),
                }],
            }),
        )
        .await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let target = SteerTarget {
            session_id: "thread".into(),
            turn_id: "source-turn".into(),
        };
        store
            .record_agent_observation(
                original.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some(target.session_id.clone()),
                        turn_id: Some(target.turn_id.clone()),
                        ..Default::default()
                    },
                    data: ProviderEventData::SessionReady {
                        model: None,
                        effort: None,
                    },
                },
            )
            .await?;
        store
            .record_agent_observation(
                original.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some(target.session_id.clone()),
                        turn_id: Some(target.turn_id.clone()),
                        ..Default::default()
                    },
                    data: ProviderEventData::TurnStarted,
                },
            )
            .await?;
        let claim = |store: Store, id: String, revision: String| {
            let (conversation, run) = (conversation.id.clone(), original.run.id.clone());
            async move {
                store
                    .claim_native_instruction(conversation, id, revision, run)
                    .await
            }
        };
        let prepare = |store: Store, message_id: String| {
            let (run, target) = (original.run.id.clone(), target.clone());
            async move {
                store
                    .record_agent_observation(
                        run,
                        EventSource::Application,
                        ProviderEvent {
                            location: ProviderLocation::default(),
                            data: ProviderEventData::SteeringPrepared {
                                message_id,
                                target,
                                frame: b"fixture-steer".to_vec(),
                            },
                        },
                    )
                    .await
            }
        };
        let settled = |target: &SteerTarget, message_id, outcome| ProviderEvent {
            location: ProviderLocation::default(),
            data: ProviderEventData::SteeringSettled {
                target: target.clone(),
                message_id,
                outcome,
            },
        };

        let rejected = enqueue(store.clone(), "Definitively rejected").await?;
        assert!(
            claim(
                store.clone(),
                rejected.id.clone(),
                rejected.revision_id.clone()
            )
            .await?
            .is_some()
        );
        prepare(store.clone(), rejected.revision_id.clone()).await?;
        store
            .record_agent_observation(
                original.run.id.clone(),
                EventSource::Provider,
                settled(
                    &target,
                    rejected.revision_id.clone(),
                    SteerOutcome::ProviderRejected,
                ),
            )
            .await?;
        let queued = store
            .pending_conversation_instructions(conversation.id.clone(), env.clone())
            .await?;
        assert_eq!(queued[0].id, rejected.id);
        assert_eq!(queued[0].delivery, InstructionDelivery::Queued);
        assert_eq!(queued[0].revision_id, rejected.revision_id);
        store
            .remove_pending_instruction(conversation.id.clone(), rejected.id, rejected.revision_id)
            .await?;

        let accepted = enqueue(store.clone(), "Original revision").await?;
        store
            .revise_pending_instruction(
                conversation.id.clone(),
                accepted.id.clone(),
                accepted.revision_id.clone(),
                "Accepted revision".into(),
                None,
            )
            .await?;
        assert!(
            claim(store.clone(), accepted.id.clone(), accepted.revision_id)
                .await?
                .is_none()
        );
        let pending = store
            .pending_conversation_instructions(conversation.id.clone(), env.clone())
            .await?;
        let revision = pending[0].revision_id.clone();
        let selected = claim(store.clone(), accepted.id.clone(), revision.clone())
            .await?
            .context("No steering claim")?;
        assert_eq!(selected.target, target);
        assert_eq!(selected.input.text, "Accepted revision");
        assert!(
            claim(store.clone(), accepted.id, revision.clone())
                .await?
                .is_none()
        );
        prepare(store.clone(), revision.clone()).await?;
        let wrong = SteerTarget {
            session_id: target.session_id.clone(),
            turn_id: "wrong-turn".into(),
        };
        assert!(
            store
                .record_agent_observation(
                    original.run.id.clone(),
                    EventSource::Provider,
                    settled(&wrong, revision.clone(), SteerOutcome::Accepted)
                )
                .await
                .is_err()
        );
        store
            .record_agent_observation(
                original.run.id.clone(),
                EventSource::Provider,
                settled(&target, revision.clone(), SteerOutcome::Accepted),
            )
            .await?;

        let uncertain = enqueue(store.clone(), "Pending confirmation").await?;
        assert!(
            claim(
                store.clone(),
                uncertain.id.clone(),
                uncertain.revision_id.clone()
            )
            .await?
            .is_some()
        );
        prepare(store.clone(), uncertain.revision_id.clone()).await?;
        drop(store);
        let store = Store::open(scope.checkout.join("store.sqlite"))?;
        store.recover_agent_attempts_after_restart().await?;
        let original_dispatch = store
            .agent_dispatch(conversation.id.clone(), original.run.id.clone())
            .await?
            .unwrap();
        assert_eq!(original_dispatch.snapshot.submitted_messages.len(), 1);
        let [
            initial_delivery,
            rejected_delivery,
            accepted_delivery,
            uncertain_delivery,
        ] = original_dispatch.instructions.as_slice()
        else {
            anyhow::bail!("Expected the original delivery and three ordered steering attempts");
        };
        assert_eq!(initial_delivery.text, "Initial prompt");
        assert_eq!(initial_delivery.mode, InstructionDispatchMode::Standalone);
        assert_eq!(
            initial_delivery.status,
            InstructionDispatchStatus::Submitted
        );
        assert!(matches!(
            initial_delivery.receipt,
            Some(InstructionReceipt::TurnStarted { .. })
        ));
        assert_eq!(rejected_delivery.text, "Definitively rejected");
        assert_eq!(
            rejected_delivery.status,
            InstructionDispatchStatus::Rejected
        );
        assert_eq!(accepted_delivery.revision_id, revision);
        assert_eq!(accepted_delivery.text, "Accepted revision");
        assert_eq!(accepted_delivery.mode, InstructionDispatchMode::Steering);
        assert_eq!(
            accepted_delivery.status,
            InstructionDispatchStatus::Accepted
        );
        assert_eq!(
            accepted_delivery.steering_frame.as_deref(),
            Some("fixture-steer")
        );
        assert_eq!(
            accepted_delivery.provider_turn_id.as_deref(),
            Some(target.turn_id.as_str())
        );
        assert!(matches!(
            accepted_delivery.receipt,
            Some(InstructionReceipt::SteeringResponse {
                outcome: SteerOutcome::Accepted,
                ..
            })
        ));
        assert_eq!(uncertain_delivery.instruction_id, uncertain.id);
        assert_eq!(
            uncertain_delivery.status,
            InstructionDispatchStatus::Unconfirmed
        );
        assert_eq!(uncertain_delivery.receipt, None);
        assert!(
            store
                .agent_dispatch("other-conversation".into(), original.run.id.clone())
                .await?
                .is_none()
        );
        let history = store.conversation_history(conversation.id.clone()).await?;
        assert_eq!(
            history
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["Initial prompt", "Accepted revision"]
        );
        let pending = store
            .pending_conversation_instructions(conversation.id.clone(), env.clone())
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, uncertain.id);
        assert_eq!(pending[0].delivery, InstructionDelivery::NeedsReview);
        assert_eq!(pending[0].run_id.as_deref(), Some(original.run.id.as_str()));
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        store
            .resolve_uncertain_steering(
                conversation.id.clone(),
                uncertain.id,
                uncertain.revision_id.clone(),
                original.run.id.clone(),
                true,
            )
            .await?;
        let reviewed = store
            .agent_dispatch(conversation.id, original.run.id)
            .await?
            .unwrap();
        assert_eq!(
            reviewed
                .instructions
                .last()
                .and_then(|instruction| instruction.review),
            Some(crate::dispatch::InstructionReview::Undelivered)
        );
        let retry = store
            .claim_next_job(env)
            .await?
            .context("Reviewed instruction did not rejoin the queue")?;
        assert_eq!(
            retry.input.last().unwrap().revision_id,
            uncertain.revision_id
        );
        assert_eq!(
            retry
                .input
                .iter()
                .filter(|m| m.revision_id == revision)
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn restarting_a_native_goal_restores_its_output_before_explicit_recovery() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalControlState, GoalDefinition, JobContext,
        };
        let (_directory, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        scope.runner = crate::runner::DirectWindowsRunner::IDENTITY.into();
        let database = scope.checkout.join("store.sqlite");
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope.clone(),
                None,
                GoalDefinition {
                    objective: "Restore the recorded provider work".into(),
                    criteria: vec![],
                    max_turns: None,
                    max_seconds: None,
                    token_budget: None,
                },
                None,
            )
            .await?;
        let attempt = store
            .claim_next_job(environment.clone())
            .await?
            .context("missing native fixture attempt")?;
        let native_location = |turn: Option<&str>| ProviderLocation {
            session_id: Some("retired-thread".into()),
            turn_id: turn.map(str::to_owned),
            ..Default::default()
        };
        store.mark_agent_running(attempt.run.id.clone()).await?;
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: native_location(None),
                    data: ProviderEventData::SessionReady {
                        model: Some("requested-model".into()),
                        effort: None,
                    },
                },
            )
            .await?;
        store.execute({
            let revision = goal.id.clone();
            let run = attempt.run.id.clone();
            move |db| {
                db.execute(
                    "INSERT INTO capability_evidence(id, version, qualification_key, origin, payload)
                     VALUES ('restart-fixture', 1, 'test', 'protocol_fixture', X'00')", [],
                )?;
                db.execute(
                    "INSERT INTO native_goal_executors(run_id, revision_id, provider_session_id, evidence_id)
                     VALUES (?1, ?2, 'retired-thread', 'restart-fixture')",
                    params![run, revision],
                )?;
                db.execute("UPDATE goal_revisions SET continuation_owner = 'native' WHERE id = ?1", [revision])?;
                Ok(())
            }
        }).await?;
        for (id, data) in [
            ("provider-turn-1", ProviderEventData::TurnStarted),
            (
                "provider-turn-1",
                ProviderEventData::AssistantDelta {
                    text: "Streaming duplicate".into(),
                },
            ),
            (
                "provider-turn-1",
                ProviderEventData::AssistantMessage {
                    text: "Saved result".into(),
                },
            ),
            (
                "provider-turn-1",
                ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            ),
            ("provider-turn-2", ProviderEventData::TurnStarted),
            (
                "provider-turn-2",
                ProviderEventData::AssistantDelta {
                    text: "Partial next result".into(),
                },
            ),
        ] {
            store
                .record_agent_observation(
                    attempt.run.id.clone(),
                    EventSource::Provider,
                    ProviderEvent {
                        location: native_location(Some(id)),
                        data,
                    },
                )
                .await?;
        }
        let pending_text = "Deliver this only after the native goal yields";
        let current = store.conversation(conversation.id.clone()).await?;
        store
            .save_conversation_draft(conversation.id.clone(), pending_text.into())
            .await?;
        let pending = store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: current.context_id,
                },
                scope,
                pending_text.into(),
            )
            .await?;
        let assert_native_owner = |store: Store, blocker: AdmissionBlocker| {
            let (project, job, env) = (
                pending.project_id.clone(),
                pending.id.clone(),
                environment.clone(),
            );
            async move {
                let inspection = store
                    .inspect_job(project, job, env.clone())
                    .await?
                    .context("missing instruction")?;
                assert!(
                    inspection.blockers.contains(&blocker),
                    "{:?}",
                    inspection.blockers
                );
                assert!(store.claim_next_job(env).await?.is_none());
                Ok::<_, anyhow::Error>(())
            }
        };
        assert_native_owner(
            store.clone(),
            AdmissionBlocker::ConversationBusy {
                run_id: attempt.run.id.clone(),
            },
        )
        .await?;
        drop(store);
        let store = Store::open(database)?;
        store.recover_agent_attempts_after_restart().await?;
        let project = goal.job.project_id;
        assert_native_owner(
            store.clone(),
            AdmissionBlocker::ContinuationOwner {
                run_id: attempt.run.id.clone(),
            },
        )
        .await?;
        assert!(
            store
                .resume_goal(project.clone(), goal.id.clone())
                .await
                .is_err()
        );
        assert_eq!(
            store
                .confirm_native_goal_stop(project.clone(), goal.id.clone(), attempt.run.id.clone())
                .await?,
            GoalControlState::Paused
        );
        store.resume_goal(project, goal.id).await?;
        let recovery = store
            .claim_next_job(environment)
            .await?
            .context("missing reconciled attempt")?;
        assert!(matches!(recovery.intent, SessionIntent::Start));
        assert_eq!(
            recovery.previous_run_id.as_deref(),
            Some(attempt.run.id.as_str())
        );
        assert!(
            recovery
                .input
                .iter()
                .any(|message| message.text == "Saved result\n\nPartial next result")
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_a_goal_waits_for_owned_children_and_fences_new_delegation() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalControlState, GoalDefinition,
            GoalStopAction, JobContext, JobRequest, QueueState,
        };
        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope,
                None,
                GoalDefinition {
                    objective: "Coordinate owned child work".into(),
                    criteria: vec![],
                    max_turns: Some(2),
                    max_seconds: None,
                    token_budget: None,
                },
                None,
            )
            .await?;
        let project = goal.job.project_id.clone();
        store.set_job_queue_capacity(2).await?;
        let root = store
            .claim_next_job(environment.clone())
            .await?
            .context("missing goal attempt")?;
        store.mark_agent_running(root.run.id.clone()).await?;
        let request = JobRequest {
            checkout_id: None,
            task_id: None,
            mode: SessionMode::ReadOnly,
            instructions: "Child evidence".into(),
            dependencies: vec![],
            requires_bridge: false,
        };
        let child = store
            .submit_agent_job(root.run.id.clone(), "child".into(), request.clone())
            .await?;
        let inspection = store
            .inspect_job(project.clone(), child.id.clone(), environment.clone())
            .await?
            .unwrap();
        let [AdmissionBlocker::Approval { approval_id }] = inspection.blockers.as_slice() else {
            anyhow::bail!("expected a review for the child");
        };
        store
            .decide_job_approval(project.clone(), approval_id.clone(), true)
            .await?;
        let running = store
            .claim_next_job(environment.clone())
            .await?
            .context("missing admitted child")?;
        store.mark_agent_running(running.run.id.clone()).await?;
        let pending = store
            .submit_agent_job(running.run.id.clone(), "grandchild".into(), request.clone())
            .await?;
        assert_eq!(
            store
                .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Cancel)
                .await?,
            GoalControlState::Cancelling
        );
        assert!(
            store
                .submit_agent_job(running.run.id.clone(), "new".into(), request.clone())
                .await
                .is_err()
        );
        assert_eq!(
            store
                .submit_agent_job(root.run.id.clone(), "child".into(), request)
                .await?
                .id,
            child.id
        );
        let cancelled_child = store.job(project.clone(), child.id).await?.unwrap();
        assert_eq!(cancelled_child.queue_state, QueueState::Cancelled);
        assert!(cancelled_child.latest_attempt.unwrap().cancel_requested);
        assert_eq!(
            store
                .job(project.clone(), pending.id)
                .await?
                .unwrap()
                .queue_state,
            QueueState::Cancelled
        );
        store
            .finish_observed_agent_attempt(root.run.id, RunStatus::Cancelled, None, None, true)
            .await?;
        assert_eq!(
            store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .unwrap()
                .control_state,
            GoalControlState::Cancelling
        );
        assert!(store.claim_next_job(environment).await?.is_none());
        store
            .finish_observed_agent_attempt(running.run.id, RunStatus::Cancelled, None, None, true)
            .await?;
        assert!(
            store
                .active_conversation_goal(conversation.id)
                .await?
                .is_none()
        );
        assert_eq!(
            store
                .request_goal_stop(project, goal.id, GoalStopAction::Cancel)
                .await?,
            GoalControlState::Cancelled
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_saved_goal_question_cannot_dispatch_past_the_turn_limit() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalDefinition, JobContext, QuestionTimer,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope,
                None,
                GoalDefinition {
                    objective: "Request a decision in one turn".into(),
                    criteria: vec![],
                    max_turns: Some(1),
                    max_seconds: None,
                    token_budget: None,
                },
                None,
            )
            .await?;
        let attempt = store.claim_next_job(environment.clone()).await?.unwrap();
        store.mark_agent_running(attempt.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &attempt).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let request_id = RequestId::Number(44);
        let schema = json!({"type":"object","properties":{"continue":{"type":"boolean"}},"required":["continue"]});
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::Question {
                        request_id: request_id.clone(),
                        form: json!({"message":"Continue?","schema":schema}),
                        normalized: Some(QuestionForm::schema("Continue?".into(), &schema)?),
                        route: QuestionRoute::Native,
                    },
                },
            )
            .await?;
        assert!(matches!(
            store
                .poll_question_timer(attempt.run.id.clone(), i64::MAX)
                .await?,
            QuestionTimer::Stop
        ));
        store
            .finish_observed_agent_attempt(
                attempt.run.id.clone(),
                RunStatus::Interrupted,
                None,
                None,
                true,
            )
            .await?;
        drop(store);

        let store = Store::open(database)?;
        assert_eq!(
            store
                .saved_conversation_questions(conversation.id.clone(), attempt.run.id.clone())
                .await?[0]
                .reason,
            "timeout"
        );
        store
            .answer_saved_question(
                conversation.id,
                QuestionOrigin {
                    run_id: attempt.run.id,
                    request_id,
                },
                QuestionReply {
                    action: QuestionAction::Accept,
                    values: [("continue".into(), json!(true))].into_iter().collect(),
                },
            )
            .await?;
        let inspected = store
            .inspect_job(goal.job.project_id, goal.job.id, environment.clone())
            .await?
            .unwrap();
        assert_eq!(inspected.queued_question, Some(true));
        assert_eq!(
            inspected.blockers,
            [AdmissionBlocker::Policy {
                reason: "The goal reached its turn limit"
            }]
        );
        assert!(store.claim_next_job(environment).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn delegated_goal_work_preserves_approval_and_dependency_waits_after_revocation()
    -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, DownstreamWaitState, GoalDefinition,
            JobContext, JobPermission, JobRequest, JobRole, JobRule, JobWorkspace,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        scope.runner = crate::runner::DirectWindowsRunner::IDENTITY.into();
        let database = scope.checkout.join("store.sqlite");
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id,
                    context_id: conversation.context_id,
                },
                scope,
                None,
                GoalDefinition {
                    objective: "Wait for delegated evidence".into(),
                    criteria: vec![],
                    max_turns: Some(3),
                    max_seconds: Some(120),
                    token_budget: None,
                },
                None,
            )
            .await?;
        store.set_job_queue_capacity(1).await?;
        let root = store.claim_next_job(environment.clone()).await?.unwrap();
        store.mark_agent_running(root.run.id.clone()).await?;
        let child = store
            .submit_agent_job(
                root.run.id.clone(),
                "child".into(),
                JobRequest {
                    checkout_id: None,
                    task_id: None,
                    mode: SessionMode::ReadOnly,
                    instructions: "Collect evidence".into(),
                    dependencies: vec![],
                    requires_bridge: false,
                },
            )
            .await?;
        store
            .request_downstream_wait(root.run.id.clone(), "wait".into(), vec![child.id.clone()])
            .await?;
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store
            .finish_observed_agent_attempt(
                root.run.id.clone(),
                RunStatus::Interrupted,
                None,
                None,
                true,
            )
            .await?;
        drop(store);

        let store = Store::open(database)?;
        let project = goal.job.project_id;
        let revision = goal.id;
        let wait = store
            .downstream_wait(project.clone(), root.run.id.clone())
            .await?
            .unwrap();
        assert_eq!(wait.state, DownstreamWaitState::Waiting);
        assert!(wait.inputs[0].status.is_none());
        let approval = store
            .inspect_job(project.clone(), child.id.clone(), environment.clone())
            .await?
            .unwrap();
        let [AdmissionBlocker::Approval { approval_id }] = approval.blockers.as_slice() else {
            anyhow::bail!("Expected the child to wait for approval");
        };
        store
            .decide_job_approval(project.clone(), approval_id.clone(), true)
            .await?;
        let running = store
            .claim_next_job(environment.clone())
            .await?
            .context("the child never acquired released capacity")?;
        store.mark_agent_running(running.run.id.clone()).await?;
        let denied = vec![JobRule {
            role: JobRole::Operator,
            operation: SessionMode::ReadOnly,
            workspace: JobWorkspace::Project,
            permission: JobPermission::Deny,
            requirements: Default::default(),
        }];
        store
            .set_project_job_rules(project.clone(), vec![], denied.clone())
            .await?;
        assert!(
            store
                .goal_delegation_revoked(running.run.id.clone())
                .await?
        );
        assert!(prepare_fixture_dispatch(&store, &running).await.is_err());
        assert_eq!(
            store
                .finish_observed_agent_attempt(
                    running.run.id.clone(),
                    RunStatus::Succeeded,
                    None,
                    None,
                    false
                )
                .await?,
            RunStatus::Interrupted
        );
        assert_eq!(
            store
                .job_failure(project.clone(), running.run.id)
                .await?
                .unwrap()
                .observation
                .cause,
            Some(crate::jobs::FailureCause::GoalAuthorityRevoked)
        );
        let waiting = store
            .inspect_job(project.clone(), goal.job.id, environment.clone())
            .await?
            .unwrap();
        assert!(waiting.blockers.contains(&AdmissionBlocker::Policy {
            reason: "A project rule denies this role, operation, and workspace"
        }));
        assert!(
            waiting
                .blockers
                .iter()
                .any(|reason| matches!(reason, AdmissionBlocker::Downstream { pending: 1, .. }))
        );
        assert_eq!(
            store
                .inspect_project_queue(project.clone(), environment.clone())
                .await?
                .active,
            1
        );
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store
            .set_project_job_rules(project.clone(), denied, vec![])
            .await?;
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store.recover_agent_attempts_after_restart().await?;
        assert_eq!(
            store
                .inspect_project_queue(project.clone(), environment.clone())
                .await?
                .active,
            0
        );
        let resumed = store.claim_next_job(environment).await?.unwrap();
        assert_eq!(
            resumed.previous_run_id.as_deref(),
            Some(root.run.id.as_str())
        );
        assert!(
            resumed
                .input
                .iter()
                .any(|message| message.text.contains(&child.id))
        );
        store.mark_agent_running(resumed.run.id.clone()).await?;
        let pending = store
            .submit_agent_job(
                resumed.run.id.clone(),
                "another-child".into(),
                JobRequest {
                    checkout_id: None,
                    task_id: None,
                    mode: SessionMode::ReadOnly,
                    instructions: "Additional evidence".into(),
                    dependencies: vec![],
                    requires_bridge: false,
                },
            )
            .await?;
        store
            .execute(move |db| {
                db.execute(
                    "UPDATE goal_revisions SET created_ms = 0, deadline_ms = 1 WHERE id = ?1",
                    [revision],
                )?;
                Ok(())
            })
            .await?;
        assert!(
            store
                .goal_execution_for_run(resumed.run.id.clone())
                .await?
                .unwrap()
                .time_limit_reached()
        );
        assert!(prepare_fixture_dispatch(&store, &resumed).await.is_err());
        let pending = store
            .inspect_job(
                project,
                pending.id,
                AdmissionEnvironment {
                    runner: resumed.scope.runner.clone(),
                    bridge_available: true,
                },
            )
            .await?
            .unwrap();
        assert!(pending.blockers.contains(&AdmissionBlocker::Policy {
            reason: "The goal reached its time limit"
        }));
        Ok(())
    }

    #[tokio::test]
    async fn managed_goal_pause_fences_the_current_run_and_resumes_its_checkpoint() -> Result<()> {
        exercise_goal_stop(GoalStopScenario::ConfirmedPause).await
    }

    #[tokio::test]
    async fn revoking_authority_pauses_a_running_goal_and_fences_its_checkpoint() -> Result<()> {
        exercise_goal_stop(GoalStopScenario::AuthorityRevokedWhileRunning).await?;
        exercise_goal_stop(GoalStopScenario::AuthorityRevokedBeforeSettlement).await
    }

    #[tokio::test]
    async fn restarting_with_a_goal_stop_in_flight_keeps_the_revision_for_review() -> Result<()> {
        exercise_goal_stop(GoalStopScenario::RestartDuringPause).await
    }

    #[tokio::test]
    async fn restarting_during_goal_execution_requires_stop_review_before_resume() -> Result<()> {
        exercise_goal_stop(GoalStopScenario::RestartDuringExecution).await
    }

    #[tokio::test]
    async fn cancel_history_policy_waits_for_an_uncertain_goal_executor() -> Result<()> {
        exercise_goal_stop(GoalStopScenario::CancelAfterRestart).await
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum GoalStopScenario {
        ConfirmedPause,
        AuthorityRevokedWhileRunning,
        AuthorityRevokedBeforeSettlement,
        RestartDuringPause,
        RestartDuringExecution,
        CancelAfterRestart,
    }

    async fn exercise_goal_stop(scenario: GoalStopScenario) -> Result<()> {
        use crate::jobs::{
            AdmissionEnvironment, FailureCause, GoalControlState, GoalDefinition, GoalStopAction,
            JobContext, JobPermission, JobRole, JobRule, JobWorkspace,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        scope.runner = crate::runner::DirectWindowsRunner::IDENTITY.into();
        let database = scope.checkout.join("store.sqlite");
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let definition = GoalDefinition {
            objective: "Verify controlled interruption".into(),
            criteria: Vec::new(),
            max_turns: Some(2),
            max_seconds: None,
            token_budget: None,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope.clone(),
                None,
                definition,
                None,
            )
            .await?;
        let project = goal.job.project_id;
        assert!(
            store
                .request_goal_stop("foreign".into(), goal.id.clone(), GoalStopAction::Pause)
                .await
                .is_err()
        );
        assert_eq!(
            store
                .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
                .await?,
            GoalControlState::Paused
        );
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        store.resume_goal(project.clone(), goal.id.clone()).await?;
        let initial = store
            .claim_next_job(env.clone())
            .await?
            .context("Resumed goal lacked a queued attempt")?;
        store.mark_agent_running(initial.run.id.clone()).await?;
        let revokes_authority = matches!(
            scenario,
            GoalStopScenario::AuthorityRevokedWhileRunning
                | GoalStopScenario::AuthorityRevokedBeforeSettlement
        );
        let confirmed_stop = revokes_authority || scenario == GoalStopScenario::ConfirmedPause;
        let denied = vec![JobRule {
            role: JobRole::Operator,
            operation: initial.scope.mode,
            workspace: JobWorkspace::Project,
            permission: JobPermission::Deny,
            requirements: Default::default(),
        }];
        if revokes_authority {
            assert!(
                !store
                    .pause_goal_if_policy_changed(initial.run.id.clone())
                    .await?
            );
            store
                .set_project_job_rules(project.clone(), Vec::new(), denied.clone())
                .await?;
            if scenario == GoalStopScenario::AuthorityRevokedWhileRunning {
                assert!(
                    store
                        .pause_goal_if_policy_changed(initial.run.id.clone())
                        .await?
                );
                assert!(
                    !store
                        .pause_goal_if_policy_changed(initial.run.id.clone())
                        .await?
                );
                assert!(
                    store
                        .goal_pause_requests_input_retention(initial.run.id.clone())
                        .await?
                );
            }
        }
        if matches!(
            scenario,
            GoalStopScenario::ConfirmedPause | GoalStopScenario::RestartDuringPause
        ) {
            assert_eq!(
                store
                    .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
                    .await?,
                GoalControlState::Pausing
            );
            assert!(
                store
                    .goal_pause_requests_input_retention(initial.run.id.clone())
                    .await?
            );
        }
        assert!(
            store
                .resume_goal(project.clone(), goal.id.clone())
                .await
                .is_err()
        );
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        if confirmed_stop {
            let before_settlement = scenario == GoalStopScenario::AuthorityRevokedBeforeSettlement;
            let outcome = store
                .finish_observed_agent_attempt(
                    initial.run.id.clone(),
                    if before_settlement {
                        RunStatus::Succeeded
                    } else {
                        RunStatus::Interrupted
                    },
                    None,
                    None,
                    true,
                )
                .await?;
            assert_eq!(outcome, RunStatus::Interrupted);
            if before_settlement {
                let failure = store
                    .job_failure(project.clone(), initial.run.id.clone())
                    .await?
                    .context("Missing authorization failure")?;
                assert_eq!(
                    failure.observation.cause,
                    Some(FailureCause::GoalAuthorityRevoked)
                );
            }
        }
        drop(store);
        let store = Store::open(database)?;
        store.recover_agent_attempts_after_restart().await?;
        let persisted = store
            .active_conversation_goal(conversation.id.clone())
            .await?
            .unwrap();
        assert_eq!(persisted.id, goal.id);
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        if !confirmed_stop {
            assert_eq!(persisted.control_state, GoalControlState::NeedsReview);
            if scenario == GoalStopScenario::CancelAfterRestart {
                use crate::jobs::QueueHistoryPolicy;
                store
                    .set_conversation_queue_history_policy(
                        conversation.id.clone(),
                        Some(QueueHistoryPolicy::Cancel),
                    )
                    .await?;
                let current = store.conversation(conversation.id.clone()).await?;
                store
                    .rewind_conversation_history(conversation.id.clone(), current.context_id, None)
                    .await?;
                assert_eq!(
                    store
                        .active_conversation_goal(conversation.id.clone())
                        .await?
                        .unwrap()
                        .control_state,
                    GoalControlState::NeedsReview
                );
                assert!(store.claim_next_job(env).await?.is_none());
                assert_eq!(
                    store
                        .confirm_managed_goal_stop(project, goal.id, initial.run.id)
                        .await?,
                    GoalControlState::Cancelled
                );
                assert!(
                    store
                        .active_conversation_goal(conversation.id)
                        .await?
                        .is_none()
                );
                return Ok(());
            }
            assert!(
                store
                    .resume_goal(project.clone(), goal.id.clone())
                    .await
                    .is_err()
            );
            assert!(
                store
                    .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Cancel)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .confirm_managed_goal_stop(project.clone(), goal.id.clone(), "other-run".into())
                    .await
                    .is_err()
            );
            assert_eq!(
                store
                    .confirm_managed_goal_stop(
                        project.clone(),
                        goal.id.clone(),
                        initial.run.id.clone()
                    )
                    .await?,
                GoalControlState::Paused
            );
        } else {
            assert_eq!(persisted.control_state, GoalControlState::Paused);
        }
        if revokes_authority {
            assert!(persisted.checkpoint.is_none());
            store
                .set_project_job_rules(project.clone(), denied, Vec::new())
                .await?;
        }
        store.resume_goal(project.clone(), goal.id.clone()).await?;
        let resumed = store
            .claim_next_job(env.clone())
            .await?
            .context("The paused checkpoint did not resume")?;
        assert_eq!(
            resumed.previous_run_id.as_deref(),
            Some(initial.run.id.as_str())
        );
        assert_ne!(resumed.run.id, initial.run.id);
        store.mark_agent_running(resumed.run.id.clone()).await?;
        assert_eq!(
            store
                .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Cancel)
                .await?,
            GoalControlState::Cancelling
        );
        assert!(
            store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .is_some()
        );
        store
            .finish_observed_agent_attempt(resumed.run.id, RunStatus::Cancelled, None, None, true)
            .await?;
        assert!(
            store
                .active_conversation_goal(conversation.id)
                .await?
                .is_none()
        );
        assert_eq!(
            store
                .request_goal_stop(project, goal.id, GoalStopAction::Cancel)
                .await?,
            GoalControlState::Cancelled
        );
        assert!(store.claim_next_job(env).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn goal_revision_keeps_its_selected_scope_and_checkpoint_after_restart() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalContinuationOwner, GoalDefinition,
            GoalTurnAction, GoalTurnReport, JobContext,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let context = JobContext {
            conversation_id: conversation.id.clone(),
            context_id: conversation.context_id,
        };
        let definition = GoalDefinition {
            objective: "Inspect the migration plan".into(),
            criteria: vec!["Identify the last compatible schema".into()],
            max_turns: Some(2),
            max_seconds: Some(90),
            token_budget: None,
        };
        let goal = store
            .start_conversation_goal(
                context.clone(),
                scope.clone(),
                None,
                definition.clone(),
                None,
            )
            .await?;
        assert!(
            store
                .start_conversation_goal(
                    context.clone(),
                    scope.clone(),
                    None,
                    definition.clone(),
                    None,
                )
                .await
                .is_err()
        );
        drop(store);
        let store = Store::open(database.clone())?;
        let persisted = store
            .active_conversation_goal(conversation.id.clone())
            .await?
            .context("The active goal was lost")?;
        assert_eq!(persisted, goal);
        assert_eq!(persisted.continuation_owner, GoalContinuationOwner::Managed);
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let plan = store
            .claim_next_job(environment.clone())
            .await?
            .context("The goal was not scheduled")?;
        assert!(plan.scope == scope);
        assert_eq!(plan.conversation_id, conversation.id);
        assert!(
            plan.input
                .last()
                .unwrap()
                .text
                .contains(&definition.criteria[0])
        );
        store.mark_agent_running(plan.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &plan).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let report = GoalTurnReport {
            evidence: "Found the last compatible schema. Verify the upgrade path.".into(),
            action: GoalTurnAction::Continue,
        };
        store
            .record_goal_turn_report(plan.run.id.clone(), report.clone())
            .await?;
        store
            .record_goal_turn_report(plan.run.id.clone(), report.clone())
            .await?;
        assert!(
            store
                .record_goal_turn_report(
                    plan.run.id.clone(),
                    GoalTurnReport {
                        action: GoalTurnAction::Review,
                        ..report.clone()
                    }
                )
                .await
                .is_err()
        );
        let captured = store.conversation(conversation.id.clone()).await?;
        store
            .save_conversation_draft(conversation.id.clone(), "Check both migrations".into())
            .await?;
        let instruction = store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: captured.context_id,
                },
                scope.clone(),
                "Check both migrations".into(),
            )
            .await?;
        store
            .record_agent_observation(
                plan.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some("goal-session".into()),
                        ..Default::default()
                    },
                    data: ProviderEventData::TurnFinished {
                        outcome: TurnOutcome::Completed,
                    },
                },
            )
            .await?;
        store
            .finish_observed_agent_attempt(
                plan.run.id.clone(),
                RunStatus::Succeeded,
                Some("Schema 38".into()),
                None,
                true,
            )
            .await?;
        drop(store);
        let store = Store::open(database.clone())?;
        store.recover_agent_attempts_after_restart().await?;
        let persisted = store
            .active_conversation_goal(conversation.id.clone())
            .await?
            .context("The goal revision was lost")?;
        let checkpoint = persisted
            .checkpoint
            .context("The goal checkpoint was lost")?;
        assert_eq!(checkpoint.ordinal, 1);
        assert_eq!(checkpoint.run_id, plan.run.id);
        assert_eq!(checkpoint.report, Some(report.clone()));
        assert_eq!(checkpoint.next_action, "continue");
        assert_eq!(
            checkpoint.context_id,
            store
                .conversation(conversation.id.clone())
                .await?
                .context_id
        );
        assert_eq!(
            persisted.provider_session_id.as_deref(),
            Some("goal-session")
        );
        let waiting = store
            .inspect_job(instruction.project_id, instruction.id, environment.clone())
            .await?
            .context("The pending instruction was lost")?;
        assert_eq!(
            waiting.blockers,
            vec![AdmissionBlocker::ContinuationOwner {
                run_id: plan.run.id.clone()
            }]
        );
        let replay_run = plan.run.id.clone();
        let replay_context = checkpoint.context_id.clone();
        let goal_revision = persisted.id.clone();
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for _ in 0..2 {
                crate::jobs::accept_goal_checkpoint(&tx, &replay_run, &replay_context)?;
            }
            let checkpoints: u32 = tx.query_row(
                "SELECT count(*) FROM goal_checkpoints WHERE revision_id = ?1",
                [&goal_revision], |row| row.get(0),
            )?;
            let continuations: Vec<u32> = tx.prepare(
                "SELECT checkpoint_ordinal FROM goal_continuations WHERE revision_id = ?1 ORDER BY checkpoint_ordinal",
            )?.query_map([&goal_revision], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            assert_eq!(checkpoints, 1);
            assert_eq!(continuations, [0, 1]);
            tx.commit()?;
            Ok(())
        }).await?;
        let second_scheduler = Store::open(database)?;
        let (first, second) = tokio::try_join!(
            store.claim_next_job(environment.clone()),
            second_scheduler.claim_next_job(environment.clone()),
        )?;
        let next = match (first, second) {
            (Some(attempt), None) | (None, Some(attempt)) => attempt,
            _ => anyhow::bail!("Expected exactly one scheduler to claim the continuation"),
        };
        assert_eq!(next.previous_run_id.as_deref(), Some(plan.run.id.as_str()));
        assert_eq!(next.input.last().unwrap().text, "Check both migrations");
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        let late = GoalTurnReport {
            evidence: "Late report for the first attempt".into(),
            action: GoalTurnAction::Blocked,
        };
        for _ in 0..2 {
            assert!(
                !store
                    .record_goal_turn_report(plan.run.id.clone(), late.clone())
                    .await?
            );
        }
        assert_eq!(
            store
                .archived_goal_reports(plan.run.project_id.clone(), plan.run.id.clone())
                .await?,
            [late]
        );
        assert!(
            store
                .archived_goal_reports("another-project".into(), plan.run.id.clone())
                .await?
                .is_empty()
        );
        let original = store
            .active_conversation_goal(conversation.id.clone())
            .await?
            .and_then(|goal| goal.checkpoint)
            .context("the late report replaced the accepted checkpoint")?;
        assert_eq!(original.next_action, "continue");
        store.mark_agent_running(next.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &next).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        store
            .record_goal_turn_report(next.run.id.clone(), report)
            .await?;
        store
            .record_agent_observation(
                next.run.id.clone(),
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
            .finish_observed_agent_attempt(
                next.run.id.clone(),
                RunStatus::Succeeded,
                Some("Verified".into()),
                None,
                true,
            )
            .await?;
        let finished = store
            .active_conversation_goal(conversation.id)
            .await?
            .unwrap()
            .checkpoint
            .unwrap();
        assert_eq!(finished.ordinal, 2);
        assert_eq!(finished.next_action, "limit_reached");
        assert!(store.claim_next_job(environment).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn stage_goal_keeps_its_task_and_matches_native_evidence_to_its_session() -> Result<()> {
        exercise_native_goal_evidence(false, true).await?;
        exercise_native_goal_evidence(true, false).await?;
        exercise_native_goal_evidence(true, true).await
    }

    async fn exercise_native_goal_evidence(
        current_receipt: bool,
        quiescence_confirmed: bool,
    ) -> Result<()> {
        use crate::{
            jobs::{
                AdmissionEnvironment, GoalContinuationOwner, GoalControlState, GoalDefinition,
                GoalStopAction, JobContext, PipelineDispatch, PipelineNodeKind,
            },
            providers::goal::{
                NativeGoalAction, NativeGoalControlOutcome, NativeGoalObservation, NativeGoalState,
            },
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let project = store
            .execute({
                let checkout = conversation.checkout_id.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT project_id FROM checkouts WHERE id = ?1",
                        [checkout],
                        |row| row.get::<_, String>(0),
                    )?)
                }
            })
            .await?;
        let task = store
            .create_project_task(project.clone(), "Goal fixture task".into())
            .await?;
        let stage = PipelineDispatch {
            pipeline_run_id: "goal-pipeline".into(),
            node_instance_id: "verification".into(),
            plan_revision: Some("accepted".into()),
            release_id: None,
        };
        store
            .register_job_pipeline(project.clone(), stage.pipeline_run_id.clone())
            .await?;
        store
            .register_job_pipeline_node(
                project.clone(),
                stage.pipeline_run_id.clone(),
                stage.node_instance_id.clone(),
                PipelineNodeKind::Execution,
            )
            .await?;
        store
            .set_job_pipeline_plan(
                project.clone(),
                stage.pipeline_run_id.clone(),
                None,
                "accepted".into(),
            )
            .await?;
        let goal = store
            .start_pipeline_goal(
                stage.clone(),
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope.clone(),
                Some(task.id.clone()),
                GoalDefinition {
                    objective: "Verify the accepted plan".into(),
                    criteria: vec!["Collect fixture evidence".into()],
                    max_turns: Some(1),
                    max_seconds: None,
                    token_budget: None,
                },
            )
            .await?;
        assert!(
            store
                .active_conversation_goal(conversation.id)
                .await?
                .is_none()
        );
        assert!(
            store
                .active_pipeline_goal(
                    "foreign".into(),
                    stage.pipeline_run_id.clone(),
                    stage.node_instance_id.clone()
                )
                .await?
                .is_none()
        );
        let run = store
            .claim_next_job(AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: true,
            })
            .await?
            .context("Stage goal did not use the scheduler")?;
        assert!(run.scope == scope);
        // Simulate an admitted native owner while testing evidence storage independently of the provider.
        store
            .execute({
                let revision = goal.id.clone();
                move |db| {
                    db.execute(
                        "UPDATE goal_revisions SET continuation_owner = 'native' WHERE id = ?1",
                        [revision],
                    )?;
                    Ok(())
                }
            })
            .await?;
        let observed = |reported_thread: &str| ProviderEvent {
            location: ProviderLocation {
                session_id: Some("native-session".into()),
                ..Default::default()
            },
            data: ProviderEventData::GoalObserved {
                observation: NativeGoalObservation {
                    provider_session_id: reported_thread.into(),
                    provider_goal_id: Some("provider-goal".into()),
                    provider_turn_id: None,
                    state: NativeGoalState::Active,
                    reported_state: Some("active".into()),
                    tokens_used: None,
                    token_budget: None,
                    elapsed_seconds: None,
                    evaluation: None,
                },
            },
        };
        store
            .record_agent_observation(
                run.run.id.clone(),
                EventSource::Provider,
                observed("foreign"),
            )
            .await?;
        assert!(
            store
                .active_pipeline_goal(
                    project.clone(),
                    stage.pipeline_run_id.clone(),
                    stage.node_instance_id.clone()
                )
                .await?
                .unwrap()
                .provider_goal_id
                .is_none()
        );
        store
            .record_agent_observation(
                run.run.id.clone(),
                EventSource::Provider,
                observed("native-session"),
            )
            .await?;
        let turn = |id: &str, data: ProviderEventData| {
            store.record_agent_observation(
                run.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some("native-session".into()),
                        turn_id: Some(id.into()),
                        ..Default::default()
                    },
                    data,
                },
            )
        };
        let completed = || ProviderEventData::TurnFinished {
            outcome: TurnOutcome::Completed,
        };
        turn("provider-1", ProviderEventData::TurnStarted).await?;
        assert!(
            turn("provider-2", ProviderEventData::TurnStarted)
                .await
                .is_err()
        );
        turn("provider-1", completed()).await?;
        turn("provider-2", ProviderEventData::TurnStarted).await?;
        turn("provider-1", ProviderEventData::TurnStarted).await?;
        turn("provider-1", completed()).await?;
        assert!(
            turn(
                "provider-1",
                ProviderEventData::AssistantDelta {
                    text: "Stale output".into()
                }
            )
            .await
            .is_err()
        );
        turn(
            "provider-2",
            ProviderEventData::AssistantDelta {
                text: "Current output".into(),
            },
        )
        .await?;
        turn("provider-2", completed()).await?;
        let control = |source, data| {
            store.record_agent_observation(
                run.run.id.clone(),
                source,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some("native-session".into()),
                        ..Default::default()
                    },
                    data,
                },
            )
        };
        let unconfirmed = |id, action| ProviderEventData::NativeGoalControlSettled {
            request_id: RequestId::Number(id),
            action,
            result: NativeGoalControlOutcome::Unconfirmed,
        };
        assert!(
            control(
                EventSource::Supervisor,
                unconfirmed(17, NativeGoalAction::Pause)
            )
            .await
            .is_err()
        );
        control(EventSource::Application, ProviderEventData::NativeGoalControlPrepared {
            request_id: RequestId::Number(17),
            action: NativeGoalAction::Pause,
            frame: br#"{"id":17,"method":"thread/goal/set","params":{"threadId":"native-session","status":"paused"}}"#.to_vec(),
        }).await?;
        assert!(
            control(
                EventSource::Supervisor,
                unconfirmed(17, NativeGoalAction::Cancel)
            )
            .await
            .is_err()
        );
        control(
            EventSource::Supervisor,
            unconfirmed(17, NativeGoalAction::Pause),
        )
        .await?;
        store.mark_agent_running(run.run.id.clone()).await?;
        let ProviderEventData::GoalObserved { mut observation } = observed("native-session").data
        else {
            unreachable!();
        };
        observation.state = NativeGoalState::Paused;
        observation.reported_state = Some("paused".into());
        let prepared_pause = |id| ProviderEventData::NativeGoalControlPrepared {
            request_id: RequestId::Number(id),
            action: NativeGoalAction::Pause,
            frame: serde_json::to_vec(&serde_json::json!({"id":id, "method":"thread/goal/set",
                "params":{"threadId":"native-session","status":"paused"}}))
            .unwrap(),
        };
        let confirmed_pause = |id| ProviderEventData::NativeGoalControlSettled {
            request_id: RequestId::Number(id),
            action: NativeGoalAction::Pause,
            result: NativeGoalControlOutcome::Confirmed {
                observation: observation.clone(),
            },
        };
        control(EventSource::Application, prepared_pause(18)).await?;
        control(EventSource::Provider, confirmed_pause(18)).await?;
        assert_eq!(
            store
                .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
                .await?,
            GoalControlState::Pausing
        );
        if current_receipt {
            control(EventSource::Application, prepared_pause(19)).await?;
            control(EventSource::Provider, confirmed_pause(19)).await?;
        }
        store
            .finish_observed_agent_attempt(
                run.run.id.clone(),
                RunStatus::Interrupted,
                None,
                None,
                quiescence_confirmed,
            )
            .await?;
        observation.provider_goal_id = Some("late-goal-report".into());
        let late_receipt = ProviderEventData::NativeGoalControlSettled {
            request_id: RequestId::Number(17),
            action: NativeGoalAction::Pause,
            result: NativeGoalControlOutcome::Confirmed { observation },
        };
        control(EventSource::Provider, late_receipt.clone()).await?;
        assert!(
            store
                .record_agent_observation(
                    run.run.id.clone(),
                    EventSource::Provider,
                    observed("foreign")
                )
                .await
                .is_err()
        );
        drop(store);
        let store = Store::open(database)?;
        let (control_outcome, persisted_late_receipt) = store
            .execute({
                let run_id = run.run.id.clone();
                move |db| {
                    let outcome: String = db.query_row(
                        "SELECT outcome FROM native_goal_controls WHERE run_id = ?1 AND request_key = '17'",
                        [&run_id],
                        |row| row.get(0),
                    )?;
                    let event: Vec<u8> = db.query_row(
                    "SELECT payload FROM agent_events WHERE run_id = ?1 ORDER BY seq DESC LIMIT 1",
                    [&run_id], |row| row.get(0),
                )?;
                    let saved: ProviderEvent =
                        serde_json::from_slice(&protection::unprotect(&event)?)?;
                    Ok((outcome, saved.data))
                }
            })
            .await?;
        assert_eq!(control_outcome, "unconfirmed");
        assert_eq!(persisted_late_receipt, late_receipt);
        let turns = store
            .execute({
                let run_id = run.run.id.clone();
                move |db| {
                    let mut statement = db.prepare(
                        "SELECT t.provider_turn_id, t.outcome, a.provider_turn_id
                     FROM native_goal_turns t JOIN agent_attempts a ON a.run_id = t.run_id
                     WHERE t.run_id = ?1 ORDER BY t.started_event_seq",
                    )?;
                    Ok(statement
                        .query_map([run_id], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?)
                }
            })
            .await?;
        assert_eq!(
            turns,
            [
                ("provider-1".into(), "completed".into(), "provider-2".into()),
                ("provider-2".into(), "completed".into(), "provider-2".into()),
            ]
        );
        let revision = store
            .active_pipeline_goal(project, stage.pipeline_run_id, stage.node_instance_id)
            .await?
            .context("Stage goal revision was lost")?;
        assert_eq!(revision.id, goal.id);
        assert_eq!(revision.job.task_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(revision.job.checkout_id, conversation.checkout_id);
        assert_eq!(revision.continuation_owner, GoalContinuationOwner::Native);
        assert_eq!(
            revision.provider_session_id.as_deref(),
            Some("native-session")
        );
        assert_eq!(revision.provider_goal_id.as_deref(), Some("provider-goal"));
        assert_eq!(
            revision.control_state,
            if current_receipt && quiescence_confirmed {
                GoalControlState::Paused
            } else {
                GoalControlState::NeedsReview
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn queued_goal_continuations_follow_history_policy_and_recheck_authority() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalDefinition, GoalTurnAction, GoalTurnReport,
            JobContext, JobPermission, JobRole, JobRule, JobWorkspace, QueueHistoryPolicy,
            QueueState,
        };

        for policy in [
            QueueHistoryPolicy::Recorded,
            QueueHistoryPolicy::Review,
            QueueHistoryPolicy::Cancel,
        ] {
            let (_dir, store, conversation, mut scope) = setup().await?;
            scope.executable = std::env::current_exe()?;
            let env = AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: true,
            };
            let goal = store
                .start_conversation_goal(
                    JobContext {
                        conversation_id: conversation.id.clone(),
                        context_id: conversation.context_id,
                    },
                    scope.clone(),
                    None,
                    GoalDefinition {
                        objective: "Retain the recorded scope".into(),
                        criteria: Vec::new(),
                        max_turns: Some(2),
                        max_seconds: None,
                        token_budget: None,
                    },
                    None,
                )
                .await?;
            let project = goal.job.project_id;
            let initial = store
                .claim_next_job(env.clone())
                .await?
                .context("Initial goal did not start")?;
            store.mark_agent_running(initial.run.id.clone()).await?;
            let dispatch = prepare_fixture_dispatch(&store, &initial).await?;
            store.settle_dispatch(dispatch, Delivery::Submitted).await?;
            store
                .record_goal_turn_report(
                    initial.run.id.clone(),
                    GoalTurnReport {
                        evidence: "Intermediate result".into(),
                        action: GoalTurnAction::Continue,
                    },
                )
                .await?;
            store
                .record_agent_observation(
                    initial.run.id.clone(),
                    EventSource::Provider,
                    ProviderEvent {
                        data: ProviderEventData::TurnFinished {
                            outcome: TurnOutcome::Completed,
                        },
                        location: ProviderLocation::default(),
                    },
                )
                .await?;
            store
                .finish_observed_agent_attempt(
                    initial.run.id.clone(),
                    RunStatus::Succeeded,
                    Some("Retained checkpoint content".into()),
                    None,
                    true,
                )
                .await?;
            let before = store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .unwrap();
            let checkpoint = before.checkpoint.unwrap();
            store
                .set_conversation_queue_history_policy(conversation.id.clone(), Some(policy))
                .await?;
            let cleared = store
                .rewind_conversation_history(
                    conversation.id.clone(),
                    checkpoint.context_id.clone(),
                    None,
                )
                .await?;
            let pending = store
                .job(project.clone(), before.job.id.clone())
                .await?
                .unwrap();
            if policy == QueueHistoryPolicy::Cancel {
                assert_eq!(pending.queue_state, QueueState::Cancelled);
                assert!(
                    store
                        .active_conversation_goal(conversation.id)
                        .await?
                        .is_none()
                );
                assert!(store.claim_next_job(env).await?.is_none());
                continue;
            }
            if policy == QueueHistoryPolicy::Review {
                assert_eq!(pending.queue_state, QueueState::Paused);
                assert!(store.claim_next_job(env.clone()).await?.is_none());
                store
                    .set_pending_job_state(project.clone(), pending.id.clone(), QueueState::Queued)
                    .await?;
            }
            let denied = JobRule {
                role: JobRole::Operator,
                operation: SessionMode::ReadOnly,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Deny,
                requirements: Default::default(),
            };
            store
                .set_project_job_rules(project.clone(), vec![], vec![denied.clone()])
                .await?;
            assert!(
                store
                    .inspect_job(project.clone(), pending.id.clone(), env.clone())
                    .await?
                    .unwrap()
                    .blockers
                    .iter()
                    .any(|blocker| matches!(blocker, AdmissionBlocker::Policy { .. }))
            );
            assert!(store.claim_next_job(env.clone()).await?.is_none());
            store
                .set_project_job_rules(project, vec![denied], vec![])
                .await?;
            let resumed = store
                .claim_next_job(env)
                .await?
                .context("Retained continuation did not start")?;
            assert_eq!(resumed.previous_run_id.as_ref(), Some(&initial.run.id));
            assert!(resumed.scope == scope);
            assert_eq!(resumed.run.checkout_id, conversation.checkout_id);
            assert!(matches!(resumed.intent, SessionIntent::Start));
            assert!(
                resumed
                    .input
                    .iter()
                    .any(|message| message.text == "Retained checkpoint content")
            );
            assert_eq!(
                store.conversation(conversation.id).await?.context_id,
                cleared
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn paused_goal_retains_its_sealed_context_across_clear_until_explicit_resume()
    -> Result<()> {
        use crate::jobs::{
            AdmissionEnvironment, GoalControlState, GoalDefinition, GoalStopAction, JobContext,
            QueueHistoryPolicy,
        };
        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let goal = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope.clone(),
                None,
                GoalDefinition {
                    objective: "Continue only the recorded history".into(),
                    criteria: Vec::new(),
                    max_turns: Some(2),
                    max_seconds: None,
                    token_budget: None,
                },
                None,
            )
            .await?;
        let project = goal.job.project_id;
        let first = store.claim_next_job(env.clone()).await?.unwrap();
        store.mark_agent_running(first.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &first).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        assert_eq!(
            store
                .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
                .await?,
            GoalControlState::Pausing
        );
        store
            .finish_observed_agent_attempt(
                first.run.id.clone(),
                RunStatus::Interrupted,
                Some("Paused partial progress".into()),
                None,
                true,
            )
            .await?;
        let before = store.conversation(conversation.id.clone()).await?;
        store
            .set_conversation_queue_history_policy(
                conversation.id.clone(),
                Some(QueueHistoryPolicy::Recorded),
            )
            .await?;
        let cleared = store
            .rewind_conversation_history(conversation.id.clone(), before.context_id, None)
            .await?;
        assert_eq!(
            store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .unwrap()
                .control_state,
            GoalControlState::Paused
        );
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        store.resume_goal(project, goal.id).await?;
        let resumed = store
            .claim_next_job(env)
            .await?
            .context("Missing parked goal recovery")?;
        assert_eq!(resumed.previous_run_id.as_ref(), Some(&first.run.id));
        assert!(resumed.scope == scope);
        assert!(matches!(resumed.intent, SessionIntent::Start));
        assert!(
            resumed
                .input
                .iter()
                .any(|message| message.text == "Paused partial progress")
        );
        assert_eq!(
            store.conversation(conversation.id).await?.context_id,
            cleared
        );
        Ok(())
    }

    #[tokio::test]
    async fn recorded_history_goal_can_collect_fresh_input_after_clear() -> Result<()> {
        use crate::jobs::{AdmissionEnvironment, GoalDefinition, JobContext, QueueHistoryPolicy};

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id.clone(),
                    context_id: conversation.context_id,
                },
                scope.clone(),
                None,
                GoalDefinition {
                    objective: "Ask what scope to retain".into(),
                    criteria: Vec::new(),
                    max_turns: Some(2),
                    max_seconds: None,
                    token_budget: None,
                },
                None,
            )
            .await?;
        let first = store.claim_next_job(env.clone()).await?.unwrap();
        store.mark_agent_running(first.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &first).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let schema = json!({"type":"object","properties":{"continue":{"type":"boolean"}},"required":["continue"]});
        let request_id = RequestId::Number(43);
        for data in [
            ProviderEventData::Question {
                request_id: request_id.clone(),
                form: json!({"message":"Continue in this scope?","schema":schema}),
                normalized: Some(QuestionForm::schema(
                    "Continue in this scope?".into(),
                    &schema,
                )?),
                route: QuestionRoute::Native,
            },
            ProviderEventData::QuestionClosed { request_id },
        ] {
            store
                .record_agent_observation(
                    first.run.id.clone(),
                    EventSource::Provider,
                    ProviderEvent {
                        data,
                        location: ProviderLocation::default(),
                    },
                )
                .await?;
        }
        store
            .finish_observed_agent_attempt(
                first.run.id.clone(),
                RunStatus::Interrupted,
                Some("The goal still needs input.".into()),
                None,
                true,
            )
            .await?;
        assert_eq!(
            store
                .saved_conversation_questions(conversation.id.clone(), first.run.id.clone())
                .await?
                .len(),
            1
        );
        assert!(store.claim_next_job(env.clone()).await?.is_none());
        store
            .set_conversation_queue_history_policy(
                conversation.id.clone(),
                Some(QueueHistoryPolicy::Recorded),
            )
            .await?;
        let before = store.conversation(conversation.id.clone()).await?;
        let cleared = store
            .rewind_conversation_history(conversation.id.clone(), before.context_id, None)
            .await?;
        assert!(
            store
                .saved_conversation_questions(conversation.id.clone(), first.run.id.clone())
                .await?
                .is_empty()
        );
        let resumed = store
            .claim_next_job(env)
            .await?
            .context("The retained goal did not receive a fresh request")?;
        assert_eq!(resumed.previous_run_id.as_ref(), Some(&first.run.id));
        assert!(resumed.scope == scope);
        assert!(matches!(resumed.intent, SessionIntent::Start));
        assert!(
            resumed
                .input
                .iter()
                .any(|message| message.text == "The goal still needs input.")
        );
        assert_eq!(
            store.conversation(conversation.id).await?.context_id,
            cleared
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelling_queued_goal_via_history_releases_its_owner() -> Result<()> {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, GoalDefinition, JobContext, QueueHistoryPolicy,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let context = JobContext {
            conversation_id: conversation.id.clone(),
            context_id: conversation.context_id,
        };
        let definition = GoalDefinition {
            objective: "Inspect the fixture".into(),
            criteria: Vec::new(),
            max_turns: Some(1),
            max_seconds: None,
            token_budget: Some(100),
        };
        store
            .set_conversation_queue_history_policy(
                conversation.id.clone(),
                Some(QueueHistoryPolicy::Cancel),
            )
            .await?;
        let goal = store
            .start_conversation_goal(
                context.clone(),
                scope.clone(),
                None,
                definition.clone(),
                None,
            )
            .await?;
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: true,
        };
        let inspection = store
            .inspect_job(goal.job.project_id, goal.job.id, environment.clone())
            .await?
            .unwrap();
        assert!(inspection.blockers.contains(&AdmissionBlocker::Policy {
            reason: "The managed goal route cannot enforce this token budget",
        }));
        assert!(store.claim_next_job(environment).await?.is_none());
        let next_context = store
            .rewind_conversation_history(conversation.id.clone(), context.context_id, None)
            .await?;
        assert!(
            store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .is_none()
        );
        let replacement = store
            .start_conversation_goal(
                JobContext {
                    conversation_id: conversation.id,
                    context_id: next_context,
                },
                scope,
                None,
                GoalDefinition {
                    token_budget: None,
                    ..definition
                },
                None,
            )
            .await?;
        assert_eq!(goal.goal_id, replacement.goal_id);
        assert_ne!(goal.id, replacement.id);
        Ok(())
    }

    #[tokio::test]
    async fn instruction_dispatch_requires_the_claimed_revision_and_scope() -> Result<()> {
        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let environment = crate::jobs::AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let text = "Only send the admitted revision";
        store
            .save_conversation_draft(conversation.id.clone(), text.into())
            .await?;
        store
            .enqueue_conversation_turn(
                crate::jobs::JobContext {
                    conversation_id: conversation.id,
                    context_id: conversation.context_id,
                },
                scope.clone(),
                text.into(),
            )
            .await?;
        let mut attempt = store
            .claim_next_job(environment)
            .await?
            .context("The instruction was not claimed")?;
        let correct = attempt
            .input
            .last()
            .context("The claim has no message")?
            .revision_id
            .clone();
        attempt.input.last_mut().unwrap().revision_id = "other-revision".into();
        assert!(prepare_fixture_dispatch(&store, &attempt).await.is_err());
        attempt.input.last_mut().unwrap().revision_id = correct;
        attempt.scope.model = Some("other-model".into());
        assert!(prepare_fixture_dispatch(&store, &attempt).await.is_err());
        attempt.scope = scope;
        prepare_fixture_dispatch(&store, &attempt).await?;
        Ok(())
    }

    #[tokio::test]
    async fn pipeline_receives_only_the_first_compatible_instruction_after_restart() -> Result<()> {
        exercise_pipeline_instruction_delivery(true, false).await
    }

    #[tokio::test]
    async fn unconfirmed_owner_delivery_holds_later_turns_on_its_context() -> Result<()> {
        exercise_pipeline_instruction_delivery(false, false).await
    }

    #[tokio::test]
    async fn stale_instruction_reference_does_not_stop_a_pipeline_continuation() -> Result<()> {
        exercise_pipeline_instruction_delivery(true, true).await
    }

    async fn exercise_pipeline_instruction_delivery(
        confirmed: bool,
        stale_reference: bool,
    ) -> Result<()> {
        use crate::attachments::{AttachmentDelivery, AttachmentSource};
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, InstructionDelivery, JobContext, JobRequest,
            PipelineDispatch, PipelineNodeKind, QueueState,
        };

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let project = store
            .execute({
                let checkout = conversation.checkout_id.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT project_id FROM checkouts WHERE id = ?1",
                        [checkout],
                        |row| row.get::<_, String>(0),
                    )?)
                }
            })
            .await?;
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        store.set_job_queue_capacity(1).await?;
        store
            .register_job_pipeline(project.clone(), "pipeline".into())
            .await?;
        store
            .set_job_pipeline_plan(project.clone(), "pipeline".into(), None, "v1".into())
            .await?;
        store
            .register_job_pipeline_node(
                project.clone(),
                "pipeline".into(),
                "owner".into(),
                PipelineNodeKind::Execution,
            )
            .await?;
        let request = |instructions: &str| JobRequest {
            checkout_id: Some(conversation.checkout_id.clone()),
            task_id: None,
            mode: SessionMode::ReadOnly,
            instructions: instructions.into(),
            dependencies: Vec::new(),
            requires_bridge: false,
        };
        let context = JobContext {
            conversation_id: conversation.id.clone(),
            context_id: conversation.context_id.clone(),
        };
        let binding = PipelineDispatch {
            pipeline_run_id: "pipeline".into(),
            node_instance_id: "owner".into(),
            plan_revision: Some("v1".into()),
            release_id: None,
        };
        let pipeline = if confirmed {
            store
                .submit_pinned_pipeline_agent_job(
                    binding,
                    scope.clone(),
                    request("Pipeline prompt"),
                    context.clone(),
                    QuestionRoute::Native,
                )
                .await?
        } else {
            store
                .submit_pipeline_agent_job(binding, scope.clone(), request("Pipeline prompt"))
                .await?
        };
        let first = store
            .claim_next_job(environment.clone())
            .await?
            .context("Missing pipeline run")?;
        let conversation = store.conversation(first.conversation_id.clone()).await?;
        let context = JobContext {
            conversation_id: conversation.id.clone(),
            context_id: conversation.context_id.clone(),
        };
        store.mark_agent_running(first.run.id.clone()).await?;
        let child = store
            .submit_agent_job(
                first.run.id.clone(),
                "child".into(),
                request("Child prompt"),
            )
            .await?;
        store
            .request_pipeline_downstream_wait(
                first.run.id.clone(),
                "wait".into(),
                vec![child.id.clone()],
            )
            .await?;
        let enqueue = |text: &'static str| {
            let store = &store;
            let scope = scope.clone();
            let context = context.clone();
            async move {
                store
                    .save_conversation_draft(context.conversation_id.clone(), text.into())
                    .await?;
                store
                    .enqueue_conversation_turn(context, scope, text.into())
                    .await
            }
        };
        let first_instruction = if stale_reference {
            let reference = scope.checkout.join("instruction.txt");
            std::fs::write(&reference, b"old reference")?;
            let preview = store
                .preview_attachment(
                    conversation.checkout_id.clone(),
                    AttachmentSource::Checkout {
                        relative_path: "instruction.txt".into(),
                    },
                    AttachmentDelivery::Reference,
                )
                .await?;
            store
                .save_conversation_draft(conversation.id.clone(), "Queued first".into())
                .await?;
            let queued = store
                .enqueue_conversation_turn_with_attachments(
                    context.clone(),
                    scope.clone(),
                    "Queued first".into(),
                    vec![preview],
                )
                .await?;
            std::fs::write(reference, b"new reference")?;
            queued
        } else {
            enqueue("Queued first").await?
        };
        let second_instruction = enqueue("Queued second").await?;
        let queue = store
            .pending_conversation_instructions(conversation.id.clone(), environment.clone())
            .await?;
        store
            .revise_pending_instruction(
                conversation.id.clone(),
                first_instruction.id.clone(),
                queue[0].revision_id.clone(),
                "Revised first".into(),
                None,
            )
            .await?;
        store
            .finish_observed_agent_attempt(
                first.run.id.clone(),
                RunStatus::Interrupted,
                Some("Yielded".into()),
                None,
                true,
            )
            .await?;
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store
            .set_pending_job_state(project.clone(), child.id, QueueState::Cancelled)
            .await?;
        let continuation = store
            .claim_next_job(environment.clone())
            .await?
            .context("Missing owner continuation")?;
        assert_eq!(continuation.previous_run_id.as_ref(), Some(&first.run.id));
        if stale_reference {
            assert!(
                continuation
                    .input
                    .iter()
                    .all(|message| message.text != "Revised first")
            );
            assert!(continuation.attachments.is_empty());
            store
                .mark_agent_running(continuation.run.id.clone())
                .await?;
            store
                .finish_agent_attempt(continuation.run.id, RunStatus::Succeeded, None)
                .await?;
            let pending = store
                .pending_conversation_instructions(conversation.id, environment.clone())
                .await?;
            assert_eq!(pending[0].id, first_instruction.id);
            assert!(pending[0].editable);
            assert!(pending[0].blockers.iter().any(|blocker| matches!(blocker, AdmissionBlocker::Policy { reason } if reason.contains("Preview it again"))));
            assert!(store.claim_next_job(environment).await?.is_none());
            return Ok(());
        }
        let instruction = continuation.input.last().context("Missing queued input")?;
        assert_eq!(instruction.text, "Revised first");
        assert!(
            continuation
                .input
                .iter()
                .all(|message| message.text != "Queued second")
        );
        let claimed_revision = instruction.revision_id.clone();
        let pending = store
            .pending_conversation_instructions(conversation.id.clone(), environment.clone())
            .await?;
        assert_eq!(pending[0].delivery, InstructionDelivery::Delivering);
        assert_eq!(pending[0].run_id.as_ref(), Some(&continuation.run.id));
        assert!(!pending[0].editable);
        assert!(
            store
                .set_pending_job_state(
                    project.clone(),
                    first_instruction.id.clone(),
                    QueueState::Cancelled
                )
                .await
                .is_err()
        );
        assert!(store.claim_next_job(environment.clone()).await?.is_none());

        drop(store);
        let store = Store::open(database.clone())?;
        store.recover_agent_attempts_after_restart().await?;
        let resumed = store
            .claim_next_job(environment.clone())
            .await?
            .context("Missing safe owner retry")?;
        assert_eq!(resumed.previous_run_id.as_ref(), Some(&continuation.run.id));
        assert_eq!(resumed.input.last().unwrap().revision_id, claimed_revision);
        assert_eq!(
            resumed
                .input
                .iter()
                .filter(|message| message.revision_id == claimed_revision)
                .count(),
            1
        );
        let (attempts, active_claims) = store.execute({
            let instruction = first_instruction.id.clone();
            move |db| Ok(db.query_row(
                "SELECT count(*), sum(superseded = 0) FROM instruction_delivery_attempts WHERE job_id = ?1",
                [instruction], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?)
        }).await?;
        assert_eq!((attempts, active_claims), (2, 1));
        store.mark_agent_running(resumed.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &resumed).await?;
        if !confirmed {
            drop(store);
            let store = Store::open(database)?;
            store.recover_agent_attempts_after_restart().await?;
            let evidence = store
                .agent_dispatch(conversation.id.clone(), resumed.run.id.clone())
                .await?
                .context("missing uncertain dispatch evidence")?;
            assert_eq!(
                evidence.instructions[0].mode,
                crate::dispatch::InstructionDispatchMode::Continuation
            );
            assert_eq!(
                evidence.instructions[0].status,
                crate::dispatch::InstructionDispatchStatus::Unconfirmed
            );
            assert_eq!(evidence.instructions[0].revision_id, claimed_revision);
            assert_eq!(evidence.instructions[0].text, "Revised first");
            let pending = store
                .pending_conversation_instructions(conversation.id.clone(), environment.clone())
                .await?;
            assert_eq!(pending[0].id, first_instruction.id);
            assert_eq!(pending[0].delivery, InstructionDelivery::NeedsReview);
            assert_eq!(pending[0].run_id.as_ref(), Some(&resumed.run.id));
            let later = store
                .inspect_job(project.clone(), second_instruction.id, environment.clone())
                .await?
                .context("Missing later queued instruction")?;
            assert!(
                later
                    .blockers
                    .contains(&AdmissionBlocker::InstructionDeliveryUncertain {
                        run_id: resumed.run.id.clone(),
                    })
            );
            let owner = store
                .inspect_job(project, pipeline.id, environment.clone())
                .await?
                .context("Missing owning job")?;
            assert!(owner.recovery_actions.is_empty());
            assert!(store.claim_next_job(environment).await?.is_none());
            return Ok(());
        }
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        let evidence = store
            .agent_dispatch(conversation.id.clone(), resumed.run.id.clone())
            .await?
            .context("missing continuation dispatch evidence")?;
        assert_eq!(
            evidence.instructions[0].mode,
            crate::dispatch::InstructionDispatchMode::Continuation
        );
        assert_eq!(
            evidence.instructions[0].status,
            crate::dispatch::InstructionDispatchStatus::Submitted
        );
        assert_eq!(evidence.instructions[0].revision_id, claimed_revision);
        assert_eq!(evidence.instructions[0].text, "Revised first");
        store
            .finish_agent_attempt(resumed.run.id.clone(), RunStatus::Succeeded, None)
            .await?;
        let delivered = store
            .job(project.clone(), first_instruction.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap();
        assert_eq!(delivered.run_id, resumed.run.id);
        let owner = store
            .job(project, pipeline.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap();
        assert_eq!(owner.run_id, delivered.run_id);
        let pending = store
            .pending_conversation_instructions(conversation.id, environment.clone())
            .await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, second_instruction.id);
        let next = store
            .claim_next_job(environment)
            .await?
            .context("Missing remaining instruction")?;
        assert_eq!(next.input.last().unwrap().text, "Queued second");
        assert!(
            next.input
                .iter()
                .all(|message| message.revision_id != claimed_revision)
        );
        Ok(())
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

    #[tokio::test]
    async fn side_requests_share_capacity_without_joining_main_history() -> Result<()> {
        use crate::{
            jobs::{
                AdmissionEnvironment, ComposerSuggestionMode, JobContext, JobScheduler,
                RecoveryAction, SidePurpose, SideSubmission,
            },
            providers::{
                capabilities::{
                    CapabilityAssessment, CapabilityDescriptor, CapabilityScope, CapabilityStatus,
                    ConfigurationEvidence,
                },
                side::SideOutcome,
            },
        };

        struct SideRunner {
            frames: Arc<Mutex<Vec<Vec<u8>>>>,
            output: Arc<Mutex<Vec<mpsc::Sender<io::Result<OutputFrame>>>>>,
            source_checkout: PathBuf,
        }
        struct SideWire(Arc<Mutex<Vec<Vec<u8>>>>);
        impl io::Write for SideWire {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().push(bytes.to_vec());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Runner for SideRunner {
            fn identity(&self) -> &str {
                "side-fixture"
            }
            fn executable_revision(&self, _: &Path) -> Option<String> {
                Some("side-fixture-build".into())
            }
            fn execution_platform(&self) -> Option<ExecutionPlatform> {
                DirectWindowsRunner.execution_platform()
            }
            fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
                assert_ne!(request.checkout, self.source_checkout);
                assert!(
                    request
                        .arguments
                        .windows(2)
                        .any(|args| args == ["--tools", ""].map(std::ffi::OsStr::new))
                );
                let (send, output) = mpsc::channel();
                for data in [
                    json!({"type":"control_response","response":{"request_id":"side-init","subtype":"success"}}),
                    json!({"type":"system","subtype":"init","session_id":"side-only","model":"side-selected","permissionMode":"plan","tools":[],"cwd":request.checkout}),
                ] {
                    send.send(Ok(OutputFrame {
                        stream: Stream::Stdout,
                        data: data.to_string(),
                    }))
                    .unwrap();
                }
                self.output.lock().unwrap().push(send);
                Ok(CapturedProcess {
                    input: ProcessInput::new(SideWire(self.frames.clone())),
                    output,
                    control: Arc::new(CompletedProcess),
                })
            }
        }

        let (_dir, store, chat, mut scope) = setup().await?;
        let database = scope.checkout.join("store.sqlite");
        let frames: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let outputs: Arc<Mutex<Vec<mpsc::Sender<io::Result<OutputFrame>>>>> = Arc::default();
        let runner = Arc::new(SideRunner {
            frames: frames.clone(),
            output: outputs.clone(),
            source_checkout: scope.checkout.clone(),
        });
        scope.runner = runner.identity().into();
        scope.provider = Provider::Claude;
        scope.executable = std::env::current_exe()?;
        scope.model = Some("side-selected".into());
        let completed =
            prepare(&store, &chat.id, scope.clone(), "Original permitted source").await?;
        let profile = CapabilityScope::from_dispatch(
            &scope,
            chat.checkout_id.clone(),
            runner.executable_revision(&scope.executable),
            runner.execution_platform(),
            ConfigurationEvidence {
                detected: vec![],
                unresolved: vec![],
                provider_revision: None,
                effective_revision: None,
                scoped_launch_revision: Some("tool-disabled-source".into()),
                environment_revision: None,
                tool_revisions: Default::default(),
            },
            scope.model.clone(),
            scope.effort.clone(),
        );
        let descriptor = CapabilityDescriptor::side_request(scope.provider);
        store.mark_agent_running(completed.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch_with_capabilities(
            &store,
            &completed,
            Some(CapabilityAssessment {
                version: crate::providers::capabilities::CAPABILITY_SCHEMA_VERSION,
                scope: profile.clone(),
                entries: vec![],
            }),
        )
        .await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        for data in [
            ProviderEventData::SessionReady {
                model: scope.model.clone(),
                effort: scope.effort.clone(),
            },
            ProviderEventData::TurnFinished {
                outcome: TurnOutcome::Completed,
            },
        ] {
            store
                .record_agent_observation(
                    completed.run.id.clone(),
                    EventSource::Provider,
                    ProviderEvent {
                        location: ProviderLocation {
                            session_id: Some("primary-thread".into()),
                            turn_id: Some("primary-turn".into()),
                            ..Default::default()
                        },
                        data,
                    },
                )
                .await?;
        }
        store
            .finish_agent_attempt(
                completed.run.id.clone(),
                RunStatus::Succeeded,
                Some("Committed primary answer".into()),
            )
            .await?;

        store.set_job_queue_capacity(2).await?;
        let current = store.conversation(chat.id.clone()).await?;
        let SideSubmission::Queued {
            job_id: validation_id,
        } = store
            .enqueue_side_request(
                chat.id.clone(),
                Some(current.context_id.clone()),
                None,
                scope.clone(),
                SidePurpose::Validation,
                "Ignored untrusted text".into(),
            )
            .await?
        else {
            anyhow::bail!("The fixture should be admitted without an existing proof");
        };
        assert!(matches!(
            store.enqueue_side_request(
                chat.id.clone(), Some(current.context_id.clone()), None, scope.clone(), SidePurpose::Validation, String::new(),
            ).await?, SideSubmission::Queued { job_id } if job_id == validation_id
        ));
        let code: String = store
            .execute({
                let id = validation_id.clone();
                move |db| {
                    let protected: Vec<u8> = db.query_row(
                        "SELECT payload FROM job_side_requests WHERE job_id = ?1 AND validation",
                        [id],
                        |row| row.get(0),
                    )?;
                    let body: Value = serde_json::from_slice(&protection::unprotect(&protected)?)?;
                    Ok(body["context"]
                        .as_str()
                        .context("missing fixture code")?
                        .to_owned())
                }
            })
            .await?;
        let fixture = JobScheduler::start(store.clone(), runner.clone(), None);
        let fixture_output = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(endpoint) = outputs.lock().unwrap().first().cloned() {
                    return endpoint;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?;
        fixture_output.send(Ok(OutputFrame {
            stream: Stream::Stdout,
                    data: json!({"type":"result","subtype":"success","is_error":false,"session_id":"side-only","result":code,"modelUsage":{"side-selected":{}}}).to_string(),
        }))?;
        let verified = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut records = store.saved_side_requests(chat.id.clone()).await?;
                if records[0].evidence_id.is_some() {
                    return Ok::<_, anyhow::Error>(records.remove(0));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        assert_eq!(verified.purpose, SidePurpose::Validation);
        let evidence = store
            .resolve_capability_assessment(profile, vec![descriptor])
            .await?
            .entries
            .remove(0);
        assert_eq!(evidence.status(), CapabilityStatus::Verified);
        let evidence = evidence.evidence.unwrap();
        assert_eq!(Some(evidence.id), verified.evidence_id);
        assert_eq!(
            evidence.artifact_id,
            verified.job.latest_attempt.unwrap().run_id
        );
        fixture.shutdown().await?;
        for wire in std::mem::take(&mut *frames.lock().unwrap()) {
            let wire = String::from_utf8(wire)?;
            assert!(!wire.contains("Original permitted source"));
            assert!(!wire.contains("Ignored untrusted text"));
        }
        outputs.lock().unwrap().clear();
        store
            .set_composer_suggestion_mode(ComposerSuggestionMode::Omniagent)
            .await?;
        let SideSubmission::Queued {
            job_id: suggestion_id,
        } = store
            .enqueue_side_request(
                chat.id.clone(),
                None,
                Some(completed.run.id),
                scope.clone(),
                SidePurpose::Suggestion,
                String::new(),
            )
            .await?
        else {
            anyhow::bail!("The current completed turn should qualify for a suggestion");
        };
        for question in [
            "side-private-cancel",
            "side-private-finish",
            "side-private-attempted-tool",
            "side-private-restart",
        ] {
            assert!(matches!(
                store
                    .enqueue_side_request(
                        chat.id.clone(),
                        Some(current.context_id.clone()),
                        None,
                        scope.clone(),
                        SidePurpose::Question,
                        question.into()
                    )
                    .await?,
                SideSubmission::Queued { .. }
            ));
        }
        store
            .save_conversation_draft(chat.id.clone(), "Main turn after the side pin".into())
            .await?;
        store.execute(move |db| {
            let state: (bool, String) = db.query_row(
                "SELECT s.discarded, j.queue_state FROM job_side_requests s JOIN jobs j ON j.id = s.job_id WHERE j.id = ?1",
                [&suggestion_id], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(state, (true, "cancelled".into()));
            Ok(())
        }).await?;
        store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: chat.id.clone(),
                    context_id: current.context_id,
                },
                scope,
                "Main turn after the side pin".into(),
            )
            .await?;
        let env = AdmissionEnvironment {
            runner: runner.identity().into(),
            bridge_available: false,
        };
        let main = store
            .claim_next_job(env.clone())
            .await?
            .context("Regular work should have priority")?;
        store.mark_agent_running(main.run.id.clone()).await?;
        assert_eq!(
            main.input.last().unwrap().text,
            "Main turn after the side pin"
        );
        let scheduler = JobScheduler::start(store.clone(), runner, None);
        let wait = |index: usize, status: RunStatus| {
            let (store, id) = (store.clone(), chat.id.clone());
            async move {
                tokio::time::timeout(Duration::from_secs(12), async {
                    loop {
                        let mut records = store.saved_side_requests(id.clone()).await?;
                        records.retain(|record| record.purpose == SidePurpose::Question);
                        if records[index]
                            .job
                            .latest_attempt
                            .as_ref()
                            .is_some_and(|a| a.status == status)
                        {
                            return Ok::<_, anyhow::Error>(records.remove(index));
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await?
            }
        };
        let first = wait(0, RunStatus::Running).await?;
        let insp = store
            .inspect_project_queue(first.job.project_id.clone(), env.clone())
            .await?;
        assert_eq!(insp.active, 2);
        assert!(
            store
                .saved_side_requests(chat.id.clone())
                .await?
                .into_iter()
                .filter(|record| record.purpose == SidePurpose::Question)
                .nth(1)
                .unwrap()
                .job
                .latest_attempt
                .is_none()
        );
        store
            .request_project_job_interrupt(
                first.job.project_id,
                first.job.id,
                first.job.latest_attempt.unwrap().run_id,
            )
            .await?;
        assert_eq!(
            wait(0, RunStatus::Cancelled)
                .await?
                .response
                .unwrap()
                .outcome,
            SideOutcome::Cancelled
        );
        let send = |index: usize, data: Value| {
            let outputs = outputs.clone();
            async move {
                let output = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(output) = outputs.lock().unwrap().get(index).cloned() {
                            return output;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await?;
                output.send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: data.to_string(),
                }))?;
                Ok::<_, anyhow::Error>(())
            }
        };
        wait(1, RunStatus::Running).await?;
        send(
            1,
            json!({"type":"result","subtype":"success","is_error":false,"session_id":"side-only","result":"protected-response-canary","modelUsage":{"side-selected":{}}}),
        ).await?;
        let second = wait(1, RunStatus::Succeeded).await?;
        assert!(
            matches!(second.response.unwrap().outcome, SideOutcome::Answered { ref text, .. } if text == "protected-response-canary")
        );
        wait(2, RunStatus::Running).await?;
        send(
            2,
            json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read"}]}}),
        )
        .await?;
        assert_eq!(
            wait(2, RunStatus::Failed).await?.response.unwrap().outcome,
            SideOutcome::Failed
        );
        assert_eq!(
            store
                .latest_conversation_run(chat.id.clone())
                .await?
                .unwrap()
                .id,
            main.run.id
        );
        assert!(
            !format!(
                "{:?}",
                store.conversation_history(chat.id.clone()).await?.messages
            )
            .contains("side-private")
        );
        let sent = frames
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<Vec<_>>();
        assert!(
            sent.iter()
                .any(|frame| frame.contains("Original permitted source")
                    && frame.contains("side-private-finish")
                    && !frame.contains("Main turn after"))
        );
        let interrupted = wait(3, RunStatus::Running).await?;
        let checkpoint_path = database.with_file_name("side-recovery.sqlite");
        store.execute({
            let path = checkpoint_path.to_string_lossy().into_owned();
            move |db| {
                let (writer_count, indexed_count): (i64, i64) = db.query_row(
                    "SELECT (SELECT COUNT(*) FROM job_writer_reservations w JOIN job_attempts a ON a.run_id = w.run_id JOIN job_side_requests s ON s.job_id = a.job_id),
                         (SELECT COUNT(*) FROM agent_attempts p JOIN job_attempts a ON a.run_id = p.run_id JOIN job_side_requests s ON s.job_id = a.job_id)",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!((writer_count, indexed_count), (0, 0));
                for (table, canary) in [
                    ("job_side_requests", "side-private-finish"),
                    ("job_side_responses", "protected-response-canary"),
                ] {
                    let count: i64 = db.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE instr(payload, CAST(?1 AS BLOB)) > 0"),
                        [canary], |row| row.get(0),
                    )?;
                    assert_eq!(count, 0);
                }
                db.execute("VACUUM INTO ?1", [path])?;
                Ok(())
            }
        }).await?;
        let checkpoint = Store::open(checkpoint_path)?;
        checkpoint.recover_agent_attempts_after_restart().await?;
        let jobs = checkpoint
            .inspect_project_queue(interrupted.job.project_id, env)
            .await?;
        let recovered = jobs
            .jobs
            .iter()
            .find(|job| job.job.id == interrupted.job.id)
            .unwrap();
        assert_eq!(
            recovered.job.latest_attempt.as_ref().unwrap().status,
            RunStatus::Interrupted
        );
        assert_eq!(recovered.recovery_actions, [RecoveryAction::Retry]);
        assert!(recovered.job.queued_recovery.is_none());
        assert_eq!(
            checkpoint
                .saved_side_requests(chat.id.clone())
                .await?
                .into_iter()
                .filter(|record| record.purpose == SidePurpose::Question)
                .nth(3)
                .unwrap()
                .response
                .as_ref()
                .unwrap()
                .outcome,
            SideOutcome::Interrupted
        );
        scheduler.shutdown().await?;
        store
            .finish_agent_attempt(main.run.id, RunStatus::Succeeded, None)
            .await?;
        drop(store);
        let reopened = Store::open(database)?;
        reopened.recover_agent_attempts_after_restart().await?;
        assert!(
            matches!(reopened.saved_side_requests(chat.id.clone()).await?.into_iter().filter(|record| record.purpose == SidePurpose::Question).nth(1).unwrap().response.as_ref().unwrap().outcome, SideOutcome::Answered { ref text, .. } if text == "protected-response-canary")
        );
        assert!(
            !format!(
                "{:?}",
                reopened.conversation_history(chat.id).await?.messages
            )
            .contains("protected-response-canary")
        );
        Ok(())
    }

    #[tokio::test]
    async fn queued_instruction_claim_and_restart_submit_one_accepted_revision_to_the_cli()
    -> Result<()> {
        use crate::jobs::{AdmissionEnvironment, JobContext};

        let (_directory, store, chat, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let active = prepare(&store, &chat.id, scope.clone(), "Already running").await?;
        store.mark_agent_running(active.run.id.clone()).await?;
        let enqueue = |text: &'static str| {
            let (store, conversation_id, context_id, scope) = (
                store.clone(),
                chat.id.clone(),
                active.context_id.clone(),
                scope.clone(),
            );
            async move {
                store
                    .save_conversation_draft(conversation_id.clone(), text.into())
                    .await?;
                store
                    .enqueue_conversation_turn(
                        JobContext {
                            conversation_id,
                            context_id,
                        },
                        scope,
                        text.into(),
                    )
                    .await
            }
        };
        let first = enqueue("superseded-draft").await?;
        let removed = enqueue("withdrawn-draft").await?;
        let queue = store
            .pending_conversation_instructions(chat.id.clone(), environment.clone())
            .await?;
        store
            .revise_pending_instruction(
                chat.id.clone(),
                first.id.clone(),
                queue[0].revision_id.clone(),
                "ready-instruction".into(),
                None,
            )
            .await?;
        store
            .remove_pending_instruction(chat.id.clone(), removed.id, queue[1].revision_id.clone())
            .await?;
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store
            .finish_agent_attempt(
                active.run.id,
                RunStatus::Succeeded,
                Some("Previous turn settled".into()),
            )
            .await?;

        let pending = store
            .pending_conversation_instructions(chat.id.clone(), environment.clone())
            .await?;
        let other = Store::open(database.clone())?;
        let (claim, edit) = tokio::join!(
            store.claim_next_job(environment.clone()),
            other.revise_pending_instruction(
                chat.id.clone(),
                first.id,
                pending[0].revision_id.clone(),
                "racing-instruction".into(),
                None
            ),
        );
        let plan = claim?.context("Missing instruction claim")?;
        let accepted = plan.input.last().context("Missing claimed revision")?;
        assert_eq!(
            accepted.text,
            if edit.is_ok() {
                "racing-instruction"
            } else {
                "ready-instruction"
            }
        );
        let accepted_text = accepted.text.clone();
        let accepted_id = accepted.revision_id.clone();
        drop(other);
        drop(store);

        let store = Store::open(database)?;
        store.recover_agent_attempts_after_restart().await?;
        let replay = store
            .claim_next_job(environment.clone())
            .await?
            .context("Missing recovered claim")?;
        assert_eq!(replay.previous_run_id.as_ref(), Some(&plan.run.id));
        let run = replay.run.id.clone();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: chat.id.clone(),
            frames: frames.clone(),
        }));
        assert_eq!(
            start_managed_turn(store.clone(), runner, replay, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let dispatch = store
            .agent_dispatch(chat.id.clone(), run)
            .await?
            .context("Missing final wire evidence")?;
        assert_eq!(dispatch.delivery, Delivery::Submitted);
        assert_eq!(
            dispatch
                .snapshot
                .submitted_messages
                .iter()
                .filter(|message| message.revision_id == accepted_id)
                .count(),
            1
        );
        let sent = frames
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| serde_json::from_slice::<Value>(bytes))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let turn_inputs = sent
            .iter()
            .filter(|frame| frame["method"] == "turn/start")
            .collect::<Vec<_>>();
        assert_eq!(turn_inputs.len(), 1);
        let prompt = turn_inputs[0]["params"]["input"][0]["text"]
            .as_str()
            .context("The CLI didn't receive the prompt")?;
        assert_eq!(prompt.matches(accepted_text.as_str()).count(), 1);
        for rejected in [
            "superseded-draft",
            "withdrawn-draft",
            if edit.is_ok() {
                "ready-instruction"
            } else {
                "racing-instruction"
            },
        ] {
            assert!(!prompt.contains(rejected));
        }
        assert!(
            store
                .pending_conversation_instructions(chat.id, environment.clone())
                .await?
                .is_empty()
        );
        assert!(store.claim_next_job(environment).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn queue_recovers_an_ambiguous_dispatch_only_on_request_and_records_one_terminal_event()
    -> Result<()> {
        use crate::jobs::{AdmissionBlocker, AdmissionEnvironment, JobContext, RecoveryAction};

        let (_dir, store, chat, mut scope) = setup().await?;
        scope.executable = std::env::current_exe()?;
        let database = scope.checkout.join("store.sqlite");
        let environment = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let prompt = "Protect this queued input after a crash.";
        store
            .save_conversation_draft(chat.id.clone(), prompt.into())
            .await?;
        let job = store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: chat.id.clone(),
                    context_id: chat.context_id,
                },
                scope.clone(),
                prompt.into(),
            )
            .await?;
        let first = store
            .claim_next_job(environment.clone())
            .await?
            .context("initial claim missing")?;
        let lost_run = first.run.id.clone();
        assert!(store.record_agent_launch_intent(lost_run.clone()).await?);
        store.mark_agent_running(lost_run.clone()).await?;
        let dispatch_id = prepare_fixture_dispatch(&store, &first).await?;
        let before = store
            .agent_dispatch(chat.id.clone(), lost_run.clone())
            .await?
            .context("prepared evidence missing")?;
        assert_eq!(before.delivery, Delivery::Prepared);
        let current = store.conversation(chat.id.clone()).await?;
        let next_prompt = "Wait for the uncertain instruction to be reviewed.";
        store
            .save_conversation_draft(chat.id.clone(), next_prompt.into())
            .await?;
        let later = store
            .enqueue_conversation_turn(
                JobContext {
                    conversation_id: chat.id.clone(),
                    context_id: current.context_id,
                },
                scope,
                next_prompt.into(),
            )
            .await?;
        drop(store);

        let store = Store::open(database.clone())?;
        store.recover_agent_attempts_after_restart().await?;
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        let pending = store
            .inspect_job(later.project_id, later.id, environment.clone())
            .await?
            .unwrap();
        assert!(
            pending
                .blockers
                .contains(&AdmissionBlocker::InstructionDeliveryUncertain {
                    run_id: lost_run.clone()
                })
        );
        let interrupted = store
            .job(job.project_id.clone(), job.id.clone())
            .await?
            .unwrap();
        assert_eq!(
            interrupted.latest_attempt.unwrap().status,
            RunStatus::Interrupted
        );
        let saved = store
            .agent_dispatch(chat.id.clone(), lost_run.clone())
            .await?
            .context("dispatch evidence was lost")?;
        assert_eq!(saved.id, dispatch_id);
        assert_eq!(saved.delivery, Delivery::Unconfirmed);
        assert_eq!(saved.frames, before.frames);
        store
            .queue_job_recovery(
                job.project_id.clone(),
                job.id.clone(),
                lost_run.clone(),
                RecoveryAction::Resume,
            )
            .await?;
        let resumed = store
            .claim_next_job(environment.clone())
            .await?
            .context("explicit resume was not claimed")?;
        assert!(matches!(resumed.intent, SessionIntent::Start));
        assert_eq!(resumed.previous_run_id, Some(lost_run));
        assert_eq!(resumed.input[0].text, prompt);
        store.mark_agent_running(resumed.run.id.clone()).await?;
        let dispatch = prepare_fixture_dispatch(&store, &resumed).await?;
        store.settle_dispatch(dispatch, Delivery::Submitted).await?;
        store
            .record_agent_observation(
                resumed.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::TurnFinished {
                        outcome: TurnOutcome::Completed,
                    },
                },
            )
            .await?;

        let canceller = Store::open(database.clone())?;
        let (cancel, complete) = tokio::join!(
            canceller.request_job_interrupt(chat.id.clone(), resumed.run.id.clone()),
            store.finish_agent_attempt(
                resumed.run.id.clone(),
                RunStatus::Succeeded,
                Some("Confirmed answer".into())
            ),
        );
        complete?;
        if let Err(error) = cancel {
            assert!(error.to_string().contains("no longer active"));
        }
        assert!(
            canceller
                .finish_agent_attempt(resumed.run.id.clone(), RunStatus::Cancelled, None)
                .await
                .is_err()
        );
        let terminal = store
            .job(job.project_id, job.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap();
        assert_eq!(terminal.status, RunStatus::Succeeded);
        assert!(!terminal.cancel_requested);
        let db = rusqlite::Connection::open(database)?;
        let statuses = db.prepare("SELECT json_extract(payload, '$.status') FROM run_events WHERE turn_id = ?1 ORDER BY seq")?
            .query_map([resumed.run.turn_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(statuses, ["starting", "running", "succeeded"]);
        let following = store
            .claim_next_job(environment)
            .await?
            .context("confirmed recovery did not release the next instruction")?;
        assert_eq!(following.input.last().unwrap().text, next_prompt);
        store
            .finish_agent_attempt(following.run.id, RunStatus::Cancelled, None)
            .await?;
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
        fn confirm_quiescence(&self) -> io::Result<()> {
            Ok(())
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
                    if let Some(path) = attachment.input.path() {
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
        fn attachment_staging(
            &self,
            scope: &crate::runner::AttachmentRunScope<'_>,
        ) -> io::Result<Box<dyn crate::runner::AttachmentStaging>> {
            DirectWindowsRunner.attachment_staging(scope)
        }
        fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
            self.launch_with_config(
                request,
                json!({"approval_policy":"never","sandbox_mode":"read-only"}),
            )
        }
    }

    impl FixtureRunner {
        fn launch_with_config(
            &self,
            request: LaunchRequest<'_>,
            config: Value,
        ) -> io::Result<CapturedProcess> {
            self.launch_fixture(request, config, false)
        }

        fn launch_fixture(
            &self,
            request: LaunchRequest<'_>,
            config: Value,
            wait_for_steering: bool,
        ) -> io::Result<CapturedProcess> {
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
                json!({"id":4,"result":{"config":config}}),
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
                if wait_for_steering && data["method"] == "turn/completed" {
                    continue;
                }
                write
                    .send(Ok(OutputFrame {
                        stream: Stream::Stdout,
                        data: data.to_string(),
                    }))
                    .unwrap();
            }
            Ok(CapturedProcess {
                input: if wait_for_steering {
                    ProcessInput::new(SteeringWire {
                        log: self.0.clone(),
                        output: write,
                    })
                } else {
                    ProcessInput::new(self.0.clone())
                },
                output,
                control: Arc::new(CompletedProcess),
            })
        }
    }

    struct SteeringWire {
        log: WireLog,
        output: mpsc::Sender<io::Result<OutputFrame>>,
    }

    impl io::Write for SteeringWire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let written = self.log.write(bytes)?;
            let frame: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
            if frame["method"] == "turn/steer" {
                let conversation = self.log.conversation.clone();
                let expected_frame = bytes.to_vec();
                self.log.runtime.block_on(self.log.store.execute(move |db| {
                    let payload: Vec<u8> = db.query_row(
                        "SELECT e.payload FROM instruction_delivery_attempts i JOIN agent_events e ON e.seq = i.prepared_event_seq
                         JOIN active_agent_attempts active ON active.run_id = i.run_id
                         WHERE active.conversation_id = ?1 AND i.steering_state = 'prepared'",
                        [conversation], |row| row.get(0),
                    ).context("Steering reached the wire before its receipt was durable")?;
                    let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                    assert!(matches!(event.data, ProviderEventData::SteeringPrepared { frame, .. } if frame == expected_frame));
                    Ok(())
                })).map_err(io::Error::other)?;
                assert_eq!(frame["params"]["expectedTurnId"], "native-turn");
                for response in [
                    json!({"id":frame["id"],"result":{"turnId":"native-turn"}}),
                    json!({"method":"turn/completed","params":{"threadId":"native-session",
                        "turn":{"id":"native-turn","status":"completed"}}}),
                ] {
                    self.output
                        .send(Ok(OutputFrame {
                            stream: Stream::Stdout,
                            data: response.to_string(),
                        }))
                        .map_err(io::Error::other)?;
                }
            }
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct HeldSteeringRunner(FixtureRunner);

    impl Runner for HeldSteeringRunner {
        fn identity(&self) -> &str {
            self.0.identity()
        }
        fn execution_platform(&self) -> Option<ExecutionPlatform> {
            self.0.execution_platform()
        }
        fn executable_revision(&self, path: &Path) -> Option<String> {
            self.0.executable_revision(path)
        }
        fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
            self.0.launch_fixture(
                request,
                json!({"approval_policy":"never","sandbox_mode":"read-only"}),
                true,
            )
        }
    }

    #[tokio::test]
    async fn task_dispatch_freezes_hierarchy_and_selected_checklist_revisions() -> Result<()> {
        use crate::{
            jobs::{AdmissionEnvironment, JobRequest, PipelineDispatch, PipelineNodeKind},
            tasks::{DescriptionOwner, TaskRunScope},
        };

        let (dir, store, chat, scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        let registered = crate::repository::register(&store, scope.checkout.clone()).await?;
        let project = registered.project_id;
        let epic = store
            .create_project_epic(project.clone(), "Release".into())
            .await?;
        let task = store
            .create_project_task(project.clone(), "API".into())
            .await?;
        store
            .set_project_task_epic(project.clone(), task.id.clone(), Some(epic.id.clone()))
            .await?;
        let child = store
            .create_project_subtask(project.clone(), task.id.clone(), "Review".into())
            .await?;
        let epic_owner = DescriptionOwner::Epic(epic.id);
        let task_owner = DescriptionOwner::Work(task.id);
        let epic_revision = store
            .save_project_description(
                project.clone(),
                epic_owner.clone(),
                None,
                Some("# Original Epic".into()),
            )
            .await?;
        let task_revision = store
            .save_project_description(
                project.clone(),
                task_owner.clone(),
                None,
                Some("# Original API".into()),
            )
            .await?;
        let child_revision = store
            .save_project_description(
                project.clone(),
                DescriptionOwner::Work(child.id.clone()),
                None,
                Some("# Review API".into()),
            )
            .await?;
        let checklist = store
            .ensure_project_checklist(project.clone(), child.id.clone())
            .await?;
        let selected = store
            .append_project_checklist_item(
                project.clone(),
                child.id.clone(),
                "Check status codes".into(),
            )
            .await?;
        let sibling = store
            .append_project_checklist_item(
                project.clone(),
                child.id.clone(),
                "Check HTTP headers".into(),
            )
            .await?;
        let pipeline = "description-pipeline".to_owned();
        store
            .register_job_pipeline(project.clone(), pipeline.clone())
            .await?;
        store
            .register_job_pipeline_node(
                project.clone(),
                pipeline.clone(),
                "review".into(),
                PipelineNodeKind::Execution,
            )
            .await?;
        let binding = PipelineDispatch {
            pipeline_run_id: pipeline,
            node_instance_id: "review".into(),
            plan_revision: None,
            release_id: None,
        };
        let request = JobRequest {
            checkout_id: Some(chat.checkout_id),
            task_id: Some(child.id.clone()),
            mode: SessionMode::ReadOnly,
            instructions: "Review the saved changes".into(),
            dependencies: Vec::new(),
            requires_bridge: false,
        };
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let dispatch = |store: Store, item: Option<String>| {
            let (binding, scope, request, env) =
                (binding.clone(), scope.clone(), request.clone(), env.clone());
            async move {
                store
                    .submit_pipeline_task_agent_job(
                        binding,
                        scope,
                        request,
                        TaskRunScope {
                            role: "reviewer".into(),
                            checklist_item_id: item,
                        },
                    )
                    .await?;
                let plan = store
                    .claim_next_job(env)
                    .await?
                    .context("missing review job")?;
                let chat = plan.conversation_id.clone();
                let run = plan.run.id.clone();
                let runner = Arc::new(FixtureRunner(WireLog {
                    store: store.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    conversation: chat.clone(),
                    frames: Arc::new(Mutex::new(Vec::new())),
                }));
                assert_eq!(
                    start_managed_turn(store, runner, plan, None).wait().await?,
                    RunStatus::Succeeded
                );
                Ok::<_, anyhow::Error>((chat, run))
            }
        };

        let (first_chat, first_run) = dispatch(store.clone(), Some(selected.id.clone())).await?;
        let edited = store
            .save_project_description(
                project.clone(),
                task_owner.clone(),
                Some(task_revision.id.clone()),
                Some("# Updated API".into()),
            )
            .await?;
        assert!(
            store
                .save_project_description(
                    project.clone(),
                    task_owner,
                    Some(task_revision.id.clone()),
                    Some("Stale write".into())
                )
                .await
                .is_err()
        );
        store
            .save_project_description(
                project.clone(),
                epic_owner,
                Some(epic_revision.id.clone()),
                None,
            )
            .await?;
        let db_path = dir.path().join("store.sqlite");
        drop(store);
        let store = Store::open(db_path.clone())?;
        let first = store
            .agent_dispatch(first_chat, first_run)
            .await?
            .context("first dispatch evidence was lost")?;
        let frozen = first
            .snapshot
            .task_context
            .context("the selected task context was not saved")?;
        assert_eq!(frozen.task_id, child.id);
        assert_eq!(frozen.descriptions.epic.as_ref(), Some(&epic_revision));
        assert_eq!(frozen.descriptions.task.as_ref(), Some(&task_revision));
        assert_eq!(frozen.descriptions.subtask.as_ref(), Some(&child_revision));
        assert_eq!(frozen.checklist_item_id.as_ref(), Some(&selected.id));
        assert_eq!(frozen.checklist_id.as_ref(), Some(&checklist));
        assert_eq!(frozen.checklist, [selected.clone()]);
        let frame: Value = serde_json::from_slice(&first.frames[0])?;
        let text = frame
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .context("missing submitted prompt")?;
        let (_, block) = text
            .split_once("\n\nSaved task context\n")
            .context("missing task context in submitted prompt")?;
        assert_eq!(
            serde_json::from_str::<crate::tasks::TaskDispatchContext>(block)?,
            frozen
        );
        assert_eq!(
            store
                .project_description_revision(project.clone(), task_revision.id.clone())
                .await?,
            Some(task_revision.clone())
        );
        let db = rusqlite::Connection::open(db_path)?;
        let body: Vec<u8> = db.query_row(
            "SELECT body FROM project_description_revisions WHERE id = ?1",
            [&task_revision.id],
            |row| row.get(0),
        )?;
        assert_ne!(body, b"# Original API");
        assert_eq!(protection::unprotect(&body)?, b"# Original API");
        drop(db);

        let (second_chat, second_run) = dispatch(store.clone(), None).await?;
        let second = store
            .agent_dispatch(second_chat, second_run)
            .await?
            .context("second dispatch missing")?;
        let current = second
            .snapshot
            .task_context
            .context("missing current task context")?;
        assert_eq!(current.descriptions.epic, None);
        assert_eq!(current.descriptions.task, Some(edited));
        assert_eq!(current.descriptions.subtask, Some(child_revision));
        assert_eq!(current.checklist_item_id, None);
        assert_eq!(current.checklist, [selected, sibling]);
        Ok(())
    }

    #[tokio::test]
    async fn pipeline_dispatch_reproduces_a_selected_repository_markdown_after_rename() -> Result<()>
    {
        use crate::jobs::{AdmissionEnvironment, JobRequest, PipelineDispatch, PipelineNodeKind};

        let (_dir, store, chat, scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        let registered = crate::repository::register(&store, scope.checkout.clone()).await?;
        let project = registered.project_id;
        let path = scope.checkout.join("release.md");
        let bytes = b"\xef\xbb\xbf# Release\r\n\r\nReview this version.\r\n";
        std::fs::write(&path, bytes)?;
        let outsider = tempfile::tempdir()?;
        let external = outsider.path().join("external.md");
        std::fs::write(&external, b"outside source")?;
        assert!(
            store
                .capture_repository_markdown(chat.checkout_id.clone(), external)
                .await
                .is_err()
        );
        let selected = store
            .capture_repository_markdown(chat.checkout_id.clone(), path.clone())
            .await?;
        assert_eq!(selected.markdown.as_bytes(), bytes);
        assert_eq!(selected.revision.relative_path, "release.md");

        std::fs::write(&path, b"# Changed source")?;
        std::fs::rename(&path, scope.checkout.join("new-name.md"))?;
        assert_eq!(
            store
                .repository_markdown_revisions(chat.checkout_id.clone())
                .await?,
            [selected.revision.clone()]
        );
        assert_eq!(
            store
                .repository_markdown("other-checkout".into(), selected.revision.id.clone())
                .await?,
            None
        );

        let pipeline = "repository-source-fixture".to_owned();
        store
            .register_job_pipeline(project.clone(), pipeline.clone())
            .await?;
        store
            .register_job_pipeline_node(
                project,
                pipeline.clone(),
                "planning".into(),
                PipelineNodeKind::Execution,
            )
            .await?;
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        store
            .submit_pipeline_agent_job_from_markdown(
                PipelineDispatch {
                    pipeline_run_id: pipeline,
                    node_instance_id: "planning".into(),
                    plan_revision: None,
                    release_id: None,
                },
                scope.clone(),
                JobRequest {
                    checkout_id: Some(chat.checkout_id.clone()),
                    task_id: None,
                    mode: SessionMode::ReadOnly,
                    instructions: "Propose a plan from this repository source".into(),
                    dependencies: Vec::new(),
                    requires_bridge: false,
                },
                selected.revision.id.clone(),
            )
            .await?;
        let plan = store
            .claim_next_job(env)
            .await?
            .context("missing planning job")?;
        let run = plan.run.id.clone();
        let conversation = plan.conversation_id.clone();
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.clone(),
            frames: Arc::new(Mutex::new(Vec::new())),
        }));
        assert_eq!(
            start_managed_turn(store.clone(), runner, plan, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        drop(store);

        let store = Store::open(scope.checkout.join("store.sqlite"))?;
        let dispatch = store
            .agent_dispatch(conversation, run)
            .await?
            .context("missing dispatch")?;
        assert_eq!(
            dispatch.snapshot.repository_markdown.as_ref(),
            Some(&selected)
        );
        let frame: Value = serde_json::from_slice(&dispatch.frames[0])?;
        let prompt = frame
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .context("missing prompt")?;
        let (_, source) = prompt
            .split_once("\n\nRepository Markdown source\n")
            .context("missing saved source bytes")?;
        assert_eq!(
            serde_json::from_str::<crate::repository::RepositoryMarkdown>(source)?,
            selected
        );
        assert_eq!(
            store
                .repository_markdown(chat.checkout_id, selected.revision.id.clone())
                .await?,
            Some(selected)
        );
        Ok(())
    }

    #[tokio::test]
    async fn dispatch_pins_role_policy_and_keeps_each_selected_context_revision() -> Result<()> {
        use crate::{
            context_records::{
                ContextProvenance, ContextRecordContent, ContextRecordKind, ContextReviewDecision,
                ContextReviewStatus,
                assembly::{
                    ContextBlock, ContextInputRevision, RequiredContextInstruction,
                    RoleContextPolicy,
                },
            },
            jobs::{
                AdmissionBlocker, AdmissionEnvironment, DispatchRejection, JobRequest,
                PipelineAgentInput, PipelineDispatch, PipelineNodeKind,
            },
        };

        let (_dir, store, chat, scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        ensure!(
            Command::new("git.exe")
                .current_dir(&scope.checkout)
                .args([
                    "-c",
                    "user.name=OmniAgent Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=.git/disabled-hooks",
                    "commit",
                    "--allow-empty",
                    "--quiet",
                    "-m",
                    "Fixture base",
                ])
                .status()?
                .success()
        );
        let base =
            crate::repository::base_commit(&scope.checkout)?.context("missing fixture base")?;
        let project = crate::repository::register(&store, scope.checkout.clone())
            .await?
            .project_id;
        let pipeline = "role-context-workflow".to_owned();
        store
            .register_job_pipeline(project.clone(), pipeline.clone())
            .await?;
        store
            .register_job_pipeline_node(
                project.clone(),
                pipeline.clone(),
                "review".into(),
                PipelineNodeKind::Execution,
            )
            .await?;
        let record = |text: &str| ContextRecordContent {
            title: "Audit expectations".into(),
            markdown: text.into(),
        };
        let first = store
            .propose_context_record(
                project.clone(),
                None,
                ContextRecordKind::ProjectConvention,
                record("Audit the request format"),
                ContextProvenance::Operator,
            )
            .await?;
        store
            .review_context_revision(
                project.clone(),
                first.record_id.clone(),
                first.id.clone(),
                ContextReviewStatus::Proposed,
                ContextReviewDecision::Accept,
            )
            .await?;

        let policy = RoleContextPolicy {
            revision_id: "review-policy-r1".into(),
            role: "reviewer".into(),
            available_bytes: 8192,
            required: vec![RequiredContextInstruction {
                instruction_id: "review-constraints".into(),
                revision_id: "constraints-r2".into(),
                title: "Review constraints".into(),
                markdown: "Keep all existing guarantees".into(),
            }],
            explicit_links: Vec::new(),
        };
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        let execute = |role_context: RoleContextPolicy| {
            let (store, env, scope, pipeline, checkout) = (
                store.clone(),
                env.clone(),
                scope.clone(),
                pipeline.clone(),
                chat.checkout_id.clone(),
            );
            async move {
                let job = store
                    .submit_pipeline_agent_job_with_input(
                        PipelineDispatch {
                            pipeline_run_id: pipeline,
                            node_instance_id: "review".into(),
                            plan_revision: None,
                            release_id: None,
                        },
                        scope,
                        JobRequest {
                            checkout_id: Some(checkout),
                            task_id: None,
                            mode: SessionMode::ReadOnly,
                            instructions: "Audit the context".into(),
                            dependencies: Vec::new(),
                            requires_bridge: false,
                        },
                        PipelineAgentInput {
                            role_context: Some(role_context),
                            ..Default::default()
                        },
                    )
                    .await?;
                let plan = store
                    .claim_next_job(env)
                    .await?
                    .context("missing context job")?;
                let run = plan.run.id.clone();
                let conversation = plan.conversation_id.clone();
                let session = plan.agent_session_id.clone();
                let frames = Arc::new(Mutex::new(Vec::new()));
                let runner = Arc::new(FixtureRunner(WireLog {
                    store: store.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    conversation: conversation.clone(),
                    frames: frames.clone(),
                }));
                let outcome = start_managed_turn(store.clone(), runner, plan, None)
                    .wait()
                    .await?;
                let evidence = store.agent_dispatch(conversation, run).await?;
                Ok::<_, anyhow::Error>((job.id, session, outcome, evidence, frames))
            }
        };

        let (_, session, outcome, first_dispatch, _) = execute(policy.clone()).await?;
        assert_eq!(outcome, RunStatus::Succeeded);
        let first_dispatch = first_dispatch.context("missing first dispatch")?;
        assert_eq!(
            first_dispatch.snapshot.session_id.as_deref(),
            Some(session.as_str())
        );
        assert_eq!(first_dispatch.snapshot.base_commit.as_ref(), Some(&base));
        assert_eq!(
            first_dispatch.snapshot.job_policy_revision.as_deref(),
            Some("default")
        );
        let pinned = first_dispatch
            .snapshot
            .role_context
            .context("missing protected role selection")?;
        assert_eq!(pinned.policy_revision_id, policy.revision_id);
        assert_eq!(
            pinned.policy_digest,
            crate::providers::capabilities::revision(&policy)?
        );
        assert_eq!(pinned.selection.role, policy.role);
        assert_eq!(pinned.selection.blocks.len(), 2);
        let frame: Value = serde_json::from_slice(&first_dispatch.frames[0])?;
        let prompt = frame
            .pointer("/params/input/0/text")
            .and_then(Value::as_str)
            .context("missing input frame")?;
        let (_, encoded) = prompt
            .split_once("\n\nAssembled context\n")
            .context("missing selected context")?;
        assert_eq!(
            serde_json::from_str::<Vec<ContextBlock>>(encoded)?,
            pinned.selection.blocks
        );

        let edited = store
            .propose_context_revision(
                project.clone(),
                first.record_id.clone(),
                first.id.clone(),
                record("Audit response headers instead"),
                ContextProvenance::Operator,
            )
            .await?;
        store
            .review_context_revision(
                project.clone(),
                first.record_id.clone(),
                edited.id.clone(),
                ContextReviewStatus::Proposed,
                ContextReviewDecision::Accept,
            )
            .await?;
        let (_, _, outcome, second_dispatch, _) = execute(policy.clone()).await?;
        assert_eq!(outcome, RunStatus::Succeeded);
        let later = second_dispatch
            .context("missing later dispatch")?
            .snapshot
            .role_context
            .unwrap();
        assert_eq!(later.selection.blocks[0], pinned.selection.blocks[0]);
        assert_eq!(
            pinned.selection.blocks[1].markdown,
            "Audit the request format"
        );
        assert_eq!(
            later.selection.blocks[1].markdown,
            "Audit response headers instead"
        );
        assert!(
            matches!(&later.selection.blocks[1].revision, ContextInputRevision::Record { version, revision: 2 } if version.revision_id == edited.id)
        );

        let (blocked, _, outcome, evidence, writes) = execute(RoleContextPolicy {
            revision_id: "review-policy-r2".into(),
            available_bytes: 1,
            ..policy
        })
        .await?;
        assert_eq!(outcome, RunStatus::Failed);
        assert!(evidence.is_none());
        assert!(writes.lock().unwrap().iter().all(|bytes| {
            serde_json::from_slice::<Value>(bytes)
                .is_ok_and(|frame| frame["method"] != "turn/start")
        }));
        let inspection = store
            .inspect_job(project, blocked, env)
            .await?
            .context("missing blocked job")?;
        assert!(inspection.blockers.iter().any(|blocker| matches!(blocker, AdmissionBlocker::DispatchRejected {
            rejection: DispatchRejection::RequiredContextOverBudget { available_bytes: 1, required_bytes }
        } if *required_bytes > 1)));
        Ok(())
    }

    #[tokio::test]
    async fn scheduled_steering_records_the_exact_wire_before_the_cli_acknowledges() -> Result<()> {
        use crate::jobs::{JobContext, JobScheduler};

        let (_dir, store, conversation, mut scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        // Keep the same authenticated profile and fingerprint for qualification and delivery.
        scope.effort = None;
        let log = WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.id.clone(),
            frames: Arc::new(Mutex::new(Vec::new())),
        };
        let baseline = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "Check the fixture scope",
        )
        .await?;
        let baseline_id = baseline.run.id.clone();
        assert_eq!(
            start_managed_turn(
                store.clone(),
                Arc::new(FixtureRunner(log.clone())),
                baseline,
                None
            )
            .wait()
            .await?,
            RunStatus::Succeeded
        );
        let assessment = store
            .agent_dispatch(conversation.id.clone(), baseline_id.clone())
            .await?
            .unwrap()
            .snapshot
            .capabilities
            .unwrap();
        let descriptor = assessment
            .entries
            .iter()
            .find(|entry| {
                entry.descriptor.subject
                    == CapabilitySubject::Protocol {
                        capability: Capability::LiveSteering,
                    }
            })
            .context("Missing fixture steering capability")?
            .descriptor
            .clone();
        store
            .record_capability_evidence(
                assessment.scope,
                descriptor,
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                baseline_id,
            )
            .await?;

        let scheduler = JobScheduler::start(
            store.clone(),
            Arc::new(HeldSteeringRunner(FixtureRunner(log))),
            None,
        );
        let enqueue = |text: &'static str| {
            let (store, conversation, scope) =
                (store.clone(), conversation.id.clone(), scope.clone());
            async move {
                let owner = store.conversation(conversation.clone()).await?;
                store
                    .save_conversation_draft(conversation.clone(), text.into())
                    .await?;
                store
                    .enqueue_conversation_turn(
                        JobContext {
                            conversation_id: conversation,
                            context_id: owner.context_id,
                        },
                        scope,
                        text.into(),
                    )
                    .await
            }
        };
        let owner = enqueue("Run with native steering").await?;
        let run = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(attempt) = store.job(owner.project_id.clone(), owner.id.clone()).await?.and_then(|job| job.latest_attempt) {
                    let ready: bool = store.execute({
                        let run = attempt.run_id.clone();
                        move |db| Ok(db.query_row("SELECT provider_turn_id IS NOT NULL FROM agent_attempts WHERE run_id = ?1", [run], |row| row.get(0))?)
                    }).await?;
                    if ready { break Ok::<_, anyhow::Error>(attempt.run_id); }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await??;
        let followup = enqueue("Handle this instruction now").await?;
        let pending = store
            .pending_conversation_instructions(conversation.id.clone(), scheduler.environment())
            .await?;
        let instruction = pending
            .into_iter()
            .find(|entry| entry.id == followup.id)
            .context("Missing pending steering instruction")?;
        assert!(
            scheduler
                .steer_pending_instruction(
                    &store,
                    conversation.id.clone(),
                    run.clone(),
                    followup.id.clone(),
                    instruction.revision_id.clone()
                )
                .await?
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let messages = store.conversation_messages(conversation.id.clone()).await?;
                if messages
                    .iter()
                    .any(|message| message.revision_id == instruction.revision_id)
                {
                    break Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        let linked = store
            .job(followup.project_id, followup.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap();
        assert_eq!(linked.run_id, run);
        assert_eq!(linked.status, RunStatus::Succeeded);
        assert!(
            store
                .pending_conversation_instructions(conversation.id, scheduler.environment())
                .await?
                .is_empty()
        );
        scheduler.shutdown().await?;
        Ok(())
    }

    struct NativeQuotaFixture {
        wire: FixtureRunner,
        requested: u32,
        reported: u32,
        reported_v2: u32,
    }

    impl Runner for NativeQuotaFixture {
        fn identity(&self) -> &str {
            DirectWindowsRunner::IDENTITY
        }

        fn execution_platform(&self) -> Option<ExecutionPlatform> {
            self.wire.execution_platform()
        }

        fn executable_revision(&self, path: &Path) -> Option<String> {
            self.wire.executable_revision(path)
        }

        fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
            if request
                .arguments
                .first()
                .is_some_and(|arg| *arg == "app-server")
            {
                let enabled = format!("agents.enabled={}", self.requested > 0);
                assert!(request.arguments.contains(&std::ffi::OsStr::new(&enabled)));
                let v2 = format!(
                    "features.multi_agent_v2.max_concurrent_threads_per_session={}",
                    self.requested + 1
                );
                assert!(request.arguments.contains(&std::ffi::OsStr::new(&v2)));
                if self.requested > 0 {
                    let cap = format!(
                        "agents.max_concurrent_threads_per_session={}",
                        self.requested
                    );
                    assert!(request.arguments.contains(&std::ffi::OsStr::new(&cap)));
                }
            }
            self.wire.launch_with_config(
                request,
                json!({
                    "approval_policy":"never", "sandbox_mode":"read-only",
                    "agents": {
                        "enabled": self.reported > 0,
                        "max_concurrent_threads_per_session": self.reported,
                    },
                    "features": { "multi_agent_v2": { "max_concurrent_threads_per_session": self.reported_v2 } }
                }),
            )
        }
    }

    #[tokio::test]
    async fn strict_subagent_allowances_are_verified_before_the_prompt_and_pin_the_session()
    -> Result<()> {
        use crate::jobs::SubagentLimitMode;

        let (_dir, store, conversation, mut scope) = setup().await?;
        scope.runner = DirectWindowsRunner::IDENTITY.into();
        let strict = Some(SubagentLimitMode::Strict);
        let mut first_session = None;
        for (requested, reported, reported_v2, expected) in [
            (1, 1, 2, RunStatus::Succeeded),
            (0, 0, 1, RunStatus::Succeeded),
            (1, 1, 3, RunStatus::Failed),
        ] {
            store
                .set_conversation_subagent_quota(
                    conversation.id.clone(),
                    strict,
                    2,
                    Some(requested),
                )
                .await?;
            let turn = prepare(
                &store,
                &conversation.id,
                scope.clone(),
                "Test the native quota",
            )
            .await?;
            assert!(matches!(turn.intent, SessionIntent::Start));
            if let Some(prior) = first_session.replace(turn.agent_session_id.clone()) {
                assert_ne!(prior, turn.agent_session_id);
            }
            assert_eq!(turn.native_subagent_limit, Some(requested));
            let before = store
                .conversation_subagent_quota(conversation.id.clone())
                .await?;
            assert_eq!(before.native_sessions[0].allowance, requested);
            assert!(!before.native_sessions[0].verified);
            let run = turn.run.id.clone();
            let log = Arc::new(Mutex::new(Vec::new()));
            let runner = Arc::new(NativeQuotaFixture {
                wire: FixtureRunner(WireLog {
                    store: store.clone(),
                    runtime: tokio::runtime::Handle::current(),
                    conversation: conversation.id.clone(),
                    frames: log.clone(),
                }),
                requested,
                reported,
                reported_v2,
            });
            let outcome = tokio::time::timeout(
                Duration::from_secs(10),
                start_managed_turn(store.clone(), runner, turn, None).wait(),
            )
            .await??;
            assert_eq!(outcome, expected);
            let dispatched = store.agent_dispatch(conversation.id.clone(), run).await?;
            assert_eq!(dispatched.is_some(), expected == RunStatus::Succeeded);
            if let Some(dispatched) = dispatched {
                let flags = dispatched.snapshot.scoped_launch.unwrap().inputs;
                let cap = format!(
                    "features.multi_agent_v2.max_concurrent_threads_per_session={}",
                    requested + 1
                );
                assert!(flags.iter().any(|input| input.contents == cap));
            }
            assert!(
                store
                    .conversation_subagent_quota(conversation.id.clone())
                    .await?
                    .native_sessions
                    .is_empty()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn native_subagent_audit_requires_a_complete_authenticated_fixture_and_rechecks_the_scope()
    -> Result<()> {
        use crate::{jobs::SubagentAuditPolicy, providers::ChildDetails};

        let (_dir, store, conversation, scope) = setup().await?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.id.clone(),
            frames: log,
        }));
        let baseline = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "Read effective CLI scope",
        )
        .await?;
        let baseline_id = baseline.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner.clone(), baseline, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let evidence = store
            .agent_dispatch(conversation.id.clone(), baseline_id.clone())
            .await?
            .unwrap();
        let mut snapshot = evidence.snapshot;
        let assessment = snapshot
            .capabilities
            .as_ref()
            .context("Fixture had no capability scope")?;
        let effective = assessment.scope.clone();
        let descriptor = assessment
            .entries
            .iter()
            .find(|entry| {
                entry.descriptor.subject
                    == CapabilitySubject::Protocol {
                        capability: Capability::NativeSubagentAudit,
                    }
            })
            .context("Missing audit capability subject")?
            .descriptor
            .clone();
        assert!(
            store
                .record_capability_evidence(
                    effective.clone(),
                    descriptor.clone(),
                    EvidenceOrigin::AuthenticatedFixture,
                    ObservedResult::Succeeded,
                    baseline_id,
                )
                .await
                .is_err()
        );

        // Feed the normalized frames a capable adapter would report in an authenticated fixture.
        let qualified = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "Observe a native child",
        )
        .await?;
        let qualified_id = qualified.run.id;
        store.mark_agent_running(qualified_id.clone()).await?;
        snapshot.context_id = qualified.context_id;
        snapshot.submitted_messages = qualified.input;
        snapshot.native_history = NativeHistory::Fresh;
        let sent = store
            .prepare_dispatch(qualified_id.clone(), snapshot, vec![b"fixture".to_vec()])
            .await?;
        store.settle_dispatch(sent, Delivery::Submitted).await?;
        let observe = |run: String, complete: bool| {
            store.record_agent_observation(
                run,
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        session_id: Some("native-session".into()),
                        ..Default::default()
                    },
                    data: ProviderEventData::ChildActivity {
                        child_id: Some("child-a".into()),
                        status: Some("completed".into()),
                        details: Some(ChildDetails {
                            prompt: Some("Submitted child prompt".into()),
                            context_provenance: complete.then(|| "context:fixture-revision".into()),
                            response: complete.then(|| "Recorded child answer".into()),
                            ..Default::default()
                        }),
                    },
                },
            )
        };
        observe(qualified_id.clone(), true).await?;
        store
            .finish_agent_attempt(
                qualified_id.clone(),
                RunStatus::Succeeded,
                Some("Fixture response".into()),
            )
            .await?;
        let evidence_id = store
            .record_capability_evidence(
                effective.clone(),
                descriptor.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                qualified_id.clone(),
            )
            .await?;

        store
            .set_conversation_subagent_audit(
                conversation.id.clone(),
                Some(SubagentAuditPolicy::Complete),
            )
            .await?;
        let admitted = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "Use the qualified native profile",
        )
        .await?;
        assert!(
            matches!(&admitted.subagent_audit, SubagentAuditRoute::Native { evidence_id: id } if id == &evidence_id)
        );
        assert!(admitted.native_subagent_limit.is_none());
        let verified_id = admitted.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner.clone(), admitted, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        assert!(
            store
                .agent_dispatch(conversation.id.clone(), verified_id)
                .await?
                .is_some()
        );
        let guide = scope.checkout.join("AGENTS.md");
        std::fs::write(&guide, "Changed instructions in disposable audit fixture.")?;
        let stale = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "Recheck the fixture scope",
        )
        .await?;
        let stale_id = stale.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner, stale, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        assert!(
            store
                .agent_dispatch(conversation.id.clone(), stale_id)
                .await?
                .is_none()
        );
        assert!(
            prepare(
                &store,
                &conversation.id,
                scope.clone(),
                "A stale candidate should be unavailable"
            )
            .await
            .is_err()
        );
        std::fs::remove_file(guide)?;
        store
            .record_capability_evidence(
                effective,
                descriptor,
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                qualified_id,
            )
            .await?;

        let partial = prepare(
            &store,
            &conversation.id,
            scope.clone(),
            "A provider drops some evidence",
        )
        .await?;
        store.mark_agent_running(partial.run.id.clone()).await?;
        observe(partial.run.id.clone(), false).await?;
        assert_eq!(
            store
                .finish_observed_agent_attempt(
                    partial.run.id,
                    RunStatus::Succeeded,
                    Some("Partial answer".into()),
                    None,
                    true
                )
                .await?,
            RunStatus::Failed
        );
        assert!(
            prepare(&store, &conversation.id, scope, "Unverified native route")
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn desktop_admission_consumes_only_the_exact_draft_atomically() -> Result<()> {
        let (_directory, store, chat, scope) = setup().await?;
        let id = chat.id.clone();
        store
            .save_conversation_draft(id.clone(), "newer revision".into())
            .await?;
        assert!(
            store
                .prepare_conversation_draft_attempt(
                    id.clone(),
                    chat.context_id.clone(),
                    scope.clone(),
                    "older revision".into(),
                )
                .await
                .is_err()
        );
        assert!(store.latest_conversation_run(id.clone()).await?.is_none());
        assert_eq!(
            store.conversation_draft(id.clone()).await?,
            "newer revision"
        );
        assert!(
            store
                .prepare_conversation_draft_attempt(
                    id.clone(),
                    "stale context".into(),
                    scope.clone(),
                    "newer revision".into(),
                )
                .await
                .is_err()
        );
        assert_eq!(
            store.conversation_draft(id.clone()).await?,
            "newer revision"
        );
        let plan = store
            .prepare_conversation_draft_attempt(
                id.clone(),
                chat.context_id,
                scope,
                "newer revision".into(),
            )
            .await?;
        assert!(store.conversation_draft(id.clone()).await?.is_empty());
        assert_eq!(
            store.latest_conversation_run(id.clone()).await?.unwrap().id,
            plan.run.id
        );
        assert_eq!(
            store.conversation_messages(id).await?[0].text,
            "newer revision"
        );
        Ok(())
    }

    #[tokio::test]
    async fn native_attachments_submit_qualified_frozen_bytes_and_a_read_only_path() -> Result<()> {
        use crate::attachments::{
            AttachmentDelivery, AttachmentFormat, AttachmentInput, AttachmentPreview,
            AttachmentSource, NativeAttachmentEncoding,
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
            start_managed_turn(store.clone(), runner.clone(), admitted, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        std::fs::rename(&file, file.with_file_name("renamed-after-dispatch.png"))?;
        let committed = store
            .agent_dispatch(chat.id.clone(), run.clone())
            .await?
            .unwrap();
        let attachment = &committed.snapshot.attachments[0];
        assert_eq!(attachment.metadata, expected);
        assert_eq!(
            attachment.qualification_id.as_deref(),
            Some(qualification.as_str())
        );
        let native = attachment.input.native().unwrap();
        assert_eq!(native.content, PNG);
        assert_eq!(
            store
                .attachment_dispatch_content(chat.id.clone(), run.clone(), 0)
                .await?
                .as_deref(),
            Some(PNG)
        );
        assert!(
            store
                .attachment_dispatch_content("unrelated".into(), run, 0)
                .await?
                .is_none()
        );
        assert!(
            matches!(crate::dispatch::AttachmentEvidence::from(attachment).transport,
            crate::dispatch::AttachmentTransport::Native { runner_path, .. } if runner_path == native.path)
        );
        assert_eq!(
            attachment.message_revision_id,
            committed.snapshot.submitted_messages[0].revision_id
        );
        let frame: Value = serde_json::from_slice(&committed.frames[0])?;
        assert_eq!(
            frame["params"]["input"][1],
            json!({"type":"localImage","path":native.path})
        );
        assert!(!native.path.as_ref().unwrap().exists());

        let next = prepare(&store, &chat.id, scope.clone(), "Continue").await?;
        assert!(next.attachments.is_empty());
        store
            .finish_agent_attempt(next.run.id, RunStatus::Cancelled, None)
            .await?;
        let mut different = scope;
        different.account_binding = Some("another-account".into());
        let fresh = prepare(
            &store,
            &chat.id,
            different.clone(),
            "Read the retained input",
        )
        .await?;
        assert!(matches!(fresh.intent, SessionIntent::Start));
        assert_eq!(fresh.attachments.len(), 1);
        assert_eq!(fresh.attachments[0].1.metadata, expected);
        let switched_run = fresh.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner.clone(), fresh, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        assert!(
            store
                .agent_dispatch(chat.id.clone(), switched_run.clone())
                .await?
                .is_none()
        );
        let blockers = store.agent_observations(chat.id, switched_run, 0).await?;
        assert!(blockers.iter().any(|entry| match &entry.event.data {
            ProviderEventData::AttachmentBlocked { blockers } =>
                blockers.len() == 1
                    && blockers[0].source == expected.source
                    && blockers[0].delivery == AttachmentDelivery::Native
                    && blockers[0].reason.contains("unverified for this profile"),
            _ => false,
        }));

        std::fs::write(&file, STANDARD.decode(PNG)?)?;
        let branch = store
            .create_conversation(chat.checkout_id, "Switched attachment route".into())
            .await?;
        let runner = Arc::new(FixtureRunner(WireLog {
            conversation: branch.id.clone(),
            ..runner.0.clone()
        }));
        let reference = AttachmentPreview::read_with_delivery(
            &different.checkout,
            expected.source.clone(),
            AttachmentDelivery::Reference,
        )?;
        let plan = store
            .prepare_agent_attempt_with_attachments(
                branch.id.clone(),
                branch.context_id,
                different,
                "Use the reference".into(),
                vec![reference],
            )
            .await?;
        let reference_run = plan.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner, plan, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let reference_dispatch = store
            .agent_dispatch(branch.id, reference_run)
            .await?
            .unwrap();
        let sent = &reference_dispatch.snapshot.attachments[0];
        assert_eq!(sent.metadata, expected);
        assert!(matches!(sent.input, AttachmentInput::Reference(_)));
        assert!(sent.qualification_id.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn fallback_attachments_preserve_the_preview_and_audit_the_runner_target() -> Result<()> {
        use crate::attachments::{
            AttachmentDelivery, AttachmentInput, AttachmentPreview, AttachmentSource,
            ReferencePreview,
        };

        let (_dir, store, chat, mut scope) = setup().await?;
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: chat.id.clone(),
            frames: Arc::default(),
        }));
        let excerpt_path = scope.checkout.join("excerpt.txt");
        std::fs::write(&excerpt_path, "unselected\ncafé ☕\nprivate")?;
        let excerpt_source = AttachmentSource::Checkout {
            relative_path: "excerpt.txt".into(),
        };
        assert!(
            AttachmentPreview::read_with_delivery(
                &scope.checkout,
                excerpt_source.clone(),
                AttachmentDelivery::Excerpt { start: 15, end: 20 },
            )
            .is_err()
        ); // Starts inside the two-byte é.
        let excerpt = AttachmentPreview::read_with_delivery(
            &scope.checkout,
            excerpt_source,
            AttachmentDelivery::Excerpt { start: 11, end: 20 },
        )?;
        assert_eq!(excerpt.excerpt().unwrap().text, "café ☕");

        let checkout_path = scope.checkout.join("large.bin");
        std::fs::File::create(&checkout_path)?.set_len(20 * 1024 * 1024 + 1)?;
        let checkout_source = AttachmentSource::Checkout {
            relative_path: "large.bin".into(),
        };
        assert!(AttachmentPreview::read(&scope.checkout, checkout_source.clone()).is_err());
        let checkout_reference = AttachmentPreview::read_with_delivery(
            &scope.checkout,
            checkout_source,
            AttachmentDelivery::Reference,
        )?;
        assert_eq!(
            checkout_reference.reference_target(),
            Some(ReferencePreview::CheckoutPath(&checkout_path))
        );

        let external_dir = tempfile::tempdir()?;
        let external = dunce::canonicalize(external_dir.path())?.join("selected.txt");
        std::fs::write(&external, "external selection")?;
        let external_reference = AttachmentPreview::read_with_delivery(
            &scope.checkout,
            AttachmentSource::SelectedFile {
                path: external.clone(),
            },
            AttachmentDelivery::Reference,
        )?;
        assert_eq!(
            external_reference.reference_target(),
            Some(ReferencePreview::ReadOnlyCopyOf(&external))
        );
        scope.checkout = scope.checkout.join("nested-cwd");
        std::fs::create_dir(&scope.checkout)?;
        let plan = store
            .prepare_agent_attempt_with_attachments(
                chat.id.clone(),
                chat.context_id,
                scope,
                "Use the attachments".into(),
                vec![excerpt, checkout_reference, external_reference],
            )
            .await?;
        let run = plan.run.id.clone();
        std::fs::write(&excerpt_path, "edited after admission")?;
        assert_eq!(
            start_managed_turn(store.clone(), runner, plan, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        std::fs::rename(
            &excerpt_path,
            excerpt_path.with_file_name("renamed-excerpt.txt"),
        )?;
        std::fs::rename(
            &checkout_path,
            checkout_path.with_file_name("renamed-reference.bin"),
        )?;
        let saved = store
            .agent_dispatch(chat.id.clone(), run.clone())
            .await?
            .unwrap();
        let frame: Value = serde_json::from_slice(&saved.frames[0])?;
        assert_eq!(saved.snapshot.attachments.len(), 3);
        let AttachmentInput::Excerpt(excerpt) = &saved.snapshot.attachments[0].input else {
            panic!("dispatch omitted the frozen excerpt")
        };
        assert_eq!(excerpt.text, "café ☕");
        assert_eq!(
            crate::dispatch::AttachmentEvidence::from(&saved.snapshot.attachments[0]).transport,
            crate::dispatch::AttachmentTransport::Excerpt {
                start: 11,
                end: 20,
                text: "café ☕".into()
            }
        );
        assert_eq!(
            frame["params"]["input"][1]["text"],
            "Excerpt from attachment \"excerpt.txt\" (UTF-8 bytes 11..20):\ncafé ☕"
        );
        assert_eq!(
            saved.snapshot.attachments[1].input.path(),
            Some(checkout_path.as_path())
        );
        assert_eq!(
            crate::dispatch::AttachmentEvidence::from(&saved.snapshot.attachments[1]).transport,
            crate::dispatch::AttachmentTransport::Reference {
                runner_path: checkout_path.clone(),
                uri: None
            }
        );
        let staged = saved.snapshot.attachments[2].input.path().unwrap();
        assert_ne!(staged, external);
        assert!(!staged.exists());
        assert_eq!(std::fs::read_to_string(external)?, "external selection");
        assert_eq!(
            frame["params"]["input"][2]["text"],
            format!(
                "Attachment reference \"large.bin\" (local file path): {}",
                checkout_path.display()
            )
        );
        assert_eq!(
            frame["params"]["input"][3]["text"],
            format!(
                "Attachment reference \"selected.txt\" (local file path): {}",
                staged.display()
            )
        );
        let observations = store.agent_observations(chat.id, run, 0).await?;
        let sent = observations
            .iter()
            .find(|row| {
                matches!(&row.event.data,
            ProviderEventData::DispatchSubmitted { dispatch_id } if dispatch_id == &saved.id)
            })
            .context("missing dispatch submission")?;
        assert!(observations.iter().any(|row| row.seq > sent.seq && row.source == EventSource::Provider
            && matches!(&row.event.data, ProviderEventData::ContextRead { path, .. } if path == "test.txt")));
        assert!(
            saved
                .frames
                .iter()
                .all(|bytes| !String::from_utf8_lossy(bytes).contains("test.txt"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn recovered_jobs_replay_frozen_attachments_and_recheck_references() -> Result<()> {
        use crate::{
            attachments::{
                AttachmentDelivery, AttachmentPreview, AttachmentSource, CapturedPayload,
            },
            jobs::{AdmissionEnvironment, JobContext, RecoveryAction},
            runner::{AttachmentRunScope, StagedAttachmentFile},
        };
        use base64::{Engine, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};

        const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ9sAAAAASUVORK5CYII=";
        for action in [RecoveryAction::Retry, RecoveryAction::Resume] {
            let (_dir, store, chat, mut scope) = setup().await?;
            scope.executable = std::env::current_exe()?;
            let env = AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: false,
            };
            let preview = |name: &str, bytes: &[u8], delivery| {
                std::fs::write(scope.checkout.join(name), bytes)?;
                AttachmentPreview::read_with_delivery(
                    &scope.checkout,
                    AttachmentSource::Checkout {
                        relative_path: name.into(),
                    },
                    delivery,
                )
            };
            let prompt = "Inspect these saved inputs";
            store
                .save_conversation_draft(chat.id.clone(), prompt.into())
                .await?;
            let job = store
                .enqueue_conversation_turn_with_attachments(
                    JobContext {
                        conversation_id: chat.id.clone(),
                        context_id: chat.context_id,
                    },
                    scope.clone(),
                    prompt.into(),
                    vec![
                        preview(
                            "image.png",
                            &STANDARD.decode(PNG)?,
                            AttachmentDelivery::Native,
                        )?,
                        preview(
                            "excerpt.txt",
                            b"original excerpt",
                            AttachmentDelivery::Excerpt { start: 0, end: 8 },
                        )?,
                        preview(
                            "reference.txt",
                            b"original reference",
                            AttachmentDelivery::Reference,
                        )?,
                    ],
                )
                .await?;
            let first = store
                .claim_next_job(env.clone())
                .await?
                .context("missing first attempt")?;
            let run = first.run.id.clone();
            store.mark_agent_running(run.clone()).await?;
            let dispatch = prepare_fixture_dispatch(&store, &first).await?;
            store.settle_dispatch(dispatch, Delivery::Submitted).await?;
            for data in [
                ProviderEventData::SessionReady {
                    model: None,
                    effort: None,
                },
                ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Interrupted,
                },
            ] {
                store
                    .record_agent_observation(
                        run.clone(),
                        EventSource::Provider,
                        ProviderEvent {
                            location: ProviderLocation {
                                session_id: Some("prior-provider-session".into()),
                                ..Default::default()
                            },
                            data,
                        },
                    )
                    .await?;
            }
            let mut abandoned = DirectWindowsRunner.attachment_staging(
                &AttachmentRunScope::for_attempt(&first.run, &scope.checkout),
            )?;
            let bytes = b"private attempt copy";
            abandoned.stage_read_only(
                StagedAttachmentFile {
                    name: "attachment.txt",
                    size: bytes.len() as u64,
                    sha256: &format!("{:x}", Sha256::digest(bytes)),
                },
                &mut bytes.as_slice(),
            )?;
            let path = abandoned.directory().join("attachment.txt");
            drop(abandoned);
            assert!(path.exists());
            store
                .finish_agent_attempt(run.clone(), RunStatus::Cancelled, None)
                .await?;
            store.retire_local_attachments_after_restart().await?;
            assert!(!path.exists());
            assert!(
                store
                    .agent_dispatch(chat.id.clone(), run.clone())
                    .await?
                    .is_some()
            );

            for name in ["image.png", "excerpt.txt", "reference.txt"] {
                std::fs::write(scope.checkout.join(name), b"replacement content")?;
            }
            store
                .queue_job_recovery(job.project_id, job.id, run, action)
                .await?;
            let replay = store
                .claim_next_job(env)
                .await?
                .context("missing recovery attempt")?;
            assert!(matches!(replay.intent, SessionIntent::Start));
            assert!(
                matches!(&replay.attachments[0].1.payload, CapturedPayload::Native { content } if content == PNG)
            );
            assert!(
                matches!(&replay.attachments[1].1.payload, CapturedPayload::Excerpt { excerpt } if excerpt.text == "original")
            );
            let replay_run = replay.run.id.clone();
            let runner = Arc::new(FixtureRunner(WireLog {
                store: store.clone(),
                runtime: tokio::runtime::Handle::current(),
                conversation: chat.id.clone(),
                frames: Arc::default(),
            }));
            assert_eq!(
                start_managed_turn(store.clone(), runner, replay, None)
                    .wait()
                    .await?,
                RunStatus::Failed
            );
            assert!(
                store
                    .agent_dispatch(chat.id.clone(), replay_run.clone())
                    .await?
                    .is_none()
            );
            let observations = store.agent_observations(chat.id, replay_run, 0).await?;
            assert!(observations.iter().any(|entry| matches!(
                &entry.event.data,
                ProviderEventData::AttachmentBlocked { blockers } if blockers.iter().any(|item|
                    item.source.file_name() == Some("reference.txt") && item.reason.contains("preview it again"))
            )));
        }
        Ok(())
    }

    #[tokio::test]
    async fn stale_references_and_unsupported_native_formats_report_each_file_before_dispatch()
    -> Result<()> {
        use crate::attachments::{AttachmentDelivery, AttachmentPreview, AttachmentSource};

        let (_dir, store, chat, scope) = setup().await?;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: chat.id.clone(),
            frames: frames.clone(),
        }));
        let path = scope.checkout.join("stale.txt");
        std::fs::write(&path, "old content")?;
        std::fs::write(scope.checkout.join("unsupported.txt"), "plain text")?;
        let reference = AttachmentPreview::read_with_delivery(
            &scope.checkout,
            AttachmentSource::Checkout {
                relative_path: "stale.txt".into(),
            },
            AttachmentDelivery::Reference,
        )?;
        let unsupported = AttachmentPreview::read(
            &scope.checkout,
            AttachmentSource::Checkout {
                relative_path: "unsupported.txt".into(),
            },
        )?;
        let plan = store
            .prepare_agent_attempt_with_attachments(
                chat.id.clone(),
                chat.context_id,
                scope,
                "Use these files".into(),
                vec![reference, unsupported],
            )
            .await?;
        let run = plan.run.id.clone();
        std::fs::write(path, "new content")?;
        assert_eq!(
            start_managed_turn(store.clone(), runner, plan, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        assert!(
            store
                .agent_dispatch(chat.id.clone(), run.clone())
                .await?
                .is_none()
        );
        assert!(frames.lock().unwrap().iter().all(|frame| {
            serde_json::from_slice::<Value>(frame).unwrap()["method"] != "turn/start"
        }));
        let blockers = store
            .agent_observations(chat.id, run, 0)
            .await?
            .into_iter()
            .find_map(|entry| match entry.event.data {
                ProviderEventData::AttachmentBlocked { blockers } => Some(blockers),
                _ => None,
            })
            .context("missing attachment blockers")?;
        assert_eq!(blockers.len(), 2);
        assert_eq!(blockers[0].source.file_name(), Some("stale.txt"));
        assert!(blockers[0].reason.contains("preview it again"));
        assert_eq!(blockers[1].source.file_name(), Some("unsupported.txt"));
        assert!(blockers[1].reason.contains("no native route"));
        Ok(())
    }

    #[tokio::test]
    async fn attachment_limits_combine_current_project_settings_and_exact_profile_evidence()
    -> Result<()> {
        use crate::attachments::{
            AttachmentDelivery, AttachmentFormat, AttachmentLimits, AttachmentPreview,
            AttachmentSource, NativeAttachmentEncoding, QualifiedAttachmentContext,
            QualifiedAttachmentFormat, QualifiedAttachmentLimits,
        };

        let (_dir, store, chat, scope) = setup().await?;
        let project = crate::repository::register(&store, scope.checkout.clone()).await?;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let make_runner = |conversation: &str| {
            Arc::new(FixtureRunner(WireLog {
                store: store.clone(),
                runtime: tokio::runtime::Handle::current(),
                conversation: conversation.into(),
                frames: frames.clone(),
            }))
        };
        let baseline =
            prepare(&store, &chat.id, scope.clone(), "Establish fixture profile").await?;
        let run = baseline.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), make_runner(&chat.id), baseline, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let capability_scope = store
            .agent_dispatch(chat.id.clone(), run)
            .await?
            .unwrap()
            .snapshot
            .capabilities
            .unwrap()
            .scope;
        let qualified = QualifiedAttachmentLimits {
            files_per_turn: Some(1),
            encoded_request_bytes: Some(600),
            formats: vec![QualifiedAttachmentFormat {
                format: AttachmentFormat::Png,
                encoding: NativeAttachmentEncoding::CodexLocalImage,
                file_bytes: Some(1),
                input_tokens: Some(12),
            }],
            context: Some(QualifiedAttachmentContext {
                window_tokens: 2_000,
                reserved_output_tokens: 1_000,
                fixed_input_tokens: 30,
            }),
        };
        let qualification = store
            .record_attachment_limit_evidence(
                capability_scope,
                "simulated-qualified-limits".into(),
                qualified.clone(),
            )
            .await?;
        let project_limits = AttachmentLimits {
            files_per_turn: 3,
            bytes_per_file: 1024,
            bytes_per_turn: 2048,
            utf8_bytes_per_excerpt: 1400,
        };
        store
            .set_project_attachment_limits(project.project_id.clone(), project_limits)
            .await?;
        assert_eq!(
            store
                .project_attachment_limits(project.project_id.clone())
                .await?,
            project_limits
        );

        let notes = AttachmentSource::Checkout {
            relative_path: "source.txt".into(),
        };
        std::fs::write(scope.checkout.join("source.txt"), "a".repeat(1_500))?;
        let app_error = store
            .preview_attachment(
                chat.checkout_id.clone(),
                notes.clone(),
                AttachmentDelivery::Native,
            )
            .await
            .err()
            .context("project preview accepted an oversized file")?
            .to_string();
        assert!(app_error.contains("source.txt") && app_error.contains("1024 bytes"));
        let range = |count| AttachmentDelivery::Excerpt {
            start: 0,
            end: count,
        };
        let context_case =
            AttachmentPreview::read_with_delivery(&scope.checkout, notes.clone(), range(1_100))?;
        let count_case = [
            AttachmentPreview::read_with_delivery(&scope.checkout, notes.clone(), range(3))?,
            AttachmentPreview::read_with_delivery(&scope.checkout, notes, range(5))?,
        ];
        std::fs::write(scope.checkout.join("escape.txt"), "\u{1}".repeat(250))?;
        let escaped = AttachmentSource::Checkout {
            relative_path: "escape.txt".into(),
        };
        let encoded_case =
            AttachmentPreview::read_with_delivery(&scope.checkout, escaped.clone(), range(250))?;
        std::fs::write(
            scope.checkout.join("picture.png"),
            b"\x89PNG\r\n\x1a\nfixture",
        )?;
        let image = AttachmentPreview::read(
            &scope.checkout,
            AttachmentSource::Checkout {
                relative_path: "picture.png".into(),
            },
        )?;
        for (previews, expected) in [
            (vec![image], "qualified image/png limit"),
            (count_case.into(), "qualified limit of 1 attachments"),
            (vec![context_case], "after reserving 1000 for output"),
            (vec![encoded_case], "qualified limit is 600 bytes"),
        ] {
            let candidate = store
                .create_conversation(chat.checkout_id.clone(), expected.into())
                .await?;
            let plan = store
                .prepare_agent_attempt_with_attachments(
                    candidate.id.clone(),
                    candidate.context_id,
                    scope.clone(),
                    "Use my attachments".into(),
                    previews,
                )
                .await?;
            let run = plan.run.id.clone();
            frames.lock().unwrap().clear();
            assert_eq!(
                start_managed_turn(store.clone(), make_runner(&candidate.id), plan, None)
                    .wait()
                    .await?,
                RunStatus::Failed
            );
            assert!(
                store
                    .agent_dispatch(candidate.id.clone(), run.clone())
                    .await?
                    .is_none()
            );
            assert!(frames.lock().unwrap().iter().all(|frame| {
                serde_json::from_slice::<Value>(frame).unwrap()["method"] != "turn/start"
            }));
            let assessment = store
                .agent_observations(candidate.id, run, 0)
                .await?
                .into_iter()
                .find_map(|entry| match entry.event.data {
                    ProviderEventData::AttachmentAssessed { assessment } => Some(assessment),
                    _ => None,
                })
                .context("missing pre-dispatch attachment decisions")?;
            assert_eq!(assessment.qualified, Some(qualified.clone()));
            assert_eq!(
                assessment.qualification_id.as_deref(),
                Some(qualification.as_str())
            );
            assert_eq!(assessment.project, project_limits);
            assert!(
                assessment
                    .blockers
                    .iter()
                    .any(|entry| entry.reason.contains(expected)),
                "{assessment:?}"
            );
            if expected.contains("600") {
                assert!(assessment.encoded_request_bytes.unwrap() > 600);
            }
        }

        let candidate = store
            .create_conversation(chat.checkout_id.clone(), "Switched profile".into())
            .await?;
        let preview = store
            .preview_attachment(chat.checkout_id.clone(), escaped.clone(), range(250))
            .await?;
        let mut switched_scope = scope;
        switched_scope.profile_id = "another-profile".into();
        let plan = store
            .prepare_agent_attempt_with_attachments(
                candidate.id.clone(),
                candidate.context_id,
                switched_scope.clone(),
                "Use the excerpt".into(),
                vec![preview],
            )
            .await?;
        let run = plan.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), make_runner(&candidate.id), plan, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let saved = store
            .agent_dispatch(candidate.id.clone(), run.clone())
            .await?
            .unwrap();
        let events = store.agent_observations(candidate.id, run, 0).await?;
        let assessed = events
            .iter()
            .find(|entry| {
                matches!(
                    entry.event.data,
                    ProviderEventData::AttachmentAssessed { .. }
                )
            })
            .unwrap();
        let submitted = events
            .iter()
            .find(|entry| {
                matches!(
                    entry.event.data,
                    ProviderEventData::DispatchSubmitted { .. }
                )
            })
            .unwrap();
        assert!(assessed.seq < submitted.seq);
        let ProviderEventData::AttachmentAssessed { assessment } = &assessed.event.data else {
            unreachable!()
        };
        assert!(assessment.qualified.is_none());
        assert_eq!(
            assessment.encoded_request_bytes,
            Some(saved.frames[0].len() as u64)
        );

        let candidate = store
            .create_conversation(
                chat.checkout_id.clone(),
                "Limits updated after admission".into(),
            )
            .await?;
        let preview = store
            .preview_attachment(chat.checkout_id, escaped, range(3))
            .await?;
        let plan = store
            .prepare_agent_attempt_with_attachments(
                candidate.id.clone(),
                candidate.context_id,
                switched_scope,
                "Use the excerpt".into(),
                vec![preview],
            )
            .await?;
        let run = plan.run.id.clone();
        store
            .set_project_attachment_limits(
                project.project_id,
                AttachmentLimits {
                    files_per_turn: 0,
                    ..project_limits
                },
            )
            .await?;
        assert_eq!(
            start_managed_turn(store.clone(), make_runner(&candidate.id), plan, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        let events = store.agent_observations(candidate.id, run, 0).await?;
        let assessment = events
            .into_iter()
            .find_map(|entry| match entry.event.data {
                ProviderEventData::AttachmentAssessed { assessment } => Some(assessment),
                _ => None,
            })
            .context("updated project limit was not assessed")?;
        assert_eq!(assessment.project.files_per_turn, 0);
        assert!(
            assessment.blockers[0]
                .reason
                .contains("project limit of 0 attachments")
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduled_roles_recheck_required_skill_evidence_before_sending_a_prompt() -> Result<()>
    {
        use crate::jobs::{
            AdmissionBlocker, AdmissionEnvironment, DispatchRejection, JobContext, JobPermission,
            JobRequest, JobRequirements, JobRole, JobRule, JobWorkspace, PipelineDispatch,
            PipelineNodeKind, RequiredCustomization,
        };
        use crate::providers::skills::SkillEntry;

        let (_dir, store, conversation, scope) = setup().await?;
        std::fs::write(&scope.executable, b"fixture")?;
        let skill = scope.checkout.join(".agents/skills/fixture/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap())?;
        std::fs::write(&skill, "Inspect this checkout.")?;
        let eligible = RequiredCustomization::Skill {
            id: SkillEntry::identity(scope.provider, &skill)?,
        };
        let disabled = RequiredCustomization::Skill {
            id: SkillEntry::identity(
                scope.provider,
                &scope.checkout.join("other-profile/skills/fixture/SKILL.md"),
            )?,
        };
        let layout = crate::repository::register(&store, scope.checkout.clone()).await?;
        let project = layout.project_id;
        let env = AdmissionEnvironment {
            runner: scope.runner.clone(),
            bridge_available: false,
        };
        store.set_job_queue_capacity(1).await?;
        let rule = |role, requirement| JobRule {
            role,
            operation: SessionMode::ReadOnly,
            workspace: JobWorkspace::Project,
            permission: JobPermission::Allow,
            requirements: JobRequirements {
                capabilities: vec![],
                customizations: vec![requirement],
            },
        };
        store
            .set_project_job_rules(
                project.clone(),
                vec![],
                vec![
                    rule(JobRole::Operator, eligible.clone()),
                    rule(JobRole::Pipeline, disabled.clone()),
                ],
            )
            .await?;

        let enqueue = |context_id, text: &'static str| {
            let (store, scope, conversation_id) =
                (store.clone(), scope.clone(), conversation.id.clone());
            async move {
                store
                    .save_conversation_draft(conversation_id.clone(), text.into())
                    .await?;
                store
                    .enqueue_conversation_turn(
                        JobContext {
                            conversation_id,
                            context_id,
                        },
                        scope,
                        text.into(),
                    )
                    .await
            }
        };
        let accepted = enqueue(conversation.context_id, "Inspect with the verified skill").await?;
        let stage = store
            .create_conversation(
                layout.checkout_id.clone(),
                "Stage with an unavailable skill".into(),
            )
            .await?;
        let pipeline = "fixture-requirements".to_owned();
        let revision = "fixture-plan".to_owned();
        store
            .register_job_pipeline(project.clone(), pipeline.clone())
            .await?;
        store
            .register_job_pipeline_node(
                project.clone(),
                pipeline.clone(),
                "stage".into(),
                PipelineNodeKind::Execution,
            )
            .await?;
        store
            .set_job_pipeline_plan(project.clone(), pipeline.clone(), None, revision.clone())
            .await?;
        let missing = store
            .submit_pinned_pipeline_agent_job(
                PipelineDispatch {
                    pipeline_run_id: pipeline,
                    node_instance_id: "stage".into(),
                    plan_revision: Some(revision),
                    release_id: None,
                },
                scope.clone(),
                JobRequest {
                    checkout_id: Some(layout.checkout_id),
                    task_id: None,
                    mode: SessionMode::ReadOnly,
                    instructions: "Inspect with the disabled skill".into(),
                    requires_bridge: false,
                    dependencies: vec![],
                },
                JobContext {
                    conversation_id: stage.id.clone(),
                    context_id: stage.context_id,
                },
                QuestionRoute::Native,
            )
            .await?;

        let frames = Arc::new(Mutex::new(Vec::new()));
        let runner = |conversation: &str| {
            Arc::new(FixtureRunner(WireLog {
                store: store.clone(),
                runtime: tokio::runtime::Handle::current(),
                conversation: conversation.into(),
                frames: frames.clone(),
            }))
        };
        let first = store
            .claim_next_job(env.clone())
            .await?
            .context("the eligible job was not claimed")?;
        assert_eq!(first.conversation_id, conversation.id);
        let run = first.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner(&conversation.id), first, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let record = store
            .inspect_job(project.clone(), accepted.id, env.clone())
            .await?
            .unwrap();
        assert!(record.blockers.is_empty());
        let dispatch = store
            .agent_dispatch(conversation.id.clone(), run)
            .await?
            .context("the verified skill did not dispatch")?;
        assert_eq!(dispatch.delivery, Delivery::Submitted);
        assert!(
            dispatch
                .snapshot
                .filesystem_skills
                .iter()
                .any(|entry| entry.path == skill && entry.revision.is_some())
        );

        let blocked = store
            .claim_next_job(env.clone())
            .await?
            .context("missing requirement was not checked")?;
        assert_eq!(blocked.conversation_id, stage.id);
        let run = blocked.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner(&stage.id), blocked, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        let rejection = DispatchRejection::Requirements {
            capabilities: vec![],
            customizations: vec![disabled],
        };
        let detail = store
            .inspect_job(project.clone(), missing.id, env.clone())
            .await?
            .unwrap();
        assert_eq!(
            detail.blockers,
            [AdmissionBlocker::DispatchRejected {
                rejection: rejection.clone()
            }]
        );
        assert_eq!(
            store
                .job_failure(project.clone(), run.clone())
                .await?
                .unwrap()
                .dispatch_rejection,
            Some(rejection)
        );
        assert!(store.agent_dispatch(stage.id, run).await?.is_none());

        let context = store
            .conversation(conversation.id.clone())
            .await?
            .context_id;
        let changed = enqueue(context, "Inspect again after changing the checkout").await?;
        let second = store
            .claim_next_job(env.clone())
            .await?
            .context("the second operator job was not claimed")?;
        std::fs::remove_file(&skill)?;
        let run = second.run.id.clone();
        assert_eq!(
            start_managed_turn(store.clone(), runner(&conversation.id), second, None)
                .wait()
                .await?,
            RunStatus::Failed
        );
        let rejection = DispatchRejection::Requirements {
            capabilities: vec![],
            customizations: vec![eligible],
        };
        let detail = store
            .inspect_job(project.clone(), changed.id, env.clone())
            .await?
            .unwrap();
        assert_eq!(
            detail.blockers,
            [AdmissionBlocker::DispatchRejected { rejection }]
        );
        assert!(store.agent_dispatch(conversation.id, run).await?.is_none());
        assert_eq!(store.inspect_project_queue(project, env).await?.active, 0);
        assert_eq!(
            frames
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| serde_json::from_slice::<Value>(frame)
                    .is_ok_and(|frame| frame["method"] == "turn/start"))
                .count(),
            1
        );
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
