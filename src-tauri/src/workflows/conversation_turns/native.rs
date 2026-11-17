use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{
    conversations::{MessageRevision, SessionScope, attempt_output},
    jobs::{Job, QueueState, load_job},
    protection,
    runs::RunStatus,
    workflows::{Endpoint, NodeInstance, Wire, WorkflowGraph, builtin_registry},
};

use super::{
    BuiltinNode, ConversationPipelineInputs, ConversationPipelineTurnRequest, NodeConfiguration,
    SavedRequest, WorkflowDefinition, WorkflowRevision, origin, turn_entry,
};
use crate::workflows::{WorkflowRun, generic_agent, launch::launch_entry_in_tx};

pub(in crate::workflows) const WORKFLOW_ID: &str = "omniagent:direct-conversation";
const AGENT_ID: &str = "conversation-agent";

fn graph() -> Result<WorkflowDefinition> {
    let mut registry = builtin_registry();
    let input = json!({
        "type": "object", "required": ["historyContextId"], "additionalProperties": false,
        "properties": {"messageRevisionId": {"type":"string"}, "historyContextId": {"type":"string"}}
    });
    let reply = json!({
        "type": "object", "required": ["replyRevisionId"], "additionalProperties": false,
        "properties": {"replyRevisionId": {"type":"string"}}
    });
    for definition in &mut registry {
        match definition.kind {
            BuiltinNode::TypedStart => {
                definition
                    .output_schemas
                    .insert("inputs".into(), input.clone());
            }
            BuiltinNode::ConversationAgent => {
                definition
                    .input_schemas
                    .insert("input".into(), input.clone());
                definition
                    .output_schemas
                    .insert("result".into(), reply.clone());
            }
            BuiltinNode::End => {
                definition
                    .input_schemas
                    .insert("result".into(), reply.clone());
                definition
                    .output_schemas
                    .insert("result".into(), reply.clone());
            }
            _ => {}
        }
    }
    let endpoint = |node: &str, port: &str| Endpoint {
        node: node.into(),
        port: port.into(),
    };
    let wire = |from, output, to, input| Wire {
        from: endpoint(from, output),
        to: endpoint(to, input),
    };
    let instance = |id: &str, kind, configuration| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    };
    let start = serde_json::from_value(json!({
        "kind":"typed_start", "fields":[]
    }))?;
    crate::workflows::compile_resolved_graph(
        WORKFLOW_ID.into(),
        WorkflowGraph {
            name: "Direct conversation".into(),
            nodes: vec![
                instance("start", BuiltinNode::TypedStart, start),
                instance(
                    AGENT_ID,
                    BuiltinNode::ConversationAgent,
                    NodeConfiguration::None,
                ),
                instance("end", BuiltinNode::End, NodeConfiguration::End),
            ],
            wires: vec![
                wire("start", "start", AGENT_ID, "ready"),
                wire("start", "inputs", AGENT_ID, "input"),
                wire(AGENT_ID, "completed", "end", "ready"),
                wire(AGENT_ID, "result", "end", "result"),
            ],
        },
        |node| registry.iter().find(|entry| entry.kind == node.kind),
        true,
    )
}

pub(crate) fn pin_direct_conversation(
    db: &Connection,
    project: &str,
    conversation: &str,
) -> Result<WorkflowRevision> {
    let saved = db.query_row(
        "SELECT workflow_id, revision FROM conversation_direct_pipeline_pins WHERE conversation_id = ?1 AND project_id = ?2",
        params![conversation, project],
        |row| Ok(WorkflowRevision { id: row.get(0)?, revision: row.get(1)? }),
    ).optional()?;
    if let Some(saved) = saved {
        return Ok(saved);
    }
    let definition = graph()?;
    let pinned = &definition.version;
    db.execute(
        "INSERT OR IGNORE INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)",
        params![
            project,
            pinned.id,
            pinned.revision,
            protection::protect(&serde_json::to_vec(&definition)?)?
        ],
    )?;
    db.execute(
        "INSERT INTO conversation_direct_pipeline_pins VALUES (?1, ?2, ?3, ?4)",
        params![conversation, project, pinned.id, pinned.revision],
    )?;
    Ok(definition.version)
}

