mod subject;

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, NativeHistory},
    jobs::{QueueState, load_job},
    protection,
    providers::{
        ProviderEvent, ProviderEventData, SessionMode, capabilities::ReportedCapabilities,
        configuration,
    },
    repository::{self, CheckoutLayout},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    workflows::{
        ArtifactKind, ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, PinnedNode,
        WorkflowAgentSubmission, WorkflowArtifact, WorkflowDefinition, WorkflowJobInput,
        WorkflowRun,
        artifacts::{insert_scoped_artifact, load_artifact},
        authoring::package::roles,
        automation::require_pending_agent,
        control::{
            ControlStageReceipt, ControlStageState, recorded_control, refresh, require_stage,
        },
        data_nodes::ValidateOutputConfiguration,
        inspection::record_workflow_input_ids,
        runtime::{load_definition, load_node_attempt, load_pinned_node, load_workflow_run},
    },
};

use super::{AgentStageOutcome, WorkflowAgentResponse, response_text, stage_job};
use subject::{Round, completion_inputs, fingerprint, input_key};

fn enabled(node: &PinnedNode) -> bool {
    matches!(&node.instance.configuration, NodeConfiguration::AgentRole { model: Some(_), requirements, .. }
        if node.instance.kind == BuiltinNode::Agent && requirements.review_reuse
           && requirements.plan.is_none() && requirements.goal.is_none()
           && requirements.stage.is_empty() && requirements.role_template.is_none())
}

