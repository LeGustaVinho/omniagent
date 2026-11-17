use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    attachments::AttachmentDispatch,
    context_records::assembly::RoleDispatchContext,
    conversations::{
        MessageRevision, SessionScope, SubmittedConversationReference, reference_delivery,
    },
    protection,
    providers::{
        ProviderEvent,
        capabilities::CapabilityAssessment,
        configuration::InstructionInventory,
        launch_scope::ScopedLaunchEvidence,
        plugins::PluginInventory,
        questions::{QuestionOrigin, QuestionRoute},
        skills::{SkillCatalog, SkillEntry, SkillRequest},
    },
    repository::RepositoryMarkdown,
    store::Store,
    tasks::TaskDispatchContext,
};

mod attachment_evidence;
mod attachment_retention;
mod generated_image_evidence;
pub(crate) use generated_image_evidence::{
    is_current_native_image_qualification, latest_native_image_qualification,
};
mod history_deletion;
mod instruction_evidence;
mod inventory;
mod lineage;
mod profile;
mod qualification;
pub use attachment_evidence::{AttachmentEvidence, AttachmentRead, AttachmentTransport};
pub(crate) use attachment_evidence::{
    load_saved_native_attachment, saved_native_from_source, verify_saved_native_metadata,
};
pub(crate) use attachment_retention::revoked_history_run;
pub use attachment_retention::{
    AttachmentHistoryRemoval, AttachmentHistoryTarget, AttachmentRemovalPreview,
};
pub(crate) use history_deletion::omit_deleted_dispatch_messages;
pub use history_deletion::{
    HistoryDeletionDiff, HistoryDeletionMode, HistoryDeletionPreview, HistoryDeletionTarget,
};
pub use instruction_evidence::{
    InstructionDispatch, InstructionDispatchMode, InstructionDispatchStatus, InstructionReceipt,
    InstructionReview,
};
pub use inventory::DispatchInventory;
pub(crate) use inventory::belongs_to_dispatch;
pub use lineage::{
    ArtifactSource, ByteRange, ConversationArtifactRevision, ConversationSelection,
    DispatchIdentity, DispatchLineage, DispatchSourceRevision, DispatchUse, ExcerptSource,
    MessageRevisionOrigin, QueuedSummary, SelectedMessageSpan, SummaryRun,
};
pub(crate) use lineage::{
    enqueue_summary, load_conversation_artifact, read_background_dispatch_result,
    reconcile_summary, record_conversation_excerpt, record_message_copy,
};
pub use profile::DispatchProfileEvidence;
pub use qualification::{QualificationCursor, QualificationPage};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub version: u16,
    pub context_id: String,
    pub scope: SessionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_policy_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_context: Option<RoleDispatchContext>,
    pub base_commit: Option<String>,
    pub submitted_messages: Vec<MessageRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conversation_references: Vec<SubmittedConversationReference>,
    pub native_history: NativeHistory,
    #[serde(default)]
    pub capabilities: Option<CapabilityAssessment>,
    #[serde(default)]
    pub filesystem_skills: Vec<SkillEntry>,
    #[serde(default)]
    pub reported_skills: Option<SkillCatalog>,
    #[serde(default)]
    pub instructions: Option<InstructionInventory>,
    #[serde(default)]
    pub scoped_launch: Option<ScopedLaunchEvidence>,
    #[serde(default)]
    pub requested_skill: Option<SkillRequest>,
    #[serde(default)]
    pub plugin_inventory: Option<PluginInventory>,
    #[serde(default)]
    pub attachments: Vec<AttachmentDispatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_context: Option<TaskDispatchContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_markdown: Option<RepositoryMarkdown>,
    #[serde(default, skip_serializing_if = "QuestionRoute::is_native")]
    pub question_route: QuestionRoute,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_question: Option<QuestionOrigin>,
}