pub(crate) fn launch_direct_turn(
    db: &Connection,
    job: &Job,
    scope: &SessionScope,
    message_revision: Option<&str>,
) -> Result<()> {
    let context = job
        .context
        .as_ref()
        .context("A direct turn needs a conversation")?;
    let conversation = &context.conversation_id;
    let workflow = pin_direct_conversation(db, &job.project_id, conversation)?;
    let mut inputs = ConversationPipelineInputs::default();
    inputs
        .values
        .insert("historyContextId".into(), context.context_id.clone().into());
    if let Some(revision) = message_revision {
        inputs
            .values
            .insert("messageRevisionId".into(), revision.into());
    }
    let saved = SavedRequest {
        request: ConversationPipelineTurnRequest {
            conversation_id: conversation.clone(),
            request_id: job.id.clone(),
            text: String::new(),
            submitted_draft: None,
            inputs: None,
            selection: scope
                .model
                .clone()
                .map(|model_id| crate::model_catalog::ModelSelection {
                    provider: scope.provider,
                    profile_id: scope.profile_id.clone(),
                    model_id,
                }),
            effort: scope.effort.clone(),
            reference_ids: Vec::new(),
            action: None,
        },
        request_fingerprint: String::new(),
        inputs,
        workflow,
        checkout_id: job.checkout_id.clone(),
        project_id: job.project_id.clone(),
        native_job: Some(job.id.clone()),
    };
    let run = launch_entry_in_tx(
        db,
        &job.project_id,
        &saved.workflow,
        turn_entry(db, &saved, &[])?,
        &origin(&saved),
    )?;
    let source: String = db.query_row(
        "SELECT id FROM workflow_artifacts WHERE pipeline_run_id = ?1 AND node_instance_id = 'start' AND port_name = 'inputs'",
        [&run.id],
        |row| row.get(0),
    )?;
    crate::workflows::control::require_stage(
        db,
        &job.project_id,
        &run.id,
        AGENT_ID,
        None,
        &[("input".into(), source)].into(),
    )?;

    db.execute(
        "INSERT INTO conversation_pipeline_turns(conversation_id, request_id, payload, message_revision_id, history_context_id, workflow_run_id, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
        params![conversation, job.id, protection::protect(&serde_json::to_vec(&saved)?)?, message_revision, context.context_id, run.id],
    )?;
    Ok(())
}

pub(in crate::workflows) enum NativeOutcome {
    Waiting,
    Failed,
    Completed(MessageRevision),
}

pub(in crate::workflows) fn native_outcome(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<NativeOutcome> {
    let id: Option<String> = db
        .query_row(
            "SELECT job.id FROM conversation_pipeline_turns turn
        JOIN jobs job ON job.id = turn.request_id AND job.conversation_id = turn.conversation_id
        WHERE job.project_id = ?1 AND turn.workflow_run_id = ?2",
            params![run.project_id, run.id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(NativeOutcome::Waiting);
    };
    let job =
        load_job(db, &run.project_id, &id)?.context("The conversation agent's job disappeared")?;
    if job.queued_recovery.is_some() {
        return Ok(NativeOutcome::Waiting);
    }
    let adopted = db.query_row(
        "SELECT delivery.run_id, attempt.status FROM borrowed_instruction_deliveries delivery
         JOIN runs attempt ON attempt.id = delivery.run_id WHERE delivery.job_id = ?1 AND NOT delivery.uncertain",
        [&id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    ).optional()?;
    let attempt = if let Some((run, status)) = adopted {
        Some((run, RunStatus::from_sql(&status)?))
    } else {
        job.latest_attempt
            .map(|attempt| (attempt.run_id, attempt.status))
    };
    let Some((attempt, status)) = attempt else {
        return Ok(if job.queue_state == QueueState::Cancelled {
            NativeOutcome::Failed
        } else {
            NativeOutcome::Waiting
        });
    };
    if !status.is_terminal() || generic_agent::attempt_has_open_wait(db, &attempt)? {
        return Ok(NativeOutcome::Waiting);
    }
    if status != RunStatus::Succeeded {
        return Ok(NativeOutcome::Failed);
    }
    let is_goal = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM goal_revisions WHERE initial_job_id = ?1)",
        [&id],
        |row| row.get::<_, bool>(0),
    )?;
    let attempt = if is_goal {
        match generic_agent::goal_result_run(db, &run.project_id, &id)? {
            generic_agent::GoalResult::Ready { run_id, .. } => run_id,
            generic_agent::GoalResult::Waiting => return Ok(NativeOutcome::Waiting),
            generic_agent::GoalResult::Failed => return Ok(NativeOutcome::Failed),
        }
    } else {
        attempt
    };
    let context = db.query_row(
        "SELECT result_context_id FROM agent_attempts WHERE run_id = ?1",
        [&attempt],
        |row| row.get::<_, Option<String>>(0),
    )?;
    let Some(context) = context else {
        return Ok(NativeOutcome::Failed);
    };
    Ok(match attempt_output(db, &attempt, &context)? {
        Some(message) => NativeOutcome::Completed(message),
        None => NativeOutcome::Failed,
    })
}

pub(crate) fn is_conversation_template(definition: &WorkflowDefinition) -> bool {
    if !super::WorkflowStartSchema::for_definition(definition)
        .is_some_and(|schema| schema.conversation_enabled())
    {
        return false;
    }
    root_end(definition)
        .and_then(|node| node.definition.output_schemas.get("result"))
        .is_some_and(|schema| {
            schema
                .pointer("/properties/reply/type")
                .and_then(Value::as_str)
                == Some("string")
                && schema["required"].as_array().is_some_and(|fields| {
                    fields.iter().any(|field| field.as_str() == Some("reply"))
                })
        })
}

pub(super) fn root_end(definition: &WorkflowDefinition) -> Option<&crate::workflows::PinnedNode> {
    definition
        .nodes
        .iter()
        .find(|node| node.instance.kind == BuiltinNode::End && !node.instance.id.contains("::"))
}