pub(super) fn evaluate(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<AgentStageOutcome>> {
    if !enabled(node) {
        return Ok(None);
    }
    let row = db
        .query_row(
            "SELECT output_artifact_id, source_artifact_id FROM workflow_agent_review_reuses
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND unit_key = ?4
           AND input_revision = ?5",
            params![
                run.id,
                node.instance.id,
                run.plan_revision.as_deref().unwrap_or_default(),
                unit.unwrap_or_default(),
                input_key(inputs)?
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((output, source)) = row else {
        return Ok(None);
    };
    let artifact = load_artifact(
        db,
        &run.project_id,
        &run.id,
        &output,
        ArtifactKind::AgentResponse,
    )?
    .context("The reused review lost its native output")?;
    let ArtifactValue::AgentResponse(response) = artifact.value else {
        unreachable!()
    };
    ensure!(
        artifact.node_instance_id == node.instance.id
            && artifact.unit_id.as_deref() == unit
            && response.reused_from_artifact_id.as_ref() == Some(&source),
        "The reused review does not match this stage"
    );
    Ok(Some(AgentStageOutcome {
        state: ControlStageState::Completed,
        port: "completed",
        reason: Some("Reused the approved review of unchanged evidence".into()),
        source_run_id: Some(response.run_id),
    }))
}

impl Store {
    pub(in crate::workflows) async fn try_reuse_automatic_review(
        &self,
        submission: &WorkflowAgentSubmission,
        key: &str,
    ) -> Result<bool> {
        if submission.scope.mode != SessionMode::ReadOnly
            || submission.request.mode != SessionMode::ReadOnly
            || submission.scope.account_binding.is_none()
            || submission.dispatch.release_id.is_some()
            || submission.request.requires_bridge
            || !submission.request.dependencies.is_empty()
            || !submission.artifacts.contains_key("review_subject")
        {
            return Ok(false);
        }
        let (node, _) = self
            .workflow_execution_inputs(
                &submission.project_id,
                &submission.dispatch.pipeline_run_id,
                &submission.dispatch.node_instance_id,
                submission.unit_id.as_deref(),
                &submission.artifacts,
            )
            .await?;
        if !enabled(&node) {
            return Ok(false);
        }
        let Some(checkout) = submission.request.checkout_id.clone() else {
            return Ok(false);
        };
        let prepared = ReviewAdmission {
            project: submission.project_id.clone(),
            dispatch: submission.dispatch.clone(),
            unit: submission.unit_id.clone(),
            inputs: submission.artifacts.clone(),
            task: submission.request.task_id.clone(),
            checkout,
            scope: submission.scope.clone(),
            key: key.into(),
        };
        let layout = repository::verify_checkout(
            self,
            prepared.checkout.clone(),
            prepared.scope.checkout.clone(),
        )
        .await?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let reused = prepared.reuse(&tx, &layout)?;
            tx.commit()?;
            Ok(reused)
        })
        .await
    }
}

struct ReviewAdmission {
    project: String,
    dispatch: crate::jobs::PipelineDispatch,
    unit: Option<String>,
    inputs: NodeInputs,
    task: Option<String>,
    checkout: String,
    scope: SessionScope,
    key: String,
}

impl ReviewAdmission {
    fn reuse(&self, db: &Connection, layout: &CheckoutLayout) -> Result<bool> {
        let run = load_workflow_run(db, &self.project, &self.dispatch.pipeline_run_id)?
            .context("The review workflow is unavailable")?;
        let node = load_pinned_node(db, &self.project, &run.id, &self.dispatch.node_instance_id)?
            .context("The review stage is unavailable")?;
        let unit = self.unit.as_deref();
        if !enabled(&node)
            || run.plan_revision != self.dispatch.plan_revision
            || stage_job(
                db,
                &self.project,
                &run.id,
                &node.instance.id,
                unit,
                &self.inputs,
            )?
            .is_some()
        {
            return Ok(false);
        }
        if let NodeConfiguration::AgentRole {
            requirements,
            model,
            ..
        } = &node.instance.configuration
            && let Some(version) = &requirements.bound_role
        {
            let Some(template) = roles::load_role(db, &self.project, version)? else {
                return Ok(false);
            };
            if !template.requirements.is_empty()
                || roles::qualify_bound_role(&template, model.as_ref(), &self.scope).is_err()
            {
                return Ok(false);
            }
        }
        require_stage(
            db,
            &self.project,
            &run.id,
            &node.instance.id,
            unit,
            &self.inputs,
        )?;
        require_pending_agent(
            db,
            &self.dispatch,
            &self.key,
            &WorkflowJobInput {
                unit_id: self.unit.clone(),
                artifacts: self.inputs.clone(),
                plan_revision_request: None,
                comparison: None,
            },
        )?;
        let definition = load_definition(db, &self.project, &run.version)?
            .context("The pinned review workflow disappeared")?;
        let Some((source, mut response)) = self
            .prior(db, &run, &definition, &node, layout)
            .unwrap_or(None)
        else {
            return Ok(false);
        };
        response.reused_from_artifact_id = Some(source.clone());
        let output = insert_scoped_artifact(
            db,
            &self.project,
            run.id.clone(),
            node.instance.id.clone(),
            "response".into(),
            self.unit.clone(),
            ArtifactValue::AgentResponse(response),
        )?;
        let plan = run.plan_revision.as_deref().unwrap_or_default();
        record_workflow_input_ids(db, &output, plan, &self.inputs)?;
        db.execute(
            "INSERT INTO workflow_agent_review_reuses (pipeline_run_id, node_instance_id, plan_key,
                 unit_key, input_revision, output_artifact_id, source_artifact_id) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![run.id, node.instance.id, plan, unit.unwrap_or_default(), input_key(&self.inputs)?, output.id, source],
        )?;
        let deleted = db.execute(
            "DELETE FROM workflow_automatic_agents WHERE pipeline_run_id = ?1 AND node_instance_id = ?2
               AND request_key = ?3 AND plan_key = ?4 AND job_id IS NULL",
            params![run.id, node.instance.id, self.key, plan],
        )?;
        ensure!(
            deleted == 1,
            "The review's automatic dispatch changed before reuse"
        );
        refresh(db, &self.project, &run.id)?;
        Ok(true)
    }

    fn prior(
        &self,
        db: &Connection,
        run: &WorkflowRun,
        definition: &WorkflowDefinition,
        node: &PinnedNode,
        layout: &CheckoutLayout,
    ) -> Result<Option<(String, WorkflowAgentResponse)>> {
        let Some(round) = Round::for_node(definition, &node.instance.id) else {
            return Ok(None);
        };
        let Some(previous_id) = round.previous(definition, &node.instance.id) else {
            return Ok(None);
        };
        let Some(previous) = definition
            .nodes
            .iter()
            .find(|n| n.instance.id == previous_id)
        else {
            return Ok(None);
        };
        if !enabled(previous)
            || node.instance.configuration != previous.instance.configuration
            || node.definition != previous.definition
        {
            return Ok(None);
        }
        let previous_round = Round::for_node(definition, &previous_id)
            .context("The prior review round is unavailable")?;
        let Some((current_validator, config)) = validator(definition, &node.instance.id) else {
            return Ok(None);
        };
        let Some((prior_validator, prior_config)) = validator(definition, &previous_id) else {
            return Ok(None);
        };
        if config != prior_config
            || previous_round.normalize(prior_validator) != round.normalize(current_validator)
        {
            return Ok(None);
        }
        let receipts = recorded_control(db, run)?;
        let unit = self.unit.as_deref();
        let Some(review_receipt) = completed(&receipts, run, unit, &previous_id) else {
            return Ok(None);
        };
        if completed(&receipts, run, unit, prior_validator).is_none() {
            return Ok(None);
        }
        let Some(source_run) = review_receipt.source_run_id.as_deref() else {
            return Ok(None);
        };
        let current = fingerprint(db, run, definition, &round, unit, &self.inputs, &receipts)?;
        let mut matching = None;
        for artifact in outputs(
            db,
            run,
            &previous_id,
            unit,
            "response",
            ArtifactKind::AgentResponse,
        )? {
            let ArtifactValue::AgentResponse(response) = &artifact.value else {
                unreachable!()
            };
            if response.run_id != source_run || response.checkout_id != self.checkout {
                continue;
            }
            let mut records = completion_inputs(db, run, &artifact.id)?;
            if records.len() != 1 {
                continue;
            }
            let inputs = records.remove(0);
            if current
                != fingerprint(
                    db,
                    run,
                    definition,
                    &previous_round,
                    unit,
                    &inputs,
                    &receipts,
                )?
            {
                continue;
            }
            let text = response_text(db, &self.project, &run.id, response)?;
            let Ok(parsed) = config.parse(&text) else {
                continue;
            };
            if parsed.get("approved").and_then(serde_json::Value::as_bool) != Some(true) {
                continue;
            }
            if parsed
                .get("findings")
                .is_some_and(|findings| !findings.as_array().is_some_and(Vec::is_empty))
            {
                continue;
            }
            let value = ArtifactValue::JsonObject(parsed);
            let expected = BTreeMap::from([("source".to_owned(), artifact.id.clone())]);
            let mut validated = false;
            for output in outputs(
                db,
                run,
                prior_validator,
                unit,
                "object",
                ArtifactKind::JsonObject,
            )? {
                validated |= output.value == value
                    && completion_inputs(db, run, &output.id)?.contains(&expected);
            }
            if !validated
                || !self.same_dispatch(db, run, definition, &round, &artifact, response, layout)?
            {
                continue;
            }
            if matching.is_some() {
                return Ok(None);
            }
            matching = Some((artifact.id, response.clone()));
        }
        Ok(matching)
    }

    fn same_dispatch(
        &self,
        db: &Connection,
        run: &WorkflowRun,
        definition: &WorkflowDefinition,
        round: &Round<'_>,
        artifact: &WorkflowArtifact,
        response: &WorkflowAgentResponse,
        layout: &CheckoutLayout,
    ) -> Result<bool> {
        let Some(attempt) = load_node_attempt(db, &self.project, &response.run_id)? else {
            return Ok(false);
        };
        if attempt.pin.workflow_run_id != run.id
            || attempt.pin.unit_id != self.unit
            || attempt.pin.plan_revision != run.plan_revision
            || attempt.job_id != response.job_id
            || attempt.status != RunStatus::Succeeded
        {
            return Ok(false);
        }
        let Some(source_round) = Round::for_node(definition, &attempt.pin.node_instance_id) else {
            return Ok(false);
        };
        if source_round.version != round.version
            || source_round.root != round.root
            || source_round.normalize(&attempt.pin.node_instance_id)
                != round.normalize(&self.dispatch.node_instance_id)
            || source_round.ordinal >= round.ordinal
        {
            return Ok(false);
        }
        if response.reused_from_artifact_id.is_none()
            && attempt.pin.node_instance_id != artifact.node_instance_id
        {
            return Ok(false);
        }
        let Some(job) = load_job(db, &self.project, &response.job_id)? else {
            return Ok(false);
        };
        if job.mode != SessionMode::ReadOnly
            || job.queue_state == QueueState::Cancelled
            || job.queued_recovery.is_some()
            || job.task_id != self.task
            || job.checkout_id != self.checkout
            || !job
                .latest_attempt
                .is_some_and(|a| a.run_id == response.run_id && a.previous_run_id.is_none())
        {
            return Ok(false);
        }
        context_matches(db, self, response, layout)
    }
}