impl ContextSnapshot {
    fn working_directory(&self) -> &Path {
        self.instructions
            .as_ref()
            .map_or(self.scope.checkout.as_path(), |inventory| {
                &inventory.working_directory
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum NativeHistory {
    Fresh,
    ProviderCursor {
        session_id: String,
        context_id: String,
    },
    ProviderFork {
        session_id: String,
        source_run_id: String,
        source_turn_id: String,
        context_id: String,
    },
    ProviderRecovery {
        session_id: String,
        source_run_id: String,
        source_turn_id: Option<String>,
        context_id: String,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Prepared,
    Submitted,
    Unconfirmed,
}

impl Delivery {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Submitted => "submitted",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

pub struct DispatchEvidence {
    pub id: String,
    pub turn_id: String,
    pub snapshot: ContextSnapshot,
    pub frames: Vec<Vec<u8>>,
    pub delivery: Delivery,
    pub attachments: Vec<AttachmentEvidence>,
    pub removed_attachment_count: usize,
    pub history_payload_deleted: bool,
    pub instructions: Vec<InstructionDispatch>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Application,
    Provider,
    Supervisor,
    Unknown,
}

impl EventSource {
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Provider => "provider",
            Self::Supervisor => "supervisor",
            Self::Unknown => "unknown",
        }
    }
}

pub struct RecordedAgentEvent {
    pub seq: i64,
    pub source: EventSource,
    pub event: ProviderEvent,
}

impl Store {
    pub(crate) async fn prepare_dispatch(
        &self,
        run_id: String,
        mut snapshot: ContextSnapshot,
        frames: Vec<Vec<u8>>,
    ) -> Result<String> {
        ensure!(
            !frames.is_empty() && frames.iter().all(|frame| !frame.is_empty()),
            "dispatch requires exact protocol frames"
        );
        let frame_cipher = protection::protect(&serde_json::to_vec(&frames)?)?;
        let dispatched = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !crate::jobs::validate_attempt_dispatch(&tx, &run_id)? {
                tx.commit()?;
                return Ok(None);
            }
            let (context, turn, session, scope, project_id): (String, String, String, String, String) = tx.query_row(
                "SELECT a.context_id, a.turn_id, a.agent_session_id, s.scope, c.project_id
                 FROM agent_attempts a JOIN active_agent_attempts l ON l.run_id = a.run_id
                 JOIN agent_sessions s ON s.id = a.agent_session_id
                 JOIN runs r ON r.id = a.run_id JOIN checkouts c ON c.id = r.checkout_id
                 WHERE a.run_id = ?1",
                [&run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            ).context("dispatch requires an active attempt")?;
            ensure!(context == snapshot.context_id, "snapshot does not cover the admitted context");
            ensure!(serde_json::from_str::<SessionScope>(&scope)? == snapshot.scope,
                "snapshot does not use the admitted session scope");
            crate::jobs::validate_instruction_revisions(&tx, &run_id, &snapshot.submitted_messages)?;
            let message_revisions = snapshot.submitted_messages.iter().map(|message| message.revision_id.clone()).collect::<Vec<_>>();
            ensure!(reference_delivery::submitted_references(&tx, &run_id, &message_revisions)? == snapshot.conversation_references,
                "The attached conversation references changed before dispatch");
            for attachment in &snapshot.attachments {
                verify_saved_native_metadata(&tx, &project_id, &attachment.metadata)?;
            }
            snapshot.session_id = Some(session.clone());
            snapshot.job_policy_revision = tx.query_row(
                "SELECT policy_key FROM job_attempt_policies WHERE run_id = ?1",
                [&run_id], |row| row.get(0),
            ).optional()?;
            let snapshot_cipher = protection::protect(&serde_json::to_vec(&snapshot)?)?;
            let snapshot_id = Uuid::new_v4().to_string();
            let dispatch_id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO context_snapshots(id, run_id, context_id, payload) VALUES (?1, ?2, ?3, ?4)",
                params![snapshot_id, run_id, context, snapshot_cipher])?;
            tx.execute("INSERT INTO dispatches(id, turn_id, snapshot_id, ordinal, frames, delivery)
                        VALUES (?1, ?2, ?3, 1, ?4, 'prepared')", params![dispatch_id, turn, snapshot_id, frame_cipher])?;
            lineage::record(&tx, &dispatch_id, &run_id, &snapshot)?;
            tx.execute("UPDATE agent_sessions SET cursor_context_id = NULL WHERE id = ?1", [&session])?;
            if let Some(assessment) = snapshot.capabilities.as_ref() {
                crate::runtime_inspection::save_capability_summary(&tx, &run_id, assessment)?;
            }
            tx.commit()?;
            Ok(Some(dispatch_id))
        }).await?;
        dispatched.context("Job authorization changed before prompt dispatch")
    }

    pub(crate) async fn settle_dispatch(
        &self,
        dispatch_id: String,
        delivery: Delivery,
    ) -> Result<()> {
        ensure!(
            delivery != Delivery::Prepared,
            "must resolve delivery to a non-prepared state"
        );
        self.execute(move |db| {
            let updated = db.execute(
                "UPDATE dispatches SET delivery = ?1 WHERE id = ?2 AND delivery = 'prepared'",
                params![delivery.as_sql(), dispatch_id],
            )?;
            ensure!(updated == 1, "dispatch was already settled or is missing");
            Ok(())
        })
        .await
    }

    pub async fn agent_dispatch(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Option<DispatchEvidence>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let record = tx
                .query_row(
                    "SELECT d.id, d.turn_id, s.payload, d.frames, d.delivery FROM dispatches d
                 JOIN context_snapshots s ON s.id = d.snapshot_id
                 JOIN agent_attempts a ON a.run_id = s.run_id
                 WHERE s.run_id = ?1 AND a.conversation_id = ?2 AND d.ordinal = 1",
                    params![run_id, conversation_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Vec<u8>>(2)?,
                            row.get::<_, Vec<u8>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            record
                .map(|(id, turn_id, snapshot, frames, delivery)| {
                    let snapshot: ContextSnapshot =
                        serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
                    let delivery = serde_json::from_value(serde_json::Value::String(delivery))?;
                    let removed_ordinals = attachment_retention::dispatch_removed_ordinals(&tx, &id)?;
                    let attachments = attachment_evidence::load(
                        &tx, &run_id, &id, &snapshot, delivery, &removed_ordinals,
                    )?;
                    let instructions = instruction_evidence::load(
                        &tx,
                        &run_id,
                        &snapshot.submitted_messages,
                        delivery,
                    )?;
                    let history_payload_deleted = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM history_deleted_dispatch_payloads WHERE dispatch_id = ?1)",
                        [&id],
                        |row| row.get(0),
                    )?;
                    Ok(DispatchEvidence {
                        id,
                        turn_id,
                        snapshot,
                        frames: serde_json::from_slice(&protection::unprotect(&frames)?)?,
                        delivery,
                        attachments,
                        removed_attachment_count: removed_ordinals.len(),
                        history_payload_deleted,
                        instructions,
                    })
                })
                .transpose()
        })
        .await
    }

    pub async fn agent_observations(
        &self,
        conversation_id: String,
        run_id: String,
        after: i64,
    ) -> Result<Vec<RecordedAgentEvent>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT e.seq, e.source, e.payload FROM agent_events e JOIN agent_attempts a ON a.run_id = e.run_id
                 WHERE a.run_id = ?1 AND a.conversation_id = ?2 AND e.seq > ?3 ORDER BY e.seq LIMIT 128",
            )?;
            let rows = query.query_map(params![run_id, conversation_id, after], |row| {
                Ok((row.get(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?))
            })?;
            rows.map(|row| {
                let (seq, source, payload) = row?;
                Ok(RecordedAgentEvent {
                    seq, source:serde_json::from_value(serde_json::Value::String(source))?,
                    event:serde_json::from_slice(&protection::unprotect(&payload)?)?,
                })
            }).collect()
        }).await
    }
}
