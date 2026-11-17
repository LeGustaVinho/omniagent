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
    schema, timed,
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
        self.form_with_defaults(false, None)
    }

    fn form_with_defaults(
        &self,
        allow_empty: bool,
        defaults: Option<&Map<String, Value>>,
    ) -> Result<QuestionForm> {
        ensure!(
            allow_empty || !self.fields.is_empty(),
            "Add a field to the input request"
        );
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
            if let Some(default) = defaults.and_then(|values| values.get(&field.id))
                && jsonschema::is_valid(&property, default)
            {
                property["default"] = default.clone();
            }
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
    fn value_schema(&self) -> Value {
        match self {
            Self::Text
            | Self::Choice {
                multiple: false, ..
            } => json!({"type":"string"}),
            Self::Boolean => json!({"type":"boolean"}),
            Self::Number { integer } => json!({"type":if *integer { "integer" } else { "number" }}),
            Self::Choice { multiple: true, .. } => {
                json!({"type":"array", "items":{"type":"string"}})
            }
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanInteraction {
    Approval,
    Review,
    Input,
    Feedback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanExpiration {
    #[default]
    Failed,
    Expired,
    Reject,
}

impl HumanExpiration {
    fn port(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Reject => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HumanConfiguration {
    pub interaction: HumanInteraction,
    pub message: String,
    #[serde(default)]
    pub fields: Vec<RequestInputField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub on_expiration: HumanExpiration,
}

impl HumanConfiguration {
    pub(super) fn requires_input(&self) -> bool {
        self.interaction == HumanInteraction::Review
    }

    pub(super) fn is_valid(&self) -> bool {
        self.timeout_seconds.is_none_or(|seconds| seconds > 0)
            && self.deadline(timed::now_millis()).is_ok()
            && (self.on_expiration != HumanExpiration::Reject
                || self.interaction == HumanInteraction::Approval)
            && self.form(None).is_ok()
    }

    pub(super) fn response_schema(&self, input: Option<&Value>) -> Value {
        let form = json!({
            "type": "object",
            "properties": self.fields.iter().map(|field| (field.id.clone(), field.input.value_schema())).collect::<Map<_, _>>(),
            "required": self.fields.iter().filter(|field| field.required).map(|field| &field.id).collect::<Vec<_>>(),
            "additionalProperties": false,
        });
        let input = input.cloned().unwrap_or_else(|| json!({"type":"object"}));
        let mut properties = Map::from_iter([
            ("interaction".into(), json!({"type":"string"})),
            ("status".into(), json!({"type":"string"})),
            ("original".into(), input.clone()),
            (
                "values".into(),
                if self.interaction == HumanInteraction::Review {
                    input
                } else {
                    form
                },
            ),
        ]);
        let mut required = vec!["interaction", "status"];
        if self.timeout_seconds.is_none() {
            required.push("values");
        } else {
            properties.insert("expired".into(), json!({"type":"boolean"}));
        }
        if self.interaction == HumanInteraction::Approval {
            properties.insert("approved".into(), json!({"type":"boolean"}));
            if self.timeout_seconds.is_none() || self.on_expiration == HumanExpiration::Reject {
                required.push("approved");
            }
        }
        json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
    }

    fn deadline(&self, now: i64) -> Result<Option<i64>> {
        self.timeout_seconds
            .map(|seconds| timed::after(now, seconds))
            .transpose()
    }

    fn form(&self, original: Option<&Map<String, Value>>) -> Result<QuestionForm> {
        let defaults = if self.interaction == HumanInteraction::Review {
            original
        } else {
            None
        };
        RequestInputConfiguration {
            message: self.message.clone(),
            fields: self.fields.clone(),
        }
        .form_with_defaults(
            matches!(
                self.interaction,
                HumanInteraction::Approval | HumanInteraction::Review
            ),
            defaults,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowDecisionRequest {
    RequestInput {
        form: QuestionForm,
    },
    Approval {
        message: String,
    },
    Human {
        interaction: HumanInteraction,
        form: QuestionForm,
        #[serde(skip_serializing_if = "Option::is_none")]
        original: Option<Map<String, Value>>,
    },
}

impl WorkflowDecisionRequest {
    pub(super) fn for_node(
        node: &PinnedNode,
        original: Option<Map<String, Value>>,
    ) -> Result<Self> {
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
            (BuiltinNode::Human, NodeConfiguration::Human(config)) if config.is_valid() => {
                ensure!(
                    !config.requires_input() || original.is_some(),
                    "Connect the object to review"
                );
                if let (Some(schema), Some(original)) =
                    (node.definition.input_schemas.get("input"), &original)
                {
                    schema::validate(schema, &Value::Object(original.clone()))?;
                }
                let form = config.form(original.as_ref())?;
                Ok(Self::Human {
                    interaction: config.interaction,
                    form,
                    original,
                })
            }
            _ => bail!("The pinned workflow node has no operator decision form"),
        }
    }

    fn output_port(&self) -> &'static str {
        match self {
            Self::RequestInput { .. } => "answers",
            Self::Approval { .. } => "decision",
            Self::Human { .. } => "response",
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
            (
                Self::Human {
                    interaction,
                    form,
                    original,
                },
                decision,
            ) => {
                let mut evidence = Map::from_iter([
                    ("interaction".into(), serde_json::to_value(interaction)?),
                    ("status".into(), json!(decision.control_port())),
                ]);
                if let Some(original) = original {
                    evidence.insert("original".into(), json!(original));
                }
                match (interaction, decision) {
                    (
                        HumanInteraction::Approval,
                        WorkflowStageDecision::HumanApproval { approved, values },
                    ) => {
                        validate_answer(form, values.clone())?;
                        evidence.insert("approved".into(), json!(approved));
                        evidence.insert("values".into(), json!(values));
                    }
                    (HumanInteraction::Review, WorkflowStageDecision::Answer { values }) => {
                        let mut edits = values.clone();
                        for field in &form.fields {
                            if let Some(value) = &field.default {
                                edits
                                    .entry(field.id.clone())
                                    .or_insert_with(|| value.clone());
                            }
                        }
                        validate_answer(form, edits.clone())?;
                        let mut reviewed =
                            original.clone().context("Review lost its original input")?;
                        reviewed.extend(edits);
                        evidence.insert("values".into(), json!(reviewed));
                    }
                    (
                        HumanInteraction::Input | HumanInteraction::Feedback,
                        WorkflowStageDecision::Answer { values },
                    ) => {
                        validate_answer(form, values.clone())?;
                        evidence.insert("values".into(), json!(values));
                    }
                    (_, WorkflowStageDecision::Expiration { policy }) => {
                        evidence.insert("expired".into(), json!(true));
                        if *policy == HumanExpiration::Reject {
                            evidence.insert("approved".into(), json!(false));
                        }
                    }
                    _ => bail!("The response does not match the pinned workflow decision form"),
                }
                Ok(evidence)
            }
            _ => bail!("The response does not match the pinned workflow decision form"),
        }
    }
}

fn validate_answer(form: &QuestionForm, values: Map<String, Value>) -> Result<()> {
    form.validate(&QuestionReply {
        action: QuestionAction::Accept,
        values,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowStageDecision {
    Answer {
        values: Map<String, Value>,
    },
    Approval {
        approved: bool,
    },
    HumanApproval {
        approved: bool,
        #[serde(default)]
        values: Map<String, Value>,
    },
    Expiration {
        policy: HumanExpiration,
    },
}

impl WorkflowStageDecision {
    fn control_port(&self) -> &'static str {
        match self {
            Self::Answer { .. } => "answered",
            Self::Approval { approved: true } | Self::HumanApproval { approved: true, .. } => {
                "approved"
            }
            Self::Approval { approved: false }
            | Self::HumanApproval {
                approved: false, ..
            } => "rejected",
            Self::Expiration { policy } => policy.port(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<i64>,
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
    request: WorkflowDecisionRequest,
) -> Result<Option<&'static str>> {
    let request_id = stage_request_id(run, node, unit, inputs)?;
    let stored = db
        .query_row(
            "SELECT resolution, payload FROM workflow_stage_decisions WHERE request_id = ?1",
            [&request_id],
            |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let human = match &node.instance.configuration {
        NodeConfiguration::Human(config) => Some(config),
        _ => None,
    };
    if let Some((resolution, payload)) = stored {
        if let Some(resolution) = resolution {
            return Ok(Some(
                open::<WorkflowStageDecision>(&resolution)?.control_port(),
            ));
        }
        if let Some(config) = human
            && timed::read(db, run, &node.instance.id, unit)?
                .is_some_and(|deadline| deadline.due(timed::now_millis()))
        {
            let pending = open::<WorkflowPendingDecision>(&payload)?;
            let decision = WorkflowStageDecision::Expiration {
                policy: config.on_expiration,
            };
            record_decision(db, run, node, &pending, &decision)?;
            return Ok(Some(decision.control_port()));
        }
        return Ok(None);
    }
    let deadline_at = if let Some(config) = human {
        let deadline = timed::register(
            db,
            run,
            &node.instance.id,
            unit,
            "human",
            config.deadline(timed::now_millis())?,
        )?;
        if deadline.cancelled() {
            return Ok(None);
        }
        deadline.at
    } else {
        None
    };
    let pending = WorkflowPendingDecision {
        request_id,
        node_id: node.instance.id.clone(),
        node_revision: node.revision.clone(),
        plan_revision: run.plan_revision.clone(),
        unit_id: unit.map(str::to_owned),
        inputs: inputs.clone(),
        request,
        deadline_at,
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
        let result = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let WorkflowDecisionSubmission {
                    project_id,
                    run_id,
                    request_id,
                    decision,
                } = submission;
                ensure!(
                    !matches!(decision, WorkflowStageDecision::Expiration { .. }),
                    "Expiration is resolved by the workflow scheduler"
                );
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
                let node = validate_pending(&tx, &run, &definition, &request)?;
                if let Some(saved) = resolution {
                    let saved: WorkflowStageDecision = open(&saved)?;
                    ensure!(
                        !matches!(saved, WorkflowStageDecision::Expiration { .. }),
                        "This workflow request has expired"
                    );
                    ensure!(
                        saved == decision,
                        "This workflow request already has a different response"
                    );
                    return artifacts::load_artifact(
                        &tx,
                        &project_id,
                        &run_id,
                        &artifact_id.context("The saved response has no evidence")?,
                        ArtifactKind::JsonObject,
                    )?
                    .context("The saved workflow response is unavailable")
                    .map(Some);
                }

                if let NodeConfiguration::Human(config) = &node.instance.configuration
                    && timed::read(&tx, &run, &request.node_id, request.unit_id.as_deref())?
                        .is_some_and(|deadline| deadline.due(timed::now_millis()))
                {
                    let expiration = WorkflowStageDecision::Expiration {
                        policy: config.on_expiration,
                    };
                    record_decision(&tx, &run, node, &request, &expiration)?;
                    control::refresh(&tx, &project_id, &run_id)?;
                    tx.commit()?;
                    return Ok(None);
                }
                let artifact = record_decision(&tx, &run, node, &request, &decision)?;
                control::refresh(&tx, &project_id, &run_id)?;
                tx.commit()?;
                Ok(Some(artifact))
            })
            .await?;
        self.job_signal.send_replace(());
        result.context("This workflow request has expired")
    }
}

fn record_decision(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    request: &WorkflowPendingDecision,
    decision: &WorkflowStageDecision,
) -> Result<WorkflowArtifact> {
    let evidence = validated_evidence(node, &request.request, decision)?;
    let plan_key = run.plan_revision.as_deref().unwrap_or("");
    let output = request.request.output_port();
    let artifact = artifacts::insert_scoped_artifact(
        db,
        &run.project_id,
        run.id.clone(),
        request.node_id.clone(),
        output.into(),
        request.unit_id.clone(),
        ArtifactValue::JsonObject(evidence),
    )?;
    if request.unit_id.is_none() {
        execution::record_host_completion(db, &artifact, plan_key, &request.request_id)?;
    }
    inspection::record_workflow_input_ids(db, &artifact, plan_key, &request.inputs)?;
    if let WorkflowStageDecision::HumanApproval { approved: true, .. } = decision
        && let WorkflowDecisionRequest::Human {
            original: Some(original),
            ..
        } = &request.request
    {
        let approved = artifacts::insert_scoped_artifact(
            db,
            &run.project_id,
            run.id.clone(),
            request.node_id.clone(),
            "approved_result".into(),
            request.unit_id.clone(),
            ArtifactValue::JsonObject(original.clone()),
        )?;
        inspection::record_workflow_input_ids(db, &approved, plan_key, &request.inputs)?;
    }
    let updated = db.execute(
        "UPDATE workflow_stage_decisions SET resolution = ?2, artifact_id = ?3
         WHERE request_id = ?1 AND resolution IS NULL",
        params![request.request_id, seal(decision)?, artifact.id],
    )?;
    ensure!(updated == 1, "The workflow request is no longer pending");
    if node.instance.kind == BuiltinNode::Human {
        timed::finish(db, run, &request.node_id, request.unit_id.as_deref())?;
    }
    Ok(artifact)
}

fn validated_evidence(
    node: &PinnedNode,
    request: &WorkflowDecisionRequest,
    decision: &WorkflowStageDecision,
) -> Result<Map<String, Value>> {
    let evidence = request.evidence(decision)?;
    if matches!(
        request,
        WorkflowDecisionRequest::Human {
            interaction: HumanInteraction::Review,
            ..
        }
    ) && let (Some(contract), Some(reviewed)) = (
        node.definition.input_schemas.get("input"),
        evidence.get("values"),
    ) {
        schema::validate(contract, reviewed)
            .context("The reviewed data does not match the input schema")?;
    }
    if let Some(contract) = node.definition.output_schemas.get(request.output_port()) {
        schema::validate(contract, &Value::Object(evidence.clone()))?;
    }
    Ok(evidence)
}

fn validate_pending<'a>(
    db: &Connection,
    run: &WorkflowRun,
    definition: &'a WorkflowDefinition,
    request: &WorkflowPendingDecision,
) -> Result<&'a PinnedNode> {
    let node = definition
        .nodes
        .iter()
        .find(|node| node.instance.id == request.node_id)
        .context("The decision node is unavailable in this workflow revision")?;
    let unit = request.unit_id.as_deref();
    ensure!(
        node.instance.kind != BuiltinNode::Human
            || !timed::read(db, run, &request.node_id, unit)?
                .is_some_and(|deadline| deadline.cancelled()),
        "This workflow request was cancelled"
    );
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
    Ok(node)
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
    let request = WorkflowDecisionRequest::for_node(node, None)?;
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
        WorkflowDecisionRequest::Human { .. } => bail!("Use a sample Human response"),
    };
    request.evidence(&reply)?;
    Ok(reply.control_port())
}

pub(crate) fn simulate_human(
    node: &PinnedNode,
    original: Option<Map<String, Value>>,
    sample: &Value,
) -> Result<(&'static str, Map<String, Value>)> {
    let NodeConfiguration::Human(config) = &node.instance.configuration else {
        bail!("Missing Human wait configuration");
    };
    let request = WorkflowDecisionRequest::for_node(node, original)?;
    let sample = sample
        .as_object()
        .context("The sample response needs an object")?;
    let reply = if sample.get("expired").and_then(Value::as_bool) == Some(true) {
        ensure!(
            config.timeout_seconds.is_some(),
            "Set a deadline to simulate expiration"
        );
        WorkflowStageDecision::Expiration {
            policy: config.on_expiration,
        }
    } else {
        let values = sample
            .get("values")
            .map(|value| {
                value
                    .as_object()
                    .cloned()
                    .context("Sample response field values need an object")
            })
            .transpose()?
            .unwrap_or_default();
        if config.interaction == HumanInteraction::Approval {
            let approved = sample
                .get("approved")
                .and_then(Value::as_bool)
                .context("The sample decision needs an approved Boolean")?;
            WorkflowStageDecision::HumanApproval { approved, values }
        } else {
            WorkflowStageDecision::Answer { values }
        }
    };
    Ok((
        reply.control_port(),
        validated_evidence(node, &request, &reply)?,
    ))
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