fn validator<'a>(
    definition: &'a WorkflowDefinition,
    review: &str,
) -> Option<(&'a str, &'a ValidateOutputConfiguration)> {
    let mut connections = definition
        .wires
        .iter()
        .filter(|w| w.from.node == review && w.from.port == "response");
    let wire = connections.next()?;
    if connections.next().is_some() || wire.to.port != "source" {
        return None;
    }
    let node = definition
        .nodes
        .iter()
        .find(|n| n.instance.id == wire.to.node)?;
    let NodeConfiguration::ValidateOutput(config) = &node.instance.configuration else {
        return None;
    };
    (node.instance.kind == BuiltinNode::Archived(crate::workflows::ArchivedNode::ValidateOutput))
        .then_some((&node.instance.id, config))
}

fn completed<'a>(
    receipts: &'a [ControlStageReceipt],
    run: &WorkflowRun,
    unit: Option<&str>,
    node: &str,
) -> Option<&'a ControlStageReceipt> {
    receipts.iter().find(|receipt| {
        receipt.node_id == node
            && receipt.unit_id.as_deref() == unit
            && receipt.plan_revision == run.plan_revision
            && receipt.state == ControlStageState::Completed
            && receipt
                .selected_ports
                .iter()
                .any(|port| port == "completed")
    })
}

