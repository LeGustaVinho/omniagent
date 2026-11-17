use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    conversations::{
        ConversationPipeline, MessageRevision, MessageRole, append_pipeline_message,
        consume_matching_draft, context_entries, load_conversation, load_conversation_pipeline,
        load_messages,
        reference_delivery::{bind_references, consume_draft_references, verify_draft_references},
        references::load_conversation_reference,
    },
    jobs::PipelineDispatch,
    model_catalog::ModelSelection,
    protection,
    providers::{
        SessionMode,
        composer_customizations::{is_composer_marker, without_composer_markers},
    },
    runner::DirectWindowsRunner,
    store::Store,
};

use super::{
    BuiltinNode, Executor, NodeConfiguration, PinnedNode, WorkflowDefinition, WorkflowLaunchOrigin,
    WorkflowRevision, WorkflowStartSchema,
    launch::{LaunchEntry, launch_entry_in_tx, prepare_entry},
    runtime::{load_definition, load_workflow_run},
};

const MAX_MESSAGE_BYTES: usize = 96 * 1024;
const MAX_CONTEXT_BYTES: usize = 192 * 1024;

mod dispatch;
#[cfg(test)]
mod tests;
pub(crate) use dispatch::prepare_next_conversation_stage;
use dispatch::{RunningTurn, TurnState};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPipelineInputs {
    pub document_revision_id: Option<String>,
    pub work_id: Option<String>,
    pub agile_event_id: Option<String>,
    #[serde(default)]
    pub values: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectedStageAction {
    node_id: String,
    marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationPipelineTurnRequest {
    conversation_id: String,
    request_id: String,
    text: String,
    submitted_draft: Option<String>,
    inputs: Option<ConversationPipelineInputs>,
    selection: Option<ModelSelection>,
    effort: Option<String>,
    #[serde(default)]
    reference_ids: Vec<String>,
    action: Option<SelectedStageAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedRequest {
    request: ConversationPipelineTurnRequest,
    #[serde(default)]
    request_fingerprint: String,
    inputs: ConversationPipelineInputs,
    workflow: WorkflowRevision,
    checkout_id: String,
    project_id: String,
}

struct Turn {
    sequence: i64,
    request: SavedRequest,
    state: String,
    message_revision_id: Option<String>,
    history_context_id: Option<String>,
    run_id: Option<String>,
    result_revision_id: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPipelineTurn {
    request_id: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    submitted_draft: Option<String>,
    inputs: ConversationPipelineInputs,
    selection: Option<ModelSelection>,
    effort: Option<String>,
    state: String,
    run_id: Option<String>,
    message_revision_id: Option<String>,
    result_revision_id: Option<String>,
    waiting: Option<dispatch::TurnWait>,
    artifacts: Vec<dispatch::TurnArtifact>,
    action_outcome: Option<dispatch::ActionOutcome>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationPipelineEvent {
    id: String,
    work_id: String,
    label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConversationPipelineAgent {
    node_id: String,
    label: String,
    selection: Option<ModelSelection>,
    effort: Option<String>,
    mode: SessionMode,
    action_eligible: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPipelineSchema {
    fields: Vec<Value>,
    requires_document: bool,
    requires_work: bool,
    requires_event: bool,
    requires_model: bool,
    events: Vec<ConversationPipelineEvent>,
    agents: Vec<ConversationPipelineAgent>,
}

impl Store {
    pub async fn conversation_pipeline_schema(
        &self,
        conversation: String,
    ) -> Result<ConversationPipelineSchema> {
        self.execute(move |db| {
            let (project, _, workflow) = scope(db, &conversation)?;
            let definition = load_definition(db, &project, &workflow)?
                .context("The pinned pipeline revision is unavailable")?;
            let schema = WorkflowStartSchema::for_definition(&definition);
            let requires_document = schema
                .as_ref()
                .is_none_or(|schema| schema.requires_document);
            let requires_event = schema.as_ref().is_some_and(|schema| schema.requires_event);
            let requires_work =
                requires_event || schema.as_ref().is_some_and(|schema| schema.requires_work);
            let fields = schema
                .map(|schema| {
                    let value = serde_json::to_value(schema)?;
                    Ok::<_, anyhow::Error>(
                        value
                            .get("fields")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    )
                })
                .transpose()?
                .unwrap_or_default();
            let events = if requires_event {
                available_events(db, &project)?
            } else {
                Vec::new()
            };
            let agents = agents(db, &project, &definition)?;
            let requires_model = agents.iter().any(|agent| agent.selection.is_none());
            Ok(ConversationPipelineSchema {
                fields,
                requires_document,
                requires_work,
                requires_event,
                requires_model,
                events,
                agents,
            })
        })
        .await
    }

    pub async fn enqueue_conversation_pipeline_turn(
        &self,
        request: ConversationPipelineTurnRequest,
    ) -> Result<ConversationPipelineTurn> {
        ensure!(
            !request.request_id.trim().is_empty() && request.request_id.len() <= 256,
            "The conversation turn identity is invalid"
        );
        ensure!(
            !request.text.trim().is_empty() && request.text.len() <= MAX_MESSAGE_BYTES,
            "Enter a message within the pipeline's input limit"
        );
        if let Some(action) = &request.action
            && let Some((checkout, agent)) = self.conversation_action_target(&request).await?
        {
            let (selection, effort) = if let Some(selection) = agent.selection {
                (selection, agent.effort)
            } else {
                (
                    request
                        .selection
                        .clone()
                        .context("Choose a model for the selected Agent stage")?,
                    request.effort.clone(),
                )
            };
            let scope = dispatch::qualified_stage_scope(
                self,
                checkout,
                selection,
                effort,
                agent.mode,
                DirectWindowsRunner::IDENTITY.into(),
            )
            .await?;
            let actions = self
                .composer_customizations(request.conversation_id.clone(), scope)
                .await?;
            ensure!(
                actions
                    .iter()
                    .any(|option| option.marker == action.marker
                        && option.unavailable_reason.is_none()),
                "The selected action is unavailable for the stage's current CLI profile. Select it again."
            );
        }
        let result = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous = tx.query_row(
                "SELECT sequence FROM conversation_pipeline_turns WHERE conversation_id = ?1 AND request_id = ?2",
                params![request.conversation_id, request.request_id],
                |row| row.get::<_, i64>(0),
            ).optional()?;
            if let Some(sequence) = previous {
                let turn = load_turn(&tx, sequence)?;
                ensure!(turn.request.matches(&request)?, "This turn identity already belongs to a different message");
                consume_replayed_draft(&tx, &request)?;
                let outcome = project_turn(&tx, turn)?;
                tx.commit()?;
                return Ok(outcome);
            }
            let (project_id, checkout_id, workflow) = scope(&tx, &request.conversation_id)?;
            let inputs = match &request.inputs {
                Some(inputs) => inputs.clone(),
                None => last_inputs(&tx, &request.conversation_id)?.unwrap_or_default(),
            };
            let saved = SavedRequest {
                inputs, workflow, project_id, checkout_id,
                request_fingerprint: request_fingerprint(&request)?,
                request,
            };
            let entry = turn_entry(&tx, &saved, &[])?;
            let prepared = prepare_entry(&tx, &saved.project_id, &saved.workflow, &entry, &origin(&saved))?;
            let agents = agents(&tx, &saved.project_id, &prepared.workflow)?;
            ensure!(
                saved.request.selection.is_some() || agents.iter().all(|agent| agent.selection.is_some()),
                "Choose a default model for pipeline stages without a pinned model"
            );
            if let Some(action) = &saved.request.action {
                ensure!(
                    agents.iter().any(|agent| agent.node_id == action.node_id && agent.action_eligible),
                    "Select an Agent stage that runs once in the conversation checkout"
                );
                ensure!(is_composer_marker(&action.marker), "The selected pipeline action has an invalid identity");
            }
            verify_draft_references(&tx, &saved.request.conversation_id, &saved.project_id, &saved.request.reference_ids)?;
            let attachments: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_attachment_drafts WHERE conversation_id = ?1)",
                [&saved.request.conversation_id],
                |row| row.get(0),
            )?;
            ensure!(!attachments, "Remove draft attachments before sending this pipeline turn");
            let current_draft: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM drafts WHERE kind = 'conversation' AND owner_id = ?1 AND document = '')",
                [&saved.request.conversation_id],
                |row| row.get(0),
            )?;
            if current_draft {
                consume_matching_draft(
                    &tx,
                    &saved.request.conversation_id,
                    saved.request.submitted_draft.as_deref().unwrap_or(&saved.request.text),
                )?;
            }
            consume_draft_references(&tx, &saved.request.conversation_id)?;
            tx.execute(
                "INSERT INTO conversation_pipeline_turns(conversation_id, request_id, payload) VALUES (?1, ?2, ?3)",
                params![saved.request.conversation_id, saved.request.request_id, protection::protect(&serde_json::to_vec(&saved)?)?],
            )?;
            let turn = load_turn(&tx, tx.last_insert_rowid())?;
            let result = project_turn(&tx, turn)?;
            tx.commit()?;
            Ok(result)
        }).await?;
        self.job_signal.send_replace(());
        Ok(result)
    }

    async fn conversation_action_target(
        &self,
        request: &ConversationPipelineTurnRequest,
    ) -> Result<Option<(String, ConversationPipelineAgent)>> {
        let request = request.clone();
        self.execute(move |db| {
            let stored: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_pipeline_turns WHERE conversation_id = ?1 AND request_id = ?2)",
                params![request.conversation_id, request.request_id],
                |row| row.get(0),
            )?;
            if stored {
                return Ok(None);
            }
            let action = request.action.context("The selected pipeline action is missing")?;
            ensure!(is_composer_marker(&action.marker), "The selected pipeline action has an invalid identity");
            let (project, checkout, workflow) = scope(db, &request.conversation_id)?;
            let definition = load_definition(db, &project, &workflow)?
                .context("The pinned pipeline revision is unavailable")?;
            let agent = agents(db, &project, &definition)?.into_iter()
                .find(|agent| agent.node_id == action.node_id && agent.action_eligible)
                .context("Select an Agent stage that runs once in the conversation checkout")?;
            Ok(Some((checkout, agent)))
        }).await
    }

    pub async fn conversation_pipeline_turns(
        &self,
        conversation: String,
    ) -> Result<Vec<ConversationPipelineTurn>> {
        self.execute(move |db| {
            load_conversation(db, &conversation)?;
            let ids = db.prepare("SELECT sequence FROM conversation_pipeline_turns WHERE conversation_id = ?1 ORDER BY sequence")?
                .query_map([&conversation], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            ids.into_iter().map(|id| project_turn(db, load_turn(db, id)?)).collect()
        }).await
    }

    pub(crate) async fn advance_conversation_pipeline_turns(&self) -> Result<bool> {
        self.execute(|db| {
            let mut tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let running = tx.prepare("SELECT sequence FROM conversation_pipeline_turns WHERE state = 'running' ORDER BY sequence")?
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut progressed = false;
            for id in running {
                progressed |= settle_turn(&tx, load_turn(&tx, id)?)?;
            }
            let next: Option<i64> = tx.query_row(
                "SELECT t.sequence FROM conversation_pipeline_turns t WHERE t.state = 'queued'
                 AND NOT EXISTS(SELECT 1 FROM conversation_pipeline_turns older
                    WHERE older.conversation_id = t.conversation_id AND older.sequence < t.sequence
                    AND older.state IN ('queued', 'running'))
                 AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY t.sequence LIMIT 1",
                [],
                |row| row.get(0),
            ).optional()?;
            if let Some(sequence) = next {
                let mut launch = tx.savepoint()?;
                let turn = load_turn(&launch, sequence)?;
                match start_turn(&launch, &turn) {
                    Ok(()) => launch.commit()?,
                    Err(error) => {
                        launch.rollback()?;
                        launch.commit()?;
                        tx.execute(
                            "UPDATE conversation_pipeline_turns SET state = 'failed', error = ?2 WHERE sequence = ?1",
                            params![sequence, crate::diagnostics::preview(&error.to_string())],
                        )?;
                    }
                }
                progressed = true;
            }
            tx.commit()?;
            Ok(progressed)
        }).await
    }
}

