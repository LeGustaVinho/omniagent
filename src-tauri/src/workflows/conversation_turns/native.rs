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
    SavedRequest, WorkflowDefinition, WorkflowRevision, open_request, origin, turn_entry,
};
use crate::workflows::{
    ArtifactValue, WorkflowRun,
    artifacts::insert_artifact,
    digest, generic_agent,
    inspection::record_workflow_input_ids,
    launch::{launch_entry_in_tx, load_workflow_launch},
};

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

pub(crate) fn revise_direct_turn_input(
    db: &Connection,
    project: &str,
    conversation: &str,
    job: &str,
    revision: &str,
    history_context: Option<&str>,
) -> Result<()> {
    let pending = db
        .query_row(
            "SELECT t.sequence, t.payload, t.workflow_run_id FROM conversation_pipeline_turns t
         JOIN workflow_run_pins p ON p.pipeline_run_id = t.workflow_run_id
         WHERE t.conversation_id = ?1 AND t.request_id = ?2 AND t.state = 'running'
           AND p.project_id = ?3 AND p.workflow_id = ?4
           AND (t.message_revision_id IS NOT ?5 OR (?6 IS NOT NULL AND t.history_context_id IS NOT ?6))",
            params![conversation, job, project, WORKFLOW_ID, revision, history_context],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((sequence, payload, run)) = pending else {
        return Ok(());
    };
    let mut saved = open_request(&payload)?;
    let mut launch = load_workflow_launch(db, project, &run)?
        .context("The direct turn lost its pinned launch")?;
    saved
        .inputs
        .values
        .insert("messageRevisionId".into(), revision.into());
    if let Some(history_context) = history_context {
        saved
            .inputs
            .values
            .insert("historyContextId".into(), history_context.into());
    }
    launch.inputs = saved.inputs.values.clone();
    let launch_payload = serde_json::to_vec(&launch)?;
    let original: String = db.query_row(
        "SELECT artifact_id FROM workflow_host_completions
         WHERE pipeline_run_id = ?1 AND node_instance_id = 'start' AND execution_key = ''",
        [&run],
        |row| row.get(0),
    )?;
    let artifact = insert_artifact(
        db,
        project,
        run.clone(),
        "start".into(),
        "inputs".into(),
        ArtifactValue::JsonObject(launch.inputs),
    )?;
    if artifact.id != original {
        record_workflow_input_ids(db, &artifact, "", &Default::default())?;
        db.execute(
            "UPDATE workflow_host_completions SET artifact_id = ?2, request_revision = ?3
             WHERE pipeline_run_id = ?1 AND node_instance_id = 'start' AND execution_key = ''",
            params![run, artifact.id, digest(&launch_payload)],
        )?;
        // Replace the draft binding before recording delivery of this instruction.
        db.execute("DELETE FROM workflow_artifacts WHERE id = ?1", [original])?;
    }
    db.execute(
        "UPDATE workflow_launches SET payload = ?2 WHERE pipeline_run_id = ?1",
        params![run, protection::protect(&launch_payload)?],
    )?;
    db.execute(
        "UPDATE conversation_pipeline_turns SET message_revision_id = ?2, payload = ?3,
         history_context_id = coalesce(?4, history_context_id) WHERE sequence = ?1",
        params![
            sequence,
            revision,
            protection::protect(&serde_json::to_vec(&saved)?)?,
            history_context
        ],
    )?;
    Ok(())
}

pub(crate) fn reopen_direct_turn(db: &Connection, job: &Job) -> Result<()> {
    let Some(context) = &job.context else {
        return Ok(());
    };
    let reopened = db.query_row(
        "UPDATE conversation_pipeline_turns AS t SET state = 'running', error = NULL, result_revision_id = NULL
         WHERE t.conversation_id = ?1 AND t.request_id = ?2 AND t.state = 'failed'
           AND EXISTS (SELECT 1 FROM workflow_run_pins p WHERE p.pipeline_run_id = t.workflow_run_id
                       AND p.project_id = ?3 AND p.workflow_id = ?4)
         RETURNING workflow_run_id",
        params![context.conversation_id, job.id, job.project_id, WORKFLOW_ID],
        |row| row.get::<_, String>(0),
    ).optional()?;
    if let Some(run) = reopened {
        db.execute(
            "DELETE FROM workflow_control_receipts WHERE pipeline_run_id = ?1
             AND node_instance_id IN (?2, 'end')",
            params![run, AGENT_ID],
        )?;
    }
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
    validate_conversation_template(definition).is_ok()
}

pub(in crate::workflows) fn validate_conversation_template(
    definition: &WorkflowDefinition,
) -> Result<()> {
    anyhow::ensure!(
        super::WorkflowStartSchema::for_definition(definition)
            .is_some_and(|schema| schema.conversation_enabled()),
        "A conversation pipeline needs a text message input on Start"
    );
    anyhow::ensure!(
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
            }),
        "A conversation pipeline needs an End result Schema with required reply text"
    );
    Ok(())
}

pub(super) fn root_end(definition: &WorkflowDefinition) -> Option<&crate::workflows::PinnedNode> {
    definition
        .nodes
        .iter()
        .find(|node| node.instance.kind == BuiltinNode::End && !node.instance.id.contains("::"))
}