fn outputs(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    port: &str,
    kind: ArtifactKind,
) -> Result<Vec<WorkflowArtifact>> {
    let mut query = db.prepare(
        "SELECT id FROM workflow_artifacts WHERE pipeline_run_id = ?1 AND node_instance_id = ?2
           AND unit_id IS ?3 AND port_name = ?4",
    )?;
    query
        .query_map(params![run.id, node, unit, port], |row| {
            row.get::<_, String>(0)
        })?
        .map(|id| {
            load_artifact(db, &run.project_id, &run.id, &id?, kind)?
                .context("The prior review lost a native output")
        })
        .collect()
}

fn context_matches(
    db: &Connection,
    target: &ReviewAdmission,
    response: &WorkflowAgentResponse,
    layout: &CheckoutLayout,
) -> Result<bool> {
    let bytes: Vec<u8> = db.query_row(
        "SELECT s.payload FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
         WHERE s.run_id = ?1 AND d.id = ?2 AND d.ordinal = 1",
        params![response.run_id, response.dispatch_id],
        |row| row.get(0),
    )?;
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&bytes)?)?;
    if snapshot.scope != target.scope
        || !matches!(snapshot.native_history, NativeHistory::Fresh)
        || snapshot.base_commit.as_ref()
            != Some(&repository::registered_checkout_head(
                db,
                &target.project,
                &target.checkout,
            )?)
        || snapshot.role_context.is_some()
        || snapshot.repository_markdown.is_some()
        || snapshot.requested_skill.is_some()
        || snapshot.answered_question.is_some()
        || !snapshot.attachments.is_empty()
        || !snapshot.conversation_references.is_empty()
    {
        return Ok(false);
    }
    let task = target
        .task
        .as_ref()
        .map(|task| crate::tasks::workflow_task_context(db, &target.project, task, None))
        .transpose()?;
    if task != snapshot.task_context {
        return Ok(false);
    }
    let Some(instructions) = snapshot.instructions.as_ref() else {
        return Ok(false);
    };
    let Some(capabilities) = snapshot.capabilities.as_ref() else {
        return Ok(false);
    };
    let mut reported = ReportedCapabilities::default();
    let mut events =
        db.prepare("SELECT payload FROM agent_events WHERE run_id = ?1 ORDER BY seq")?;
    for event in events.query_map([&response.run_id], |row| row.get::<_, Vec<u8>>(0))? {
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&event?)?)?;
        if matches!(
            &event.data,
            ProviderEventData::ContextRead { .. }
                | ProviderEventData::Tool { .. }
                | ProviderEventData::SkillUsed { .. }
                | ProviderEventData::HookObserved { .. }
                | ProviderEventData::Question { .. }
                | ProviderEventData::QuestionUnavailable { .. }
                | ProviderEventData::Approval { .. }
                | ProviderEventData::ChildActivity { .. }
                | ProviderEventData::MediaGeneration { .. }
                | ProviderEventData::DownstreamWaitRequested { .. }
        ) {
            return Ok(false);
        }
        reported.observe(&event.data);
    }
    let profile = &capabilities.scope;
    if target.scope.runner != DirectWindowsRunner::IDENTITY
        || profile.executable_revision.is_none()
        || DirectWindowsRunner.executable_revision(&target.scope.executable)
            != profile.executable_revision
        || target.scope.model != profile.reported_model
    {
        return Ok(false);
    }
    let (observed_config, skills, observed_instructions) =
        configuration::inspect(&target.scope, layout, &reported)?;
    Ok(observed_instructions == *instructions
        && skills == snapshot.filesystem_skills
        && observed_config.detected == profile.configuration.detected
        && observed_config.unresolved == profile.configuration.unresolved)
}