fn scope(db: &Connection, conversation: &str) -> Result<(String, String, WorkflowRevision)> {
    let chat = load_conversation(db, conversation)?;
    let workflow = match load_conversation_pipeline(db, conversation)? {
        ConversationPipeline::Direct => anyhow::bail!("This conversation uses direct turns"),
        ConversationPipeline::RepositoryDelivery { workflow }
        | ConversationPipeline::Published { workflow, .. } => workflow,
    };
    let project = db.query_row(
        "SELECT project_id FROM checkouts WHERE id = ?1",
        [&chat.checkout_id],
        |row| row.get(0),
    )?;
    Ok((project, chat.checkout_id, workflow))
}

fn consume_replayed_draft(
    db: &Connection,
    request: &ConversationPipelineTurnRequest,
) -> Result<()> {
    let draft: Option<Vec<u8>> = db.query_row(
        "SELECT d.payload FROM drafts d WHERE d.kind = 'conversation' AND d.owner_id = ?1 AND d.document = ''
           AND NOT EXISTS (SELECT 1 FROM conversation_reference_drafts r WHERE r.conversation_id = d.owner_id)
           AND NOT EXISTS (SELECT 1 FROM conversation_attachment_drafts a WHERE a.conversation_id = d.owner_id)",
        [&request.conversation_id],
        |row| row.get(0),
    ).optional()?;
    let submitted = request.submitted_draft.as_deref().unwrap_or(&request.text);
    if let Some(draft) = draft
        && protection::unprotect(&draft)? == submitted.as_bytes()
    {
        consume_matching_draft(db, &request.conversation_id, submitted)?;
    }
    Ok(())
}

