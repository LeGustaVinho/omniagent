use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    jobs::{FailureCause, FailureObservation},
    protection,
    store::Store,
};

use super::{
    ArtifactValue, BuiltinNode, Endpoint, NodeConfiguration, PinnedNode, PortType, Wire,
    WorkflowArtifact, WorkflowDefinition, WorkflowRun,
    runtime::{load_definition, load_workflow_run},
};

pub(super) mod repair;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConditionPredicate {
    Exists { pointer: String },
    Equals { pointer: String, value: Value },
    CandidateChecksPassed,
}

impl ConditionPredicate {
    pub(super) fn valid(&self) -> bool {
        let pointer = self.pointer();
        pointer.len() <= 1024
            && (pointer.is_empty() || pointer.starts_with('/'))
            && pointer
                .match_indices('~')
                .all(|(index, _)| matches!(pointer.as_bytes().get(index + 1), Some(b'0' | b'1')))
            && match self {
                Self::Exists { .. } => true,
                Self::Equals { value, .. } => {
                    !value.is_object()
                        && !value.is_array()
                        && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 4096)
                }
                Self::CandidateChecksPassed => true,
            }
    }

    fn pointer(&self) -> &str {
        match self {
            Self::Exists { pointer } | Self::Equals { pointer, .. } => pointer,
            Self::CandidateChecksPassed => "",
        }
    }

    pub(crate) fn test(&self, value: &Value) -> bool {
        let found = value.pointer(self.pointer());
        match self {
            Self::Exists { .. } => found.is_some(),
            Self::Equals { value, .. } => found == Some(value),
            Self::CandidateChecksPassed => {
                value.get("kind").and_then(Value::as_str) == Some("checks")
                    && value
                        .pointer("/evidence/allowsAcceptance")
                        .and_then(Value::as_bool)
                        == Some(true)
            }
        }
    }

    fn test_recorded_artifact(&self, db: &Connection, artifact: &ArtifactValue) -> Result<bool> {
        if matches!(self, Self::CandidateChecksPassed) {
            let ArtifactValue::Checks(checks) = artifact else {
                anyhow::bail!("This condition needs a verified candidate checks receipt");
            };
            let verified = crate::repository::verification::load_scoped_candidate_verification(
                db,
                &checks.task.project_id,
                &checks.task.task_id,
                &checks.id,
            )?
            .context("The exact candidate verification receipt is unavailable")?;
            Ok(verified.passes_repair_checks())
        } else {
            Ok(self.test(&serde_json::to_value(artifact)?))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStageState {
    Completed,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlStageReceipt {
    pub node_id: String,
    pub unit_id: Option<String>,
    pub plan_revision: Option<String>,
    pub state: ControlStageState,
    pub selected_ports: Vec<String>,
    pub reason: Option<String>,
    pub source_run_id: Option<String>,
    pub dependencies: Vec<ControlDependency>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub forwarded_outputs: std::collections::BTreeMap<String, ForwardedOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardedOutput {
    input: String,
    pub artifact_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDependency {
    from: Endpoint,
    to_port: String,
    selected: bool,
    artifact_id: Option<String>,
}

fn is_control(kind: BuiltinNode) -> bool {
    matches!(
        kind,
        BuiltinNode::Condition
            | BuiltinNode::Parallel
            | BuiltinNode::Join
            | BuiltinNode::IterationLimit
            | BuiltinNode::BuildList
            | BuiltinNode::SetFields
            | BuiltinNode::ValidateOutput
            | BuiltinNode::CollectEvidence
            | BuiltinNode::QualityGate
            | BuiltinNode::FilterMapList
            | BuiltinNode::BuildText
            | BuiltinNode::RequestInput
            | BuiltinNode::Approval
            | BuiltinNode::FindWork
            | BuiltinNode::UpdateProgress
            | BuiltinNode::CompareModels
            | BuiltinNode::Agent
            | BuiltinNode::LoadWorkContext
            | BuiltinNode::ManageWork
            | BuiltinNode::MaterializeBacklog
            | BuiltinNode::RunBacklog
            | BuiltinNode::VerifyTestPhase
            | BuiltinNode::VerifyIntegratedChecks
            | BuiltinNode::SearchPublicRepositories
            | BuiltinNode::OpenOrUpdatePullRequest
            | BuiltinNode::WaitForCi
            | BuiltinNode::MergePullRequest
            | BuiltinNode::SummarizeResult
    ) || kind.generated_iteration()
}

pub(super) fn uses_control(definition: &WorkflowDefinition) -> bool {
    definition.nodes.iter().any(|node| {
        is_control(node.instance.kind)
            || matches!(
                node.instance.kind,
                BuiltinNode::McpTool | BuiltinNode::TypedStart
            )
    }) || definition
        .wires
        .iter()
        .any(|wire| wire.from.port == "failed")
}

pub(super) fn validate_release_wires(nodes: &[PinnedNode], wires: &[Wire]) -> Result<()> {
    let source = |node: &str, port: &str| {
        wires
            .iter()
            .find(|wire| wire.to.node == node && wire.to.port == port)
            .map(|wire| &wire.from)
    };
    let kind = |id: &str| {
        nodes
            .iter()
            .find(|node| node.instance.id == id)
            .map(|node| node.instance.kind)
    };
    for step in nodes.iter().filter(|node| {
        matches!(
            node.instance.kind,
            BuiltinNode::Implement | BuiltinNode::Command | BuiltinNode::VerifyTestPhase
        )
    }) {
        let node = step.instance.id.as_str();
        let provider = source(node, "release").context("writing node requires a pinned release")?;
        let worktree =
            source(node, "worktree").context("writing node requires a pinned worktree")?;
        ensure!(
            kind(&provider.node) == Some(BuiltinNode::PrepareWorktree)
                && provider.port == "release"
                && worktree.node == provider.node
                && worktree.port == "worktree",
            "writing node {node} must use release and worktree from the same preparation"
        );
        let approver = source(&provider.node, "decision")
            .context("write preparation must consume a pinned implementation approval")?;
        ensure!(
            kind(&approver.node) == Some(BuiltinNode::ApproveImplementation)
                && approver.port == "decision",
            "write preparation must consume a pinned implementation approval"
        );
        ensure!(
            source(&provider.node, "task") == source(&approver.node, "task"),
            "write preparation and implementation approval must use the same work item"
        );
        if step.instance.kind == BuiltinNode::Implement {
            ensure!(
                source(node, "plan") == source(&approver.node, "plan"),
                "writing node {node} must use the plan selected by its implementation approval"
            );
        }
    }
    Ok(())
}

pub(super) fn unit_nodes(definition: &WorkflowDefinition) -> HashSet<&str> {
    let mut scoped: HashSet<_> = definition
        .nodes
        .iter()
        .filter(|node| node.instance.kind.per_unit())
        .map(|node| node.instance.id.as_str())
        .collect();
    let control_ids: HashSet<_> = definition
        .nodes
        .iter()
        .filter(|node| is_control(node.instance.kind))
        .map(|node| node.instance.id.as_str())
        .collect();
    loop {
        let len = scoped.len();
        for wire in &definition.wires {
            if scoped.contains(wire.from.node.as_str())
                && control_ids.contains(wire.to.node.as_str())
            {
                scoped.insert(wire.to.node.as_str());
            }
        }
        if scoped.len() == len {
            return scoped;
        }
    }
}

impl Store {
    pub(super) async fn refresh_control(&self, project: String, pipeline: String) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            refresh(&tx, &project, &pipeline)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

pub(crate) fn refresh_from_attempt(db: &Connection, attempt: &str) -> Result<()> {
    let origin: Option<(String, String)> = db.query_row(
        "SELECT p.project_id, p.pipeline_run_id FROM job_attempts a JOIN job_pipeline_origins p ON p.job_id = a.job_id
         JOIN workflow_run_pins w ON w.pipeline_run_id = p.pipeline_run_id AND w.project_id = p.project_id
         WHERE a.run_id = ?1", [attempt], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if let Some((project, run)) = origin {
        refresh(db, &project, &run)?;
    }
    Ok(())
}

pub(super) fn refresh(db: &Connection, project: &str, pipeline: &str) -> Result<()> {
    let Some(run) = load_workflow_run(db, project, pipeline)? else {
        return Ok(());
    };
    let definition = load_definition(db, project, &run.version)?
        .context("missing pinned workflow definition")?;
    let controls = uses_control(&definition);
    if !controls
        && !definition
            .nodes
            .iter()
            .any(|node| node.instance.kind == BuiltinNode::ApproveImplementation)
    {
        return Ok(());
    }
    let mut state = Evaluation::new(db, &definition, &run)?;
    if !controls {
        return state.settle(None, false);
    }
    let unit_ids = db.prepare(
        "SELECT unit_id FROM workflow_delivery_units WHERE project_id = ?1 AND pipeline_run_id = ?2 AND plan_revision = ?3 ORDER BY ordinal",
    )?.query_map(params![project, pipeline, run.plan_revision], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for unit in std::iter::once(None).chain(unit_ids.iter().map(|id| Some(id.as_str()))) {
        state.settle(unit, true)?;
    }
    Ok(())
}

pub(super) fn recorded_control(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<ControlStageReceipt>> {
    db.prepare(
        "SELECT c.payload FROM workflow_control_receipts c JOIN workflow_run_pins w ON w.pipeline_run_id = c.pipeline_run_id
         WHERE w.project_id = ?1 AND w.pipeline_run_id = ?2 ORDER BY c.plan_key, c.unit_key, c.node_instance_id",
    )?.query_map(params![run.project_id, run.id], |row| row.get::<_, Vec<u8>>(0))?.map(|row| {
        Ok(serde_json::from_slice(&protection::unprotect(&row?)?)?)
    }).collect()
}

pub(super) fn require_dependencies(
    db: &Connection,
    definition: &WorkflowDefinition,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    inputs: &super::NodeInputs,
) -> Result<bool> {
    if !uses_control(definition) {
        return Ok(false);
    }
    let state = Evaluation::new(db, definition, run)?;
    let target = state
        .nodes
        .get(node)
        .context("Pinned workflow stage is unavailable")?;
    if let Some(receipt) = state.receipt(node, unit) {
        ensure!(
            receipt.state != ControlStageState::Skipped,
            "workflow stage was skipped by its pinned dependencies"
        );
        ensure!(
            receipt.source_run_id.is_none(),
            "workflow stage already selected an outcome"
        );
    }
    for wire in definition.wires.iter().filter(|wire| wire.to.node == node) {
        match state.dependency(wire, unit)? {
            Arrival::Selected(artifact) => {
                if let Some(id) = inputs.get(&wire.to.port) {
                    ensure!(
                        artifact.is_some_and(|artifact| artifact.id == *id),
                        "workflow input {} differs from the selected source",
                        wire.to.port
                    );
                }
            }
            Arrival::Ignored => anyhow::bail!(
                "workflow input {} is outside the selected branch",
                wire.to.port
            ),
            Arrival::Pending => {
                let input = if target
                    .definition
                    .inputs
                    .iter()
                    .any(|port| port.name == wire.to.port && port.value == PortType::Control)
                {
                    "control input"
                } else {
                    "workflow input"
                };
                anyhow::bail!("{input} {} is waiting for its source", wire.to.port)
            }
        }
    }
    Ok(true)
}

pub(super) fn require_stage(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
    unit: Option<&str>,
    inputs: &super::NodeInputs,
) -> Result<()> {
    let run =
        load_workflow_run(db, project, run)?.context("workflow run is outside this project")?;
    super::authoring::ensure_package_runnable(db, project, &run.version)?;
    let definition = load_definition(db, project, &run.version)?
        .context("missing pinned workflow definition")?;
    require_dependencies(db, &definition, &run, node, unit, inputs)?;
    Ok(())
}

pub(crate) fn validate_attempt_control(db: &Connection, attempt: &str) -> Result<()> {
    let data = db
        .query_row(
            "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id, i.payload
         FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
         JOIN workflow_job_inputs i ON i.job_id = a.job_id WHERE a.run_id = ?1",
            [attempt],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((project, run, node, payload)) = data {
        let input: super::WorkflowJobInput =
            serde_json::from_slice(&protection::unprotect(&payload)?)?;
        require_stage(
            db,
            &project,
            &run,
            &node,
            input.unit_id.as_deref(),
            &input.artifacts,
        )?;
        super::authoring::package::roles::validate_attempt_role(
            db, &project, &run, &node, attempt,
        )?;
    }
    Ok(())
}

enum Arrival {
    Selected(Option<WorkflowArtifact>),
    Ignored,
    Pending,
}

pub(super) enum ApprovalSelection {
    Ready {
        plan: WorkflowArtifact,
        task: WorkflowArtifact,
    },
    Waiting {
        reason: String,
    },
    Skipped {
        reason: String,
    },
}

pub(super) fn approval_selection(
    db: &Connection,
    run: &WorkflowRun,
    node_id: &str,
) -> Result<ApprovalSelection> {
    let definition = load_definition(db, &run.project_id, &run.version)?
        .context("the pinned approval workflow is unavailable")?;
    let state = Evaluation::new(db, &definition, run)?;
    let node = state
        .nodes
        .get(node_id)
        .filter(|node| node.instance.kind == BuiltinNode::ApproveImplementation)
        .context("the selected stage does not approve implementation")?;
    state.approval_inputs(node)
}

struct Evaluation<'a> {
    db: &'a Connection,
    definition: &'a WorkflowDefinition,
    run: &'a WorkflowRun,
    unit_nodes: HashSet<&'a str>,
    nodes: HashMap<&'a str, &'a PinnedNode>,
    receipts: HashMap<(String, Option<String>), ControlStageReceipt>,
}

impl<'a> Evaluation<'a> {
    fn new(
        db: &'a Connection,
        definition: &'a WorkflowDefinition,
        run: &'a WorkflowRun,
    ) -> Result<Self> {
        let receipts = recorded_control(db, run)?
            .into_iter()
            .filter(|receipt| receipt.plan_revision == run.plan_revision)
            .map(|receipt| ((receipt.node_id.clone(), receipt.unit_id.clone()), receipt))
            .collect();
        Ok(Self {
            db,
            definition,
            run,
            unit_nodes: unit_nodes(definition),
            receipts,
            nodes: definition
                .nodes
                .iter()
                .map(|node| (node.instance.id.as_str(), node))
                .collect(),
        })
    }

    fn receipt(&self, node: &str, unit: Option<&str>) -> Option<&ControlStageReceipt> {
        self.receipts.get(&(node.into(), unit.map(str::to_owned)))
    }

    fn automatic_unit_action(&self, node: &PinnedNode) -> Result<bool> {
        Ok(matches!(
            node.instance.kind,
            BuiltinNode::VerifyCandidate | BuiltinNode::AcceptCandidate | BuiltinNode::VerifyCommit
        ) && node
            .definition
            .outputs
            .iter()
            .any(|port| port.name == "failed")
            && super::automation::enabled(self.db, &self.run.id)?)
    }

    fn save(&mut self, receipt: ControlStageReceipt) -> Result<()> {
        let bytes = protection::protect(&serde_json::to_vec(&receipt)?)?;
        self.db.execute(
            "INSERT INTO workflow_control_receipts VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                self.run.id,
                receipt.node_id,
                receipt.plan_revision.as_deref().unwrap_or(""),
                receipt.unit_id.as_deref().unwrap_or(""),
                bytes
            ],
        )?;
        self.receipts
            .insert((receipt.node_id.clone(), receipt.unit_id.clone()), receipt);
        Ok(())
    }

    fn settle(&mut self, unit: Option<&str>, controls: bool) -> Result<()> {
        // A pass can make new control receipts or activate a ready approval.
        // Revisit downstream nodes regardless of their published order.
        loop {
            let before = self.receipts.len();
            let mut approved = false;
            for node in &self.definition.nodes {
                if self.unit_nodes.contains(node.instance.id.as_str()) == unit.is_some() {
                    if controls && let Some(receipt) = self.evaluate(node, unit)? {
                        self.save(receipt)?;
                    }
                    if node.instance.kind == BuiltinNode::ApproveImplementation && unit.is_none() {
                        approved |= self.activate_approval(node)?;
                    }
                }
            }
            if !approved && before == self.receipts.len() {
                return Ok(());
            }
        }
    }

    fn activate_approval(&self, node: &PinnedNode) -> Result<bool> {
        let ApprovalSelection::Ready { plan, task } = self.approval_inputs(node)? else {
            return Ok(false);
        };
        super::approval::activate_selected_approval(self.db, self.run, node, &plan, &task)
    }

    fn approval_inputs(&self, node: &PinnedNode) -> Result<ApprovalSelection> {
        if let Some(receipt) = self.receipt(&node.instance.id, None)
            && receipt.state == ControlStageState::Skipped
        {
            return Ok(ApprovalSelection::Skipped {
                reason: receipt
                    .reason
                    .clone()
                    .unwrap_or_else(|| "The approval's branch was skipped.".into()),
            });
        }
        let (mut ready, mut plan, mut task) = (false, None, None);
        let mut waiting = None;
        for wire in self
            .definition
            .wires
            .iter()
            .filter(|wire| wire.to.node == node.instance.id)
        {
            let artifact = match self.dependency(wire, None)? {
                Arrival::Ignored => {
                    return Ok(ApprovalSelection::Skipped {
                        reason: format!(
                            "The selected branches did not reach approval input {}.",
                            wire.to.port
                        ),
                    });
                }
                Arrival::Pending => {
                    waiting.get_or_insert_with(|| {
                        format!(
                            "Waiting for {} from {}.{}.",
                            wire.to.port, wire.from.node, wire.from.port,
                        )
                    });
                    continue;
                }
                Arrival::Selected(artifact) => artifact,
            };
            match wire.to.port.as_str() {
                "ready" => ready = true,
                "plan" => plan = artifact,
                "task" => task = artifact,
                _ => {}
            }
        }
        if let Some(reason) = waiting {
            return Ok(ApprovalSelection::Waiting { reason });
        }
        match (ready, plan, task) {
            (true, Some(plan), Some(task)) => Ok(ApprovalSelection::Ready { plan, task }),
            _ => Ok(ApprovalSelection::Waiting {
                reason: "Implementation approval needs the selected plan and linked work.".into(),
            }),
        }
    }

    fn evaluate(
        &self,
        node: &PinnedNode,
        unit: Option<&str>,
    ) -> Result<Option<ControlStageReceipt>> {
        use BuiltinNode::*;
        let node_id = node.instance.id.as_str();
        if self.receipt(node_id, unit).is_some() || matches!(node.instance.kind, Start | TypedStart)
        {
            return Ok(None);
        }
        let dependencies = self
            .definition
            .wires
            .iter()
            .filter(|wire| wire.to.node == node_id)
            .map(|wire| Ok((wire, self.dependency(wire, unit)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let selected_artifact = |port: &str| {
            dependencies
                .iter()
                .find_map(|(wire, outcome)| match outcome {
                    Arrival::Selected(Some(artifact)) if wire.to.port == port => Some(artifact),
                    _ => None,
                })
        };
        let pinned_inputs = || {
            dependencies
                .iter()
                .filter_map(|(wire, outcome)| match outcome {
                    Arrival::Selected(Some(artifact))
                        if !node.definition.inputs.iter().any(|port| {
                            port.value == PortType::Control && port.name == wire.to.port
                        }) =>
                    {
                        Some((wire.to.port.clone(), artifact.clone()))
                    }
                    _ => None,
                })
                .collect::<std::collections::BTreeMap<String, WorkflowArtifact>>()
        };
        let input_ids = || {
            pinned_inputs()
                .into_iter()
                .map(|(port, artifact)| (port, artifact.id))
                .collect()
        };
        let mut diagnostic = None;
        let mut source_run = None;
        let mut forwarded_outputs = std::collections::BTreeMap::new();
        let relevant = if node.instance.kind == ForEachCollect {
            let count = selected_artifact("items")
                .map(super::iteration::list_value)
                .transpose()?
                .map_or(0, |list| list.values.len());
            let mut finished = 0;
            let mut results = 0;
            dependencies
                .iter()
                .filter(|(wire, _)| match wire.to.port.as_str() {
                    "finished" => {
                        finished += 1;
                        finished <= count
                    }
                    "result" => {
                        results += 1;
                        results <= count
                    }
                    _ => true,
                })
                .collect::<Vec<_>>()
        } else {
            dependencies.iter().collect()
        };
        let ignored = relevant.iter().any(|(wire, outcome)| {
            matches!(outcome, Arrival::Ignored)
                && (node.instance.kind != CollectEvidence || wire.to.port == "ready")
        });
        let pending = relevant
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Pending));
        let selected = relevant
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Selected(_)));
        let (state, port, reason, attempt) = if let NodeConfiguration::RepeatOutput(configuration) =
            &node.instance.configuration
        {
            use super::repetition::{RepeatOutputConfiguration, RepeatSelection, RepeatSignal};
            match configuration.select(|port| {
                match dependencies
                    .iter()
                    .find(|(wire, _)| wire.to.port == port)
                    .map(|(_, arrival)| arrival)
                {
                    Some(Arrival::Selected(_)) => RepeatSignal::Selected,
                    Some(Arrival::Ignored) => RepeatSignal::Skipped,
                    _ => RepeatSignal::Waiting,
                }
            })? {
                RepeatSelection::Waiting => return Ok(None),
                RepeatSelection::Skipped => (ControlStageState::Skipped, None, None, None),
                RepeatSelection::MissingOutput => (
                    ControlStageState::Failed,
                    Some("failed"),
                    Some("The approved iteration did not produce every configured export"),
                    None,
                ),
                RepeatSelection::Approved(round) => {
                    for name in configuration.exports.keys() {
                        let input = RepeatOutputConfiguration::value_port(round, name);
                        let artifact = selected_artifact(&input)
                            .context("The approved iteration lost an export")?;
                        forwarded_outputs.insert(
                            name.clone(),
                            ForwardedOutput {
                                input,
                                artifact_id: artifact.id.clone(),
                            },
                        );
                    }
                    (ControlStageState::Completed, Some("completed"), None, None)
                }
            }
        } else if ignored && (node.instance.kind != Join || !pending && !selected) {
            (
                ControlStageState::Skipped,
                None,
                Some("No execution reaches this stage through the selected branches"),
                None,
            )
        } else if pending {
            return Ok(None);
        } else {
            match node.instance.kind {
                QualityGate => {
                    let inputs = pinned_inputs();
                    let NodeConfiguration::QualityGate(config) = &node.instance.configuration
                    else {
                        anyhow::bail!("Invalid pinned quality gate");
                    };
                    let Some(ArtifactValue::EvidenceSet(evidence)) =
                        inputs.get("evidence").map(|entry| &entry.value)
                    else {
                        anyhow::bail!("The quality gate lost its pinned evidence set");
                    };
                    let (port, report) = config.evaluate(evidence)?;
                    self.record_stage_artifact(
                        node_id,
                        unit,
                        "decision",
                        ArtifactValue::JsonObject(report),
                        &inputs,
                    )?;
                    (ControlStageState::Completed, Some(port), None, None)
                }
                CollectEvidence => {
                    use super::data_nodes::{EvidenceOutcome, WorkflowEvidence, evidence_text};
                    let mut inputs = std::collections::BTreeMap::new();
                    let mut evidence = Vec::new();
                    for (wire, arrival) in &dependencies {
                        if wire.to.port != "evidence" {
                            continue;
                        }
                        let outcome = if let Arrival::Selected(Some(artifact)) = arrival {
                            inputs.insert(
                                format!("evidence.{}", evidence.len() + 1),
                                artifact.clone(),
                            );
                            EvidenceOutcome::Completed {
                                artifact_id: artifact.id.clone(),
                                value: serde_json::to_value(&artifact.value)?,
                            }
                        } else {
                            let source_unit =
                                unit.filter(|_| self.unit_nodes.contains(wire.from.node.as_str()));
                            if self
                                .receipt(&wire.from.node, source_unit)
                                .is_some_and(|receipt| receipt.state == ControlStageState::Failed)
                            {
                                EvidenceOutcome::Failed
                            } else {
                                EvidenceOutcome::Skipped
                            }
                        };
                        evidence.push(WorkflowEvidence {
                            source: wire.from.clone(),
                            outcome,
                        });
                    }
                    match evidence_text(&evidence) {
                        Ok(text) => {
                            self.record_stage_artifact(
                                node_id,
                                unit,
                                "evidence",
                                ArtifactValue::EvidenceSet(evidence),
                                &inputs,
                            )?;
                            self.record_stage_artifact(
                                node_id,
                                unit,
                                "text",
                                ArtifactValue::RenderedText(text),
                                &inputs,
                            )?;
                            (ControlStageState::Completed, Some("completed"), None, None)
                        }
                        Err(error) => {
                            diagnostic = Some(crate::diagnostics::preview(&error.to_string()));
                            (ControlStageState::Failed, Some("failed"), None, None)
                        }
                    }
                }
                SetFields | FilterMapList | BuildText | FindWork | ValidateOutput => {
                    let inputs = pinned_inputs();
                    let outputs = self.transform_stage(node, &inputs);
                    match outputs {
                        Ok(outputs) => {
                            for (port, value) in outputs {
                                self.record_stage_artifact(node_id, unit, port, value, &inputs)?;
                            }
                            (ControlStageState::Completed, Some("completed"), None, None)
                        }
                        Err(error) => {
                            diagnostic = Some(crate::diagnostics::preview(&error.to_string()));
                            (ControlStageState::Failed, Some("failed"), None, None)
                        }
                    }
                }
                RequestInput | Approval => {
                    let Some(port) = super::decision_nodes::stage_port(
                        self.db,
                        self.run,
                        node,
                        unit,
                        &input_ids(),
                    )?
                    else {
                        return Ok(None);
                    };
                    (ControlStageState::Completed, Some(port), None, None)
                }
                UpdateProgress | ManageWork | MaterializeBacklog => {
                    use super::agile_nodes::AgileStageOutcome;
                    let Some(outcome) = super::agile_nodes::stage_port(
                        self.db,
                        self.run,
                        node,
                        unit,
                        &input_ids(),
                    )?
                    else {
                        return Ok(None);
                    };
                    match outcome {
                        AgileStageOutcome::Completed => {
                            (ControlStageState::Completed, Some("completed"), None, None)
                        }
                        AgileStageOutcome::Failed { reason } => {
                            diagnostic = Some(reason);
                            (ControlStageState::Failed, Some("failed"), None, None)
                        }
                    }
                }
                RunBacklog => {
                    use super::backlog::execution::{ExecutionState, stage_state};
                    let backlog =
                        selected_artifact("backlog").context("The backlog input is unavailable")?;
                    match stage_state(self.db, self.run, node, unit, &backlog.id)? {
                        ExecutionState::Waiting(_) => return Ok(None),
                        ExecutionState::Completed => {
                            (ControlStageState::Completed, Some("completed"), None, None)
                        }
                        ExecutionState::Failed(reason) => {
                            diagnostic = Some(reason);
                            (ControlStageState::Failed, Some("failed"), None, None)
                        }
                    }
                }
                SearchPublicRepositories
                | OpenOrUpdatePullRequest
                | WaitForCi
                | MergePullRequest => {
                    let inputs = input_ids();
                    let Some(port) =
                        super::github::stage_port(self.db, self.run, node, unit, &inputs)?
                    else {
                        return Ok(None);
                    };
                    let state = if matches!(port, "failed" | "stale") {
                        diagnostic =
                            super::github::stage_reason(self.db, self.run, node, unit, &inputs)?;
                        ControlStageState::Failed
                    } else {
                        ControlStageState::Completed
                    };
                    (state, Some(port), None, None)
                }
                VerifyIntegratedChecks => {
                    let inputs = input_ids();
                    let Some(port) = super::integrated_checks::stage_port(
                        self.db, self.run, node, None, &inputs,
                    )?
                    else {
                        return Ok(None);
                    };
                    diagnostic =
                        super::integrated_checks::stage_reason(self.db, self.run, node, &inputs)?;
                    (ControlStageState::Completed, Some(port), None, None)
                }
                VerifyTestPhase => {
                    let Some(port) =
                        super::test_phase::stage_port(self.db, self.run, node, unit, &input_ids())?
                    else {
                        return Ok(None);
                    };
                    (ControlStageState::Completed, Some(port), None, None)
                }
                PrepareWorktree => {
                    if let (Some(decision), Some(task)) =
                        (selected_artifact("decision"), selected_artifact("task"))
                    {
                        super::automation::preparation::note_ready_preparation(
                            self.db, self.run, node_id, decision, task,
                        )?;
                    }
                    return Ok(None);
                }
                Implement | ReviewCandidate
                    if super::automation::agents_delivery::enabled_for(
                        self.db,
                        &self.run.id,
                        node,
                    )? =>
                {
                    let inputs = pinned_inputs();
                    let Some(outcome) = super::automation::agents_delivery::evaluate(
                        self.db, self.run, node, unit, &inputs,
                    )?
                    else {
                        super::automation::note_ready_agent(
                            self.db, self.run, node, unit, &inputs,
                        )?;
                        return Ok(None);
                    };
                    diagnostic = outcome.reason;
                    source_run = outcome.source_run_id;
                    (outcome.state, Some(outcome.port), None, None)
                }
                VerifyCandidate | AcceptCandidate | VerifyCommit
                    if self.automatic_unit_action(node)? =>
                {
                    let inputs = pinned_inputs();
                    let Some(outcome) = super::automation::unit_actions::evaluate(
                        self.db, self.run, node, unit, &inputs,
                    )?
                    else {
                        return Ok(None);
                    };
                    diagnostic = outcome.reason;
                    source_run = outcome.source_run_id;
                    (outcome.state, Some(outcome.port), None, None)
                }
                Agent => {
                    let inputs = pinned_inputs();
                    let Some(outcome) =
                        super::generic_agent::evaluate(self.db, self.run, node, unit, &inputs)?
                    else {
                        super::automation::note_ready_agent(
                            self.db, self.run, node, unit, &inputs,
                        )?;
                        return Ok(None);
                    };
                    diagnostic = outcome.reason;
                    source_run = outcome.source_run_id;
                    (outcome.state, Some(outcome.port), None, None)
                }
                SummarizeResult => {
                    let input = selected_artifact("summary")
                        .context("The Result has no selected summary")?;
                    let ArtifactValue::RenderedText(summary) = &input.value else {
                        anyhow::bail!("Result needs text produced by the workflow");
                    };
                    let mut inputs =
                        std::collections::BTreeMap::from([("summary".into(), input.clone())]);
                    let mut artifacts = Vec::new();
                    for (wire, arrival) in &dependencies {
                        if wire.to.port == "evidence"
                            && let Arrival::Selected(Some(artifact)) = arrival
                            && !artifacts.contains(&artifact.id)
                        {
                            artifacts.push(artifact.id.clone());
                            inputs
                                .insert(format!("evidence.{}", artifacts.len()), artifact.clone());
                        }
                    }
                    let value = ArtifactValue::ResultSummary {
                        summary: summary.clone(),
                        artifacts,
                    };
                    self.record_stage_artifact(node_id, unit, "summary", value, &inputs)?;
                    (ControlStageState::Completed, Some("completed"), None, None)
                }
                LoadWorkContext => {
                    let inputs = pinned_inputs();
                    match self.transform_stage(node, &inputs) {
                        Ok(outputs) => {
                            for (port, value) in outputs {
                                self.record_stage_artifact(node_id, unit, port, value, &inputs)?;
                            }
                            (ControlStageState::Completed, Some("loaded"), None, None)
                        }
                        Err(error) => {
                            diagnostic = Some(crate::diagnostics::preview(&error.to_string()));
                            (ControlStageState::Failed, Some("failed"), None, None)
                        }
                    }
                }
                CompareModels => {
                    let Some(outcome) = super::comparison::stage::evaluate(
                        self.db,
                        self.run,
                        node,
                        unit,
                        &pinned_inputs(),
                    )?
                    else {
                        return Ok(None);
                    };
                    diagnostic = outcome.reason;
                    (outcome.state, Some(outcome.port), None, None)
                }
                Condition => {
                    let NodeConfiguration::Condition { predicate } = &node.instance.configuration
                    else {
                        anyhow::bail!("The pinned condition is invalid");
                    };
                    let artifact = dependencies
                        .iter()
                        .find_map(|(wire, outcome)| match outcome {
                            Arrival::Selected(Some(artifact)) if wire.to.port == "value" => {
                                Some(artifact)
                            }
                            _ => None,
                        })
                        .context("The condition lost its pinned evidence")?;
                    let port = if predicate.test_recorded_artifact(self.db, &artifact.value)? {
                        "matched"
                    } else {
                        "unmatched"
                    };
                    (ControlStageState::Completed, Some(port), None, None)
                }
                Parallel => (ControlStageState::Completed, Some("branches"), None, None),
                Join => (ControlStageState::Completed, Some("joined"), None, None),
                IterationLimit => (
                    ControlStageState::Failed,
                    Some("failed"),
                    Some("Reached the configured iteration limit"),
                    None,
                ),
                BuildList => {
                    let NodeConfiguration::BuildList(configuration) = &node.instance.configuration
                    else {
                        anyhow::bail!("Missing pinned list configuration");
                    };
                    let evidence = selected_artifact("value");
                    match configuration.build(evidence) {
                        Ok(list) => {
                            let inputs = evidence
                                .into_iter()
                                .map(|artifact| ("value".into(), artifact.clone()))
                                .collect();
                            self.record_iteration_artifact(
                                node_id,
                                "items",
                                ArtifactValue::JsonList(list),
                                &inputs,
                            )?;
                            (ControlStageState::Completed, Some("completed"), None, None)
                        }
                        Err(_) => (
                            ControlStageState::Failed,
                            Some("failed"),
                            Some("The configured list source did not produce a bounded JSON array"),
                            None,
                        ),
                    }
                }
                ForEachBegin => {
                    let NodeConfiguration::ForEachBegin(configuration) =
                        &node.instance.configuration
                    else {
                        anyhow::bail!("Missing pinned iteration size");
                    };
                    let source =
                        selected_artifact("items").context("Missing pinned iteration list")?;
                    if let Some(list) =
                        super::iteration::bind_list(source, configuration.max_items)?
                    {
                        self.record_iteration_artifact(
                            node_id,
                            "items",
                            ArtifactValue::JsonList(list),
                            &std::collections::BTreeMap::from([("items".into(), source.clone())]),
                        )?;
                        (ControlStageState::Completed, Some("ready"), None, None)
                    } else {
                        (
                            ControlStageState::Failed,
                            Some("failed"),
                            Some("The list exceeds the configured For each limit"),
                            None,
                        )
                    }
                }
                ForEachGate => {
                    let NodeConfiguration::ForEachGate(configuration) =
                        &node.instance.configuration
                    else {
                        anyhow::bail!("Missing pinned iteration ordinal");
                    };
                    let source =
                        selected_artifact("items").context("Missing pinned iteration list")?;
                    if let Some(item) = super::iteration::item_for(source, configuration.ordinal)? {
                        let previous = selected_artifact("previous")
                            .context("Missing prior iteration result")?;
                        self.record_iteration_artifact(
                            node_id,
                            "item",
                            ArtifactValue::JsonItem(item),
                            &std::collections::BTreeMap::from([
                                ("items".into(), source.clone()),
                                ("previous".into(), previous.clone()),
                            ]),
                        )?;
                        (ControlStageState::Completed, Some("iterate"), None, None)
                    } else {
                        (
                            ControlStageState::Skipped,
                            None,
                            Some("The pinned list has no item at this ordinal"),
                            None,
                        )
                    }
                }
                ForEachCollect => {
                    let NodeConfiguration::ForEachCollect(configuration) =
                        &node.instance.configuration
                    else {
                        anyhow::bail!("Missing pinned iteration result type");
                    };
                    let source =
                        selected_artifact("items").context("Missing pinned iteration list")?;
                    let outputs = relevant.iter().filter_map(|(wire, outcome)| match outcome {
                        Arrival::Selected(Some(artifact)) if wire.to.port == "result" => {
                            Some(artifact)
                        }
                        _ => None,
                    });
                    let mut inputs =
                        std::collections::BTreeMap::from([("items".into(), source.clone())]);
                    let results = super::iteration::collect_results(
                        source,
                        configuration,
                        outputs.enumerate().map(|(index, artifact)| {
                            let ordinal = (index + 1) as u16;
                            inputs.insert(format!("result.{ordinal}"), artifact.clone());
                            (ordinal, artifact)
                        }),
                    )?;
                    self.record_iteration_artifact(
                        node_id,
                        "results",
                        ArtifactValue::IterationResults(results),
                        &inputs,
                    )?;
                    (ControlStageState::Completed, Some("completed"), None, None)
                }
                LoadContext => {
                    let document = dependencies
                        .iter()
                        .find_map(|(wire, arrival)| match arrival {
                            Arrival::Selected(Some(document)) if wire.to.port == "document" => {
                                Some(document)
                            }
                            _ => None,
                        })
                        .context("The context stage lost its pinned Markdown")?;
                    self.load_context(node_id, document)?;
                    (ControlStageState::Completed, Some("loaded"), None, None)
                }
                Command
                    if self.definition.wires.iter().any(|wire| {
                        wire.from.node == node_id
                            && (wire.from.port == "failed"
                                || self.nodes.get(wire.to.node.as_str()).is_some_and(|target| {
                                    target.instance.kind == CollectEvidence
                                        && wire.to.port == "evidence"
                                }))
                    }) =>
                {
                    let Some(attempt) = self.command_attempt(node_id)? else {
                        return Ok(None);
                    };
                    if attempt.status == "succeeded" && attempt.artifact.is_some() {
                        (
                            ControlStageState::Completed,
                            Some("completed"),
                            None,
                            Some(attempt.id),
                        )
                    } else if attempt.status == "failed"
                        && attempt.failure.is_some_and(confirmed_failure)
                    {
                        (
                            ControlStageState::Failed,
                            Some("failed"),
                            Some("The recorded command failure selected the error branch"),
                            Some(attempt.id),
                        )
                    } else {
                        return Ok(None);
                    }
                }
                McpTool => {
                    let Some((id, state, _)) = self.mcp_attempt(node_id)? else {
                        return Ok(None);
                    };
                    match state.as_str() {
                        "confirmed" => (
                            ControlStageState::Completed,
                            Some("completed"),
                            None,
                            Some(id),
                        ),
                        "rejected" | "not_sent" => (
                            ControlStageState::Failed,
                            Some("failed"),
                            Some("The local MCP tool did not complete"),
                            Some(id),
                        ),
                        _ => return Ok(None),
                    }
                }
                _ => return Ok(None),
            }
        };
        Ok(Some(ControlStageReceipt {
            node_id: node_id.into(),
            unit_id: unit.map(str::to_owned),
            plan_revision: self.run.plan_revision.clone(),
            state,
            selected_ports: port.into_iter().map(str::to_owned).collect(),
            reason: diagnostic.or_else(|| reason.map(str::to_owned)),
            source_run_id: source_run.or(attempt),
            forwarded_outputs,
            dependencies: dependencies
                .into_iter()
                .filter_map(|(wire, outcome)| {
                    let (selected, artifact_id) = match outcome {
                        Arrival::Selected(artifact) => (true, artifact.map(|artifact| artifact.id)),
                        Arrival::Ignored => (false, None),
                        Arrival::Pending => return None,
                    };
                    Some(ControlDependency {
                        from: wire.from.clone(),
                        to_port: wire.to.port.clone(),
                        selected,
                        artifact_id,
                    })
                })
                .collect(),
        }))
    }

    fn transform_stage(
        &self,
        node: &PinnedNode,
        inputs: &std::collections::BTreeMap<String, WorkflowArtifact>,
    ) -> Result<Vec<(&'static str, ArtifactValue)>> {
        use super::data_nodes::data_view;
        let value = match &node.instance.configuration {
            NodeConfiguration::ValidateOutput(config) => {
                let source = inputs
                    .get("source")
                    .context("The pinned output is missing")?;
                let values = match &source.value {
                    ArtifactValue::AgentResponse(response) => {
                        config.parse(&super::generic_agent::response_text(
                            self.db,
                            &self.run.project_id,
                            &self.run.id,
                            response,
                        )?)?
                    }
                    ArtifactValue::RenderedText(text) => config.parse(text)?,
                    ArtifactValue::JsonObject(values) => {
                        config.evaluate(serde_json::Value::Object(values.clone()))?
                    }
                    ArtifactValue::CommandResult(result) => match &result.output {
                        super::command::CommandData::Json(value) => {
                            config.evaluate(value.clone())?
                        }
                        super::command::CommandData::Text(text) => config.parse(text)?,
                    },
                    _ => anyhow::bail!(
                        "Select a saved Agent response, JSON object, text, or Command result"
                    ),
                };
                ("object", ArtifactValue::JsonObject(values))
            }
            NodeConfiguration::SetFields(config) => {
                let source = inputs
                    .get("source")
                    .map(|source| serde_json::to_value(&source.value))
                    .transpose()?;
                (
                    "object",
                    ArtifactValue::JsonObject(config.evaluate(source.as_ref().map(data_view))?),
                )
            }
            NodeConfiguration::FilterMapList(config) => {
                let input = inputs
                    .get("items")
                    .context("The list input is unavailable")?;
                let items = super::iteration::list_value(input)?;
                (
                    "items",
                    ArtifactValue::JsonList(config.evaluate(items, &input.id)?),
                )
            }
            NodeConfiguration::BuildText(config) => {
                let values = inputs
                    .get("values")
                    .map(|artifact| match &artifact.value {
                        ArtifactValue::JsonObject(values) => Ok(values),
                        _ => anyhow::bail!("The named template inputs must be an object"),
                    })
                    .transpose()?;
                (
                    "text",
                    ArtifactValue::RenderedText(config.evaluate(values)?),
                )
            }
            NodeConfiguration::LoadWorkContext { .. } => {
                let task = inputs
                    .get("task")
                    .map(|artifact| match &artifact.value {
                        ArtifactValue::Task(task) => super::agile_nodes::load_task_context(
                            self.db,
                            &self.run.project_id,
                            &self.run.id,
                            task,
                        ),
                        _ => anyhow::bail!("The context node requires pinned work"),
                    })
                    .transpose()?;
                let document = inputs
                    .get("document")
                    .map(|artifact| match &artifact.value {
                        ArtifactValue::Markdown(document) => Ok(document.clone()),
                        _ => anyhow::bail!("The context node requires pinned Markdown"),
                    })
                    .transpose()?;
                (
                    "context",
                    ArtifactValue::WorkflowContext(super::agile_nodes::WorkflowContext {
                        task,
                        document,
                    }),
                )
            }
            NodeConfiguration::FindWork { query } => {
                return super::agile_nodes::find_work(
                    self.db,
                    &self.run.project_id,
                    &self.run.id,
                    query,
                );
            }
            _ => anyhow::bail!("The node configuration does not match the selected operation"),
        };
        Ok(vec![value])
    }

    fn record_stage_artifact(
        &self,
        node: &str,
        unit: Option<&str>,
        port: &str,
        value: ArtifactValue,
        inputs: &std::collections::BTreeMap<String, WorkflowArtifact>,
    ) -> Result<()> {
        let artifact = super::artifacts::insert_scoped_artifact(
            self.db,
            &self.run.project_id,
            self.run.id.clone(),
            node.into(),
            port.into(),
            unit.map(str::to_owned),
            value,
        )?;
        let plan_key = self.run.plan_revision.as_deref().unwrap_or("");
        super::inspection::record_workflow_completion_inputs(self.db, &artifact, plan_key, inputs)
    }

    fn record_iteration_artifact(
        &self,
        node: &str,
        port: &str,
        value: ArtifactValue,
        inputs: &std::collections::BTreeMap<String, WorkflowArtifact>,
    ) -> Result<()> {
        let identity = inputs
            .iter()
            .map(|(port, artifact)| (port, &artifact.id))
            .collect::<Vec<_>>();
        let revision = super::digest(&serde_json::to_vec(&(identity, &value))?);
        let artifact = super::artifacts::insert_artifact(
            self.db,
            &self.run.project_id,
            self.run.id.clone(),
            node.into(),
            port.into(),
            value,
        )?;
        let plan_key = self.run.plan_revision.as_deref().unwrap_or("");
        super::execution::record_host_completion(self.db, &artifact, plan_key, &revision)?;
        super::inspection::record_workflow_completion_inputs(self.db, &artifact, plan_key, inputs)
    }

    fn load_context(&self, node: &str, document: &WorkflowArtifact) -> Result<()> {
        let exists: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_host_completions WHERE pipeline_run_id = ?1
             AND node_instance_id = ?2 AND execution_key = '')",
            params![self.run.id, node],
            |row| row.get(0),
        )?;
        if !exists {
            let artifact = super::artifacts::insert_artifact(
                self.db,
                &self.run.project_id,
                self.run.id.clone(),
                node.into(),
                "context".into(),
                document.value.clone(),
            )?;
            let inputs = std::collections::BTreeMap::from([("document".into(), document.clone())]);
            let revision = super::execution::host_request_revision(
                &inputs,
                &super::WorkflowHostRequest::LoadContext,
            )?;
            super::execution::record_host_completion(self.db, &artifact, "", &revision)?;
            super::inspection::record_workflow_completion_inputs(self.db, &artifact, "", &inputs)?;
        }
        Ok(())
    }

    fn dependency(&self, wire: &Wire, unit: Option<&str>) -> Result<Arrival> {
        let source_id = wire.from.node.as_str();
        let source_unit = if self.unit_nodes.contains(source_id) {
            let Some(unit) = unit else {
                return Ok(Arrival::Pending);
            };
            Some(unit)
        } else {
            None
        };
        let source = self
            .nodes
            .get(source_id)
            .context("Control wire lost its source")?;
        let port = source
            .definition
            .outputs
            .iter()
            .find(|port| port.name == wire.from.port)
            .context("Control wire lost its output port")?;
        let receipt = self.receipt(source_id, source_unit);
        let delivery_agent = matches!(
            source.instance.kind,
            BuiltinNode::Implement | BuiltinNode::ReviewCandidate
        ) && source
            .definition
            .outputs
            .iter()
            .any(|port| port.name == "failed");
        let automatic_unit_action = self.automatic_unit_action(source)?;
        if receipt.is_some_and(|receipt| {
            receipt.state == ControlStageState::Skipped
                || receipt.state == ControlStageState::Failed
                    && (is_control(source.instance.kind) || delivery_agent || automatic_unit_action)
                    && port.value != PortType::Control
                    && !(source.instance.kind == BuiltinNode::WaitForCi
                        && port.name == "ci"
                        && receipt.selected_ports.iter().any(|port| port == "failed"))
        }) {
            return Ok(Arrival::Ignored);
        }
        if port.value == PortType::Control
            && (is_control(source.instance.kind)
                || source.instance.kind == BuiltinNode::McpTool
                || automatic_unit_action
                || (source.instance.kind == BuiltinNode::Command || delivery_agent)
                    && receipt.is_some())
        {
            return Ok(match receipt {
                Some(receipt) if receipt.selected_ports.contains(&wire.from.port) => {
                    Arrival::Selected(None)
                }
                Some(_) => Arrival::Ignored,
                None => Arrival::Pending,
            });
        }
        if delivery_agent && wire.from.port == "failed" {
            return Ok(
                if self
                    .source_artifact(&wire.from, source_unit, true)?
                    .is_some()
                    && !super::automation::agents_delivery::enabled_for(
                        self.db,
                        &self.run.id,
                        source,
                    )?
                {
                    Arrival::Ignored
                } else {
                    Arrival::Pending
                },
            );
        }
        if wire.from.port == "failed"
            && matches!(
                source.instance.kind,
                BuiltinNode::VerifyCandidate
                    | BuiltinNode::AcceptCandidate
                    | BuiltinNode::VerifyCommit
            )
            && !automatic_unit_action
        {
            return Ok(
                if self
                    .source_artifact(&wire.from, source_unit, true)?
                    .is_some()
                {
                    Arrival::Ignored
                } else {
                    Arrival::Pending
                },
            );
        }
        if delivery_agent
            && super::automation::agents_delivery::enabled_for(self.db, &self.run.id, source)?
        {
            let Some(attempt) = receipt.and_then(|receipt| receipt.source_run_id.as_deref()) else {
                return Ok(Arrival::Pending);
            };
            let output = super::automation::agents_delivery::sealed_output(
                self.db,
                self.run,
                source_id,
                source_unit,
                attempt,
                &wire.from.port,
            )?;
            return Ok(output.map_or(Arrival::Pending, |artifact| {
                Arrival::Selected(Some(artifact))
            }));
        }
        if automatic_unit_action {
            let Some(receipt) = receipt else {
                return Ok(Arrival::Pending);
            };
            let output = super::automation::unit_actions::sealed_output(
                self.db,
                self.run,
                source_id,
                source_unit,
                &wire.from.port,
            )?;
            return Ok(match output {
                Some(artifact) => Arrival::Selected(Some(artifact)),
                None if receipt.selected_ports.iter().any(|port| port == "failed") => {
                    Arrival::Ignored
                }
                None => Arrival::Pending,
            });
        }
        if source.instance.kind == BuiltinNode::RepeatOutput {
            let Some(export) =
                receipt.and_then(|receipt| receipt.forwarded_outputs.get(&wire.from.port))
            else {
                return Ok(Arrival::Pending);
            };
            let incoming = self
                .definition
                .wires
                .iter()
                .find(|incoming| incoming.to.node == source_id && incoming.to.port == export.input)
                .context("The approved iteration lost its export wire")?;
            return Ok(match self.dependency(incoming, source_unit)? {
                Arrival::Selected(Some(artifact)) if artifact.id == export.artifact_id => {
                    Arrival::Selected(Some(artifact))
                }
                _ => Arrival::Pending,
            });
        }
        if source.instance.kind == BuiltinNode::Command {
            if wire.from.port == "failed" {
                return Ok(Arrival::Pending);
            }
            if receipt.is_some_and(|record| record.state == ControlStageState::Failed) {
                return Ok(Arrival::Ignored);
            }
            let artifact = self
                .command_attempt(source_id)?
                .and_then(|attempt| attempt.artifact);
            return Ok(artifact.map_or(Arrival::Pending, |artifact| {
                Arrival::Selected(Some(artifact))
            }));
        }
        if source.instance.kind == BuiltinNode::McpTool {
            if receipt.is_some_and(|record| record.state == ControlStageState::Failed) {
                return Ok(Arrival::Ignored);
            }
            let artifact = match self.mcp_attempt(source_id)? {
                Some((_, state, Some(id))) if state == "confirmed" => {
                    super::mcp_tools::load_mcp_artifact(
                        self.db,
                        &self.run.project_id,
                        &self.run.id,
                        &id,
                    )?
                }
                _ => None,
            };
            return Ok(artifact.map_or(Arrival::Pending, |value| Arrival::Selected(Some(value))));
        }
        let artifact =
            self.source_artifact(&wire.from, source_unit, port.value == PortType::Control)?;
        if artifact.is_none()
            && receipt.is_some_and(|receipt| receipt.state == ControlStageState::Failed)
        {
            return Ok(Arrival::Ignored);
        }
        Ok(artifact.map_or(Arrival::Pending, |artifact| {
            Arrival::Selected(Some(artifact))
        }))
    }

    fn source_artifact(
        &self,
        source: &Endpoint,
        unit: Option<&str>,
        control: bool,
    ) -> Result<Option<WorkflowArtifact>> {
        let mut statement = self.db.prepare(
            "SELECT a.payload, h.execution_key, EXISTS (
                 SELECT 1 FROM workflow_completion_inputs i WHERE i.output_artifact_id = a.id
                   AND i.execution_key = ?5) AS exact_receipt, EXISTS (
                 SELECT 1 FROM workflow_completion_inputs i WHERE i.output_artifact_id = a.id
                   AND ?6 AND i.execution_key = '')
             FROM workflow_artifacts a LEFT JOIN workflow_host_completions h ON h.artifact_id = a.id
             WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.unit_id IS ?3
               AND (?4 IS NULL OR a.port_name = ?4)
             ORDER BY CASE WHEN ?6 THEN exact_receipt ELSE 0 END DESC,
                 h.execution_key = ?5 DESC, a.port_name, a.id",
        )?;
        let key = self.run.plan_revision.as_deref().unwrap_or("");
        let typed_entry = self
            .nodes
            .get(source.node.as_str())
            .is_some_and(|node| node.instance.kind == BuiltinNode::TypedStart);
        let mut rows = statement.query(params![
            self.run.id,
            source.node,
            unit,
            (!control).then_some(&source.port),
            key,
            typed_entry,
        ])?;
        let mut selected = None;
        let mut selected_exact_start = false;
        while let Some(row) = rows.next()? {
            let payload: Vec<u8> = row.get(0)?;
            let host_key: Option<String> = row.get(1)?;
            let exact_receipt: bool = row.get(2)?;
            let launch_receipt: bool = row.get(3)?;
            let artifact: WorkflowArtifact =
                serde_json::from_slice(&protection::unprotect(&payload)?)?;
            let valid = match &artifact.value {
                ArtifactValue::Plan { .. } | ArtifactValue::BacklogPlan(_) => {
                    self.run.plan_revision.as_deref() == Some(&artifact.id)
                }
                ArtifactValue::Review { run_id } => self.attempt_matches(run_id)?,
                ArtifactValue::Candidate(reference) => {
                    self.attempt_matches(&reference.target.run_id)?
                }
                ArtifactValue::Approval(decision) => {
                    host_key.as_deref() == Some(key)
                        && super::approval::require_active_decision(self.db, decision, &artifact.id)
                            .is_ok()
                }
                ArtifactValue::Release(reference) => {
                    self.release_current(&reference.release.id)?
                        && self.prepare_matches(source, key, &reference.release)?
                }
                ArtifactValue::Worktree { .. } => host_key.as_deref() == Some(key),
                _ => {
                    host_key
                        .as_deref()
                        .is_some_and(|host| host.is_empty() || host == key)
                        || exact_receipt
                        || launch_receipt
                        || unit.is_some() && self.run.plan_revision.is_some()
                }
            };
            if valid {
                if control || host_key.as_deref() == Some(key) {
                    return Ok(Some(artifact));
                }
                let exact_start = typed_entry && source.port == "inputs" && exact_receipt;
                if selected_exact_start && !exact_start {
                    break;
                }
                if selected.replace(artifact).is_some() {
                    return Ok(None);
                }
                selected_exact_start = exact_start;
            }
        }
        Ok(selected)
    }

    fn prepare_matches(
        &self,
        source: &Endpoint,
        key: &str,
        release: &crate::jobs::PipelineRelease,
    ) -> Result<bool> {
        let payload: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT a.payload FROM workflow_host_completions h
             JOIN workflow_artifacts a ON a.id = h.artifact_id AND a.kind = 'worktree'
             WHERE h.pipeline_run_id = ?1 AND h.node_instance_id = ?2 AND h.execution_key = ?3",
                params![self.run.id, source.node, key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(payload) = payload else {
            return Ok(false);
        };
        let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        Ok(
            matches!(artifact.value, ArtifactValue::Worktree { checkout_id, task }
            if checkout_id == release.checkout_id && task.task_id == release.task_id),
        )
    }

    fn release_current(&self, release: &str) -> Result<bool> {
        let live: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM job_pipeline_releases WHERE id = ?1 AND project_id = ?2
              AND pipeline_run_id = ?3 AND revoked = 0)",
            params![release, self.run.project_id, self.run.id],
            |row| row.get(0),
        )?;
        Ok(live
            && Some(true)
                == super::approval::workflow_release_authorized(
                    self.db,
                    &self.run.project_id,
                    &self.run.id,
                    release,
                )?)
    }

    fn attempt_matches(&self, attempt: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
             JOIN runs r ON r.id = a.run_id AND r.status = 'succeeded'
             WHERE a.run_id = ?1 AND o.pipeline_run_id = ?2 AND o.plan_revision IS ?3
               AND a.ordinal = (SELECT MAX(n.ordinal) FROM job_attempts n WHERE n.job_id = a.job_id))",
            params![attempt, self.run.id, self.run.plan_revision], |row| row.get(0),
        )?)
    }

    fn mcp_attempt(&self, node: &str) -> Result<Option<(String, String, Option<String>)>> {
        Ok(self.db.query_row("SELECT a.run_id, a.state, a.artifact_id FROM workflow_mcp_attempts a
            JOIN runs r ON r.id = a.run_id
            WHERE a.job_id = (SELECT m.job_id FROM workflow_mcp_jobs m JOIN jobs j ON j.id = m.job_id
                JOIN job_pipeline_origins o ON o.job_id = j.id
                WHERE m.pipeline_run_id = ?1 AND m.node_instance_id = ?2 AND o.plan_revision IS ?3
                ORDER BY j.sequence DESC LIMIT 1)
              AND r.status IN ('succeeded','failed','interrupted','cancelled')",
            params![self.run.id, node, self.run.plan_revision], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?)
    }

    fn command_attempt(&self, node: &str) -> Result<Option<CommandAttempt>> {
        let row = self
            .db
            .query_row(
                "SELECT r.id, r.status, f.report, c.payload FROM job_pipeline_origins o
             JOIN jobs j ON j.id = o.job_id
             LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (
                  SELECT MAX(n.ordinal) FROM job_attempts n WHERE n.job_id = j.id)
             LEFT JOIN runs r ON r.id = a.run_id
             LEFT JOIN job_failures f ON f.run_id = a.run_id
             LEFT JOIN workflow_command_results c ON c.run_id = a.run_id
             WHERE o.pipeline_run_id = ?1 AND o.node_instance_id = ?2 AND o.plan_revision IS ?3
             ORDER BY j.sequence DESC LIMIT 1",
                params![self.run.id, node, self.run.plan_revision],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((Some(id), Some(status), failure, artifact)) = row else {
            return Ok(None);
        };
        Ok(Some(CommandAttempt {
            id,
            status,
            failure: failure
                .map(|payload| serde_json::from_str(&payload))
                .transpose()?,
            artifact: artifact
                .map(|payload| -> Result<_> {
                    Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?)
                })
                .transpose()?,
        }))
    }
}

struct CommandAttempt {
    id: String,
    status: String,
    failure: Option<FailureObservation>,
    artifact: Option<WorkflowArtifact>,
}

fn confirmed_failure(details: FailureObservation) -> bool {
    !matches!(
        details.cause,
        Some(FailureCause::StopUnconfirmed | FailureCause::RuntimeRestart)
    ) && details.process.is_some_and(|process| !process.cancelled)
}
