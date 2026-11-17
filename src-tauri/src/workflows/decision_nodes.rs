use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use crate::{
    protection,
    providers::questions::{QuestionAction, QuestionForm, QuestionReply},
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, PinnedNode,
    WorkflowArtifact, WorkflowDefinition, WorkflowRun, artifacts, control, digest, execution,
    inspection,
    runtime::{load_definition, load_workflow_run},
};

#[cfg(test)]
mod tests;

const MAX_DECISION_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestInputConfiguration {
    pub message: String,
    pub fields: Vec<RequestInputField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestInputField {
    pub id: String,
    pub label: String,
    pub required: bool,
    pub input: RequestInputKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestInputKind {
    Text,
    Boolean,
    Number {
        integer: bool,
    },
    Choice {
        multiple: bool,
        options: Vec<RequestInputOption>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestInputOption {
    pub id: String,
    pub label: String,
}

impl RequestInputConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        self.form().is_ok()
    }

    fn form(&self) -> Result<QuestionForm> {
        ensure!(!self.fields.is_empty(), "Add a field to the input request");
        check_size(self)?;
        let mut properties = Map::new();
        let mut order = BTreeMap::new();
        let mut required = Vec::new();
        for (index, field) in self.fields.iter().enumerate() {
            ensure!(
                !field.id.trim().is_empty()
                    && !field.label.trim().is_empty()
                    && order.insert(field.id.as_str(), index).is_none(),
                "Input fields need distinct IDs and a label"
            );
            let mut property = field.input.property(field.required);
            property["title"] = json!(field.label);
            properties.insert(field.id.clone(), property);
            if field.required {
                required.push(&field.id);
            }
        }
        let mut form = QuestionForm::schema(
            self.message.clone(),
            &json!({"type":"object", "properties":properties, "required":required}),
        )?;
        form.fields.sort_by_key(|field| order[&field.id.as_str()]);
        form.actions = vec![QuestionAction::Accept];
        Ok(form)
    }
}

impl RequestInputKind {
    fn property(&self, required: bool) -> Value {
        match self {
            Self::Text => json!({"type":"string", "minLength": u8::from(required)}),
            Self::Boolean => json!({"type":"boolean"}),
            Self::Number { integer } => json!({"type":if *integer { "integer" } else { "number" }}),
            Self::Choice { multiple, options } => {
                let options = options
                    .iter()
                    .map(|option| json!({"const":option.id,"title":option.label}))
                    .collect::<Vec<_>>();
                if *multiple {
                    json!({"type":"array","items":{"type":"string","anyOf":options},"minItems":u8::from(required)})
                } else {
                    json!({"type":"string","oneOf":options})
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalConfiguration {
    pub message: String,
}

impl ApprovalConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        !self.message.trim().is_empty() && check_size(self).is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowDecisionRequest {
    RequestInput { form: QuestionForm },
    Approval { message: String },
}

impl WorkflowDecisionRequest {
    fn for_node(node: &PinnedNode) -> Result<Self> {
        match (&node.instance.kind, &node.instance.configuration) {
            (BuiltinNode::RequestInput, NodeConfiguration::RequestInput(config)) => {
                Ok(Self::RequestInput {
                    form: config.form()?,
                })
            }
            (BuiltinNode::Approval, NodeConfiguration::Approval(config)) if config.is_valid() => {
                Ok(Self::Approval {
                    message: config.message.clone(),
                })
            }
            _ => bail!("The pinned workflow node has no operator decision form"),
        }
    }

    fn output_port(&self) -> &'static str {
        match self {
            Self::RequestInput { .. } => "answers",
            Self::Approval { .. } => "decision",
        }
    }

    fn evidence(&self, decision: &WorkflowStageDecision) -> Result<Map<String, Value>> {
        check_size(decision)?;
        match (self, decision) {
            (Self::RequestInput { form }, WorkflowStageDecision::Answer { values }) => {
                let reply = QuestionReply {
                    action: QuestionAction::Accept,
                    values: values.clone(),
                };
                form.validate(&reply)?;
                Ok(reply.values)
            }
            (Self::Approval { .. }, WorkflowStageDecision::Approval { approved }) => Ok(
                Map::from_iter([("approved".into(), Value::Bool(*approved))]),
            ),
            _ => bail!("The response does not match the pinned workflow decision form"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowStageDecision {
    Answer { values: Map<String, Value> },
    Approval { approved: bool },
}

impl WorkflowStageDecision {
    fn control_port(&self) -> &'static str {
        match self {
            Self::Answer { .. } => "answered",
            Self::Approval { approved: true } => "approved",
            Self::Approval { approved: false } => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPendingDecision {
    pub request_id: String,
    pub node_id: String,
    pub node_revision: String,
    pub plan_revision: Option<String>,
    pub unit_id: Option<String>,
    pub inputs: NodeInputs,
    pub request: WorkflowDecisionRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDecisionSubmission {
    pub project_id: String,
    pub run_id: String,
    pub request_id: String,
    pub decision: WorkflowStageDecision,
}

pub(super) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<&'static str>> {
    let request_id = stage_request_id(run, node, unit, inputs)?;
    let resolution = db
        .query_row(
            "SELECT resolution FROM workflow_stage_decisions WHERE request_id = ?1",
            [&request_id],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()?;
    if let Some(resolution) = resolution {
        return resolution
            .as_deref()
            .map(|payload| open::<WorkflowStageDecision>(payload).map(|reply| reply.control_port()))
            .transpose();
    }
    let pending = WorkflowPendingDecision {
        request_id,
        node_id: node.instance.id.clone(),
        node_revision: node.revision.clone(),
        plan_revision: run.plan_revision.clone(),
        unit_id: unit.map(str::to_owned),
        inputs: inputs.clone(),
        request: WorkflowDecisionRequest::for_node(node)?,
    };
    db.execute(
        "INSERT INTO workflow_stage_decisions (request_id,pipeline_run_id,node_instance_id,plan_key,unit_key,payload)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            pending.request_id,
            run.id,
            pending.node_id,
            run.plan_revision.as_deref().unwrap_or(""),
            unit.unwrap_or(""),
            seal(&pending)?,
        ],
    )?;
    Ok(None)
}

pub(super) fn pending_decisions(
    db: &Connection,
    run: &WorkflowRun,
    definition: &WorkflowDefinition,
) -> Result<Vec<WorkflowPendingDecision>> {
    let mut query = db.prepare(
        "SELECT payload FROM workflow_stage_decisions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND resolution IS NULL
         ORDER BY node_instance_id, unit_key",
    )?;
    let rows = query.query_map(
        params![run.id, run.plan_revision.as_deref().unwrap_or("")],
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut pending = Vec::new();
    for row in rows {
        let request: WorkflowPendingDecision = open(&row?)?;
        if validate_pending(db, run, definition, &request).is_ok() {
            pending.push(request);
        }
    }
    Ok(pending)
}

impl Store {
    pub async fn resolve_workflow_stage_decision(
        &self,
        submission: WorkflowDecisionSubmission,
    ) -> Result<WorkflowArtifact> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let WorkflowDecisionSubmission {
                project_id,
                run_id,
                request_id,
                decision,
            } = submission;
            let run = load_workflow_run(&tx, &project_id, &run_id)?
                .context("The workflow run is outside this project")?;
            let definition = load_definition(&tx, &project_id, &run.version)?
                .context("The pinned workflow definition is unavailable")?;
            let (payload, resolution, artifact_id) = tx
                .query_row(
                    "SELECT payload, resolution, artifact_id FROM workflow_stage_decisions
                 WHERE request_id = ?1 AND pipeline_run_id = ?2",
                    params![request_id, run_id],
                    |row| {
                        Ok((
                            row.get::<_, Vec<u8>>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?
                .context("The operator request is outside this workflow run")?;
            let request: WorkflowPendingDecision = open(&payload)?;
            validate_pending(&tx, &run, &definition, &request)?;
            if let Some(saved) = resolution {
                ensure!(
                    open::<WorkflowStageDecision>(&saved)? == decision,
                    "This workflow request already has a different response"
                );
                return artifacts::load_artifact(
                    &tx,
                    &project_id,
                    &run_id,
                    &artifact_id.context("The saved response has no evidence")?,
                    ArtifactKind::JsonObject,
                )?
                .context("The saved workflow response is unavailable");
            }

            let evidence = request.request.evidence(&decision)?;
            let artifact = artifacts::insert_scoped_artifact(
                &tx,
                &project_id,
                run_id.clone(),
                request.node_id,
                request.request.output_port().into(),
                request.unit_id,
                ArtifactValue::JsonObject(evidence),
            )?;
            let plan_key = run.plan_revision.as_deref().unwrap_or("");
            if artifact.unit_id.is_none() {
                execution::record_host_completion(&tx, &artifact, plan_key, &request_id)?;
            }
            inspection::record_workflow_input_ids(&tx, &artifact, plan_key, &request.inputs)?;
            let updated = tx.execute(
                "UPDATE workflow_stage_decisions SET resolution = ?2, artifact_id = ?3
                 WHERE request_id = ?1 AND resolution IS NULL",
                params![request_id, seal(&decision)?, artifact.id],
            )?;
            ensure!(updated == 1, "The workflow request is no longer pending");
            control::refresh(&tx, &project_id, &run_id)?;
            tx.commit()?;
            Ok(artifact)
        })
        .await
    }
}

fn validate_pending(
    db: &Connection,
    run: &WorkflowRun,
    definition: &WorkflowDefinition,
    request: &WorkflowPendingDecision,
) -> Result<()> {
    let node = definition
        .nodes
        .iter()
        .find(|node| node.instance.id == request.node_id)
        .context("The decision node is unavailable in this workflow revision")?;
    let unit = request.unit_id.as_deref();
    ensure!(
        request.node_revision == node.revision
            && request.plan_revision == run.plan_revision
            && request.request_id == stage_request_id(run, node, unit, &request.inputs)?,
        "The workflow request belongs to an earlier execution scope"
    );
    ensure!(
        control::unit_nodes(definition).contains(node.instance.id.as_str()) == unit.is_some(),
        "The workflow request is outside the pinned logical unit"
    );
    if let Some(unit) = unit {
        let active: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_delivery_units
             WHERE project_id = ?1 AND pipeline_run_id = ?2 AND plan_revision = ?3 AND unit_id = ?4)",
            params![run.project_id, run.id, run.plan_revision, unit],
            |row| row.get(0),
        )?;
        ensure!(
            active,
            "The logical unit no longer belongs to the active plan"
        );
    }
    ensure!(
        control::require_dependencies(
            db,
            definition,
            run,
            &request.node_id,
            unit,
            &request.inputs
        )?,
        "The workflow request has no pinned dependencies"
    );
    Ok(())
}

fn stage_request_id(
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(
        &run.id,
        &node.instance.id,
        &node.revision,
        &run.plan_revision,
        unit,
        inputs,
    ))?))
}

pub(crate) fn simulate_stage_decision(
    node: &PinnedNode,
    outputs: &BTreeMap<String, Value>,
) -> Result<&'static str> {
    let request = WorkflowDecisionRequest::for_node(node)?;
    ensure!(
        outputs.len() == 1,
        "Provide one sample workflow decision output"
    );
    let value = outputs
        .get(request.output_port())
        .and_then(Value::as_object)
        .context("The sample workflow decision needs an object output")?;
    let reply = match request {
        WorkflowDecisionRequest::RequestInput { .. } => WorkflowStageDecision::Answer {
            values: value.clone(),
        },
        WorkflowDecisionRequest::Approval { .. } => {
            ensure!(
                value.len() == 1,
                "Use an approved Boolean for the sample decision"
            );
            let approved = value
                .get("approved")
                .and_then(Value::as_bool)
                .context("The sample decision needs an approved Boolean")?;
            WorkflowStageDecision::Approval { approved }
        }
    };
    request.evidence(&reply)?;
    Ok(reply.control_port())
}

fn check_size(value: &impl Serialize) -> Result<()> {
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_DECISION_BYTES,
        "Workflow decision data exceeds the size limit"
    );
    Ok(())
}

fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn open<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}