fn available_events(db: &Connection, project: &str) -> Result<Vec<ConversationPipelineEvent>> {
    Ok(db
        .prepare(
            "SELECT e.id, e.task_id, work.title, e.kind FROM project_work_events e
         JOIN project_tasks work ON work.id = e.task_id AND work.project_id = e.project_id
         WHERE e.project_id = ?1 ORDER BY e.sequence DESC LIMIT 200",
        )?
        .query_map([project], |row| {
            let title: String = row.get(2)?;
            let kind: String = row.get(3)?;
            Ok(ConversationPipelineEvent {
                id: row.get(0)?,
                work_id: row.get(1)?,
                label: format!("{title} · {}", kind.replace('_', " ")),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn agents(
    db: &Connection,
    project: &str,
    definition: &WorkflowDefinition,
) -> Result<Vec<ConversationPipelineAgent>> {
    let units = super::control::unit_nodes(definition);
    definition
        .nodes
        .iter()
        .filter_map(|node| {
            if !matches!(
                node.definition.executor,
                Executor::PlanAgent | Executor::ReadOnlyAgent
            ) {
                return None;
            }
            Some((|| {
                let (selection, effort) = dispatch::pinned_model(db, project, node)?;
                let mode = super::stage_plan::StagePlanConfiguration::for_stage(node)?
                    .map(|plan| plan.mode())
                    .unwrap_or(if node.definition.executor == Executor::PlanAgent {
                        SessionMode::Plan
                    } else {
                        SessionMode::ReadOnly
                    });
                let action_eligible = !units.contains(node.instance.id.as_str())
                    && !matches!(
                        node.instance.kind,
                        BuiltinNode::ReviewCandidate | BuiltinNode::ReviewDiffCandidate
                    );
                Ok(ConversationPipelineAgent {
                    node_id: node.instance.id.clone(),
                    label: stage_label(node),
                    selection,
                    effort,
                    mode,
                    action_eligible,
                })
            })())
        })
        .collect()
}

fn stage_label(node: &PinnedNode) -> String {
    if node.instance.kind == BuiltinNode::Agent
        && let NodeConfiguration::AgentRole { role, .. } = &node.instance.configuration
    {
        return role.clone();
    }
    node.definition.title.clone()
}

fn last_inputs(db: &Connection, conversation: &str) -> Result<Option<ConversationPipelineInputs>> {
    let protected: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM conversation_pipeline_turns WHERE conversation_id = ?1 AND state <> 'failed'
         ORDER BY sequence DESC LIMIT 1",
        [conversation], |row| row.get(0),
    ).optional()?;
    protected
        .map(|payload| Ok(open_request(&payload)?.inputs))
        .transpose()
}

fn open_request(payload: &[u8]) -> Result<SavedRequest> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}

fn request_fingerprint(request: &ConversationPipelineTurnRequest) -> Result<String> {
    Ok(super::digest(&serde_json::to_vec(request)?))
}

impl SavedRequest {
    fn matches(&self, request: &ConversationPipelineTurnRequest) -> Result<bool> {
        if self.request_fingerprint.is_empty() {
            return Ok(&self.request == request);
        }
        Ok(self.request_fingerprint == request_fingerprint(request)?)
    }
}

fn load_turn(db: &Connection, sequence: i64) -> Result<Turn> {
    let (payload, state, message_revision_id, history_context_id, run_id, result_revision_id, error) = db.query_row(
        "SELECT payload, state, message_revision_id, history_context_id, workflow_run_id, result_revision_id, error
         FROM conversation_pipeline_turns WHERE sequence = ?1",
        [sequence],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    )?;
    Ok(Turn {
        sequence,
        request: open_request(&payload)?,
        state,
        message_revision_id,
        history_context_id,
        run_id,
        result_revision_id,
        error,
    })
}

fn origin(saved: &SavedRequest) -> WorkflowLaunchOrigin {
    WorkflowLaunchOrigin::Conversation {
        conversation_id: saved.request.conversation_id.clone(),
        request_id: saved.request.request_id.clone(),
        work_id: saved.inputs.work_id.clone(),
        event_id: saved.inputs.agile_event_id.clone(),
    }
}

fn turn_entry(
    db: &Connection,
    saved: &SavedRequest,
    history: &[MessageRevision],
) -> Result<LaunchEntry> {
    let definition = load_definition(db, &saved.project_id, &saved.workflow)?
        .context("The pinned pipeline revision is unavailable")?;
    let values = match WorkflowStartSchema::for_definition(&definition) {
        Some(schema) => {
            schema.conversation_inputs(saved.inputs.values.clone(), &saved.request.text, history)?
        }
        None => saved.inputs.values.clone(),
    };
    Ok(LaunchEntry {
        checkout_id: saved.checkout_id.clone(),
        document_revision_id: saved.inputs.document_revision_id.clone(),
        inputs: values,
    })
}

fn start_turn(db: &Connection, turn: &Turn) -> Result<()> {
    let saved = &turn.request;
    let conversation = &saved.request.conversation_id;
    let previous = load_conversation(db, conversation)?.context_id;
    let history = load_messages(
        db,
        conversation,
        &context_entries(db, conversation, &previous)?,
    )?;
    for reference in &saved.request.reference_ids {
        load_conversation_reference(db, conversation, &saved.project_id, reference)?;
    }
    let run = launch_entry_in_tx(
        db,
        &saved.project_id,
        &saved.workflow,
        turn_entry(db, saved, &history)?,
        &origin(saved),
    )?;
    let (revision, _) =
        append_pipeline_message(db, conversation, MessageRole::User, &saved.request.text)?;
    bind_references(db, &revision, &saved.request.reference_ids)?;
    let mut saved = saved.clone();
    if saved.request_fingerprint.is_empty() {
        saved.request_fingerprint = request_fingerprint(&saved.request)?;
    }
    saved.request.text.clear();
    saved.request.submitted_draft = None;
    db.execute(
        "UPDATE conversation_pipeline_turns SET state = 'running', history_context_id = ?2,
           message_revision_id = ?3, workflow_run_id = ?4, payload = ?5 WHERE sequence = ?1",
        params![
            turn.sequence,
            previous,
            revision,
            run.id,
            protection::protect(&serde_json::to_vec(&saved)?)?
        ],
    )?;
    Ok(())
}

fn project_turn(db: &Connection, turn: Turn) -> Result<ConversationPipelineTurn> {
    let mut live = None;
    if let Some(run_id) = &turn.run_id {
        live = load_workflow_run(db, &turn.request.project_id, run_id)?
            .map(|run| RunningTurn::read(db, &turn, &run))
            .transpose()?;
    }
    let (waiting, artifacts, action_outcome) = live
        .map(|state| {
            (
                if turn.state == "running" {
                    state.waiting
                } else {
                    None
                },
                state.artifacts,
                state.action_outcome,
            )
        })
        .unwrap_or_else(|| {
            (
                None,
                Vec::new(),
                if turn.state == "failed" {
                    turn.request
                        .request
                        .action
                        .as_ref()
                        .map(|action| dispatch::ActionOutcome::not_executed(&action.node_id))
                } else {
                    None
                },
            )
        });
    let text = if let Some(revision) = &turn.message_revision_id {
        load_turn_message(db, &turn.request.request.conversation_id, revision)?.text
    } else {
        turn.request.request.text
    };
    Ok(ConversationPipelineTurn {
        request_id: turn.request.request.request_id,
        text,
        submitted_draft: if turn.state == "failed" && turn.message_revision_id.is_none() {
            turn.request.request.submitted_draft
        } else {
            None
        },
        inputs: turn.request.inputs,
        selection: turn.request.request.selection,
        effort: turn.request.request.effort,
        state: turn.state,
        run_id: turn.run_id,
        message_revision_id: turn.message_revision_id,
        result_revision_id: turn.result_revision_id,
        waiting,
        artifacts,
        action_outcome,
        error: turn.error,
    })
}

fn load_turn_message(
    db: &Connection,
    conversation: &str,
    revision: &str,
) -> Result<MessageRevision> {
    load_messages(db, conversation, &[revision.to_owned()])?
        .into_iter()
        .next()
        .context("The turn's user message is unavailable")
}

fn settle_turn(db: &Connection, turn: Turn) -> Result<bool> {
    let run_id = turn
        .run_id
        .as_deref()
        .context("The running conversation turn has no pipeline run")?;
    let run = load_workflow_run(db, &turn.request.project_id, run_id)?
        .context("The conversation turn's pinned run is unavailable")?;
    super::control::refresh(db, &run.project_id, &run.id)?;
    let running = RunningTurn::read(db, &turn, &run)?;
    let (status, result, error) = match running.state {
        TurnState::Running => return Ok(false),
        TurnState::Failed(reason) => ("failed", None, Some(reason)),
        TurnState::Completed(summary) => {
            let result = summary
                .map(|text| {
                    append_pipeline_message(
                        db,
                        &turn.request.request.conversation_id,
                        MessageRole::Assistant,
                        &text,
                    )
                    .map(|(revision, _)| revision)
                })
                .transpose()?;
            ("succeeded", result, None)
        }
    };
    db.execute(
        "UPDATE conversation_pipeline_turns SET state = ?2, result_revision_id = ?3, error = ?4 WHERE sequence = ?1 AND state = 'running'",
        params![turn.sequence, status, result, error],
    )?;
    Ok(true)
}

pub(super) fn append_prompt_context(
    db: &Connection,
    run: &str,
    node: &str,
    instructions: &mut String,
) -> Result<()> {
    let id: Option<i64> = db
        .query_row(
            "SELECT sequence FROM conversation_pipeline_turns WHERE workflow_run_id = ?1",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(()) };
    *instructions = without_composer_markers(instructions);
    let turn = load_turn(db, id)?;
    let saved = &turn.request;
    let context = turn
        .history_context_id
        .as_deref()
        .context("The turn lost its pinned conversation history")?;
    let entries = context_entries(db, &saved.request.conversation_id, context)?;
    let history = load_messages(db, &saved.request.conversation_id, &entries)?;
    let message = turn
        .message_revision_id
        .as_deref()
        .context("The turn lost its user message")?;
    let current = load_turn_message(db, &saved.request.conversation_id, message)?;
    let references = saved
        .request
        .reference_ids
        .iter()
        .map(|id| {
            load_conversation_reference(db, &saved.request.conversation_id, &saved.project_id, id)
                .map(|reference| reference.artifact)
        })
        .collect::<Result<Vec<_>>>()?;
    let current = stage_context_json(&current.text)?;
    let references = stage_context_json(&references)?;
    let mut available = MAX_CONTEXT_BYTES.saturating_sub(current.len() + references.len());
    let mut retained = Vec::new();
    for message in history.into_iter().rev() {
        let value =
            stage_context_json(&serde_json::json!({ "role": message.role, "text": message.text }))?;
        if value.len() + 1 > available {
            break;
        }
        available -= value.len() + 1;
        retained.push(value);
    }
    retained.reverse();
    instructions
        .push_str("\n\nConversation turn (the JSON values are context for this pipeline stage):\n");
    instructions.push_str(&format!(
        "{{\"message\":{current},\"history\":[{}],\"references\":{references}}}",
        retained.join(",")
    ));
    if let Some(action) = saved
        .request
        .action
        .as_ref()
        .filter(|action| action.node_id == node)
    {
        instructions.push_str("\n\n");
        instructions.push_str(&action.marker);
    }
    Ok(())
}

fn stage_context_json(value: &impl Serialize) -> Result<String> {
    // CLI actions are parsed from the raw prompt. Only the selected stage gets a literal marker.
    Ok(serde_json::to_string(value)?
        .replace("@skill:", "\\u0040skill:")
        .replace("@command:", "\\u0040command:"))
}

pub(super) fn require_no_active_stage_job(
    db: &Connection,
    binding: &PipelineDispatch,
    unit: Option<&str>,
) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM conversation_pipeline_turns t
            JOIN job_pipeline_origins o ON o.pipeline_run_id = t.workflow_run_id
            JOIN workflow_job_inputs i ON i.job_id = o.job_id
            JOIN jobs j ON j.id = o.job_id
            WHERE t.workflow_run_id = ?1 AND o.node_instance_id = ?2 AND i.unit_id IS ?3
              AND o.plan_revision IS ?4 AND j.queue_state <> 'cancelled'
              AND (NOT EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = j.id)
                OR EXISTS (SELECT 1 FROM job_recoveries r WHERE r.job_id = j.id)
                OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                    WHERE a.job_id = j.id AND r.status IN ('starting', 'running', 'waiting_for_input')))
        )",
        params![binding.pipeline_run_id, binding.node_instance_id, unit, binding.plan_revision],
        |row| row.get(0),
    )?;
    ensure!(
        !exists,
        "This conversation pipeline stage already has an active job"
    );
    Ok(())
}
