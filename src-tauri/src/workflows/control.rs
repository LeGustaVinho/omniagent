use crate::workflows::ArchivedNode;

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{protection, store::Store};

use super::{
    ArtifactValue, BuiltinNode, Endpoint, NodeConfiguration, PinnedNode, PortType, Wire,
    WorkflowArtifact, WorkflowDefinition, WorkflowRun,
    runtime::{load_definition, load_workflow_run},
};

mod orchestration;
pub(super) mod repair;

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
        BuiltinNode::Archived(ArchivedNode::Condition)
            | BuiltinNode::End
            | BuiltinNode::Archived(ArchivedNode::Parallel)
            | BuiltinNode::Archived(ArchivedNode::Join)
            | BuiltinNode::Fork
            | BuiltinNode::JoinResults
            | BuiltinNode::Loop
            | BuiltinNode::Archived(ArchivedNode::IterationLimit)
            | BuiltinNode::Archived(ArchivedNode::BuildList)
            | BuiltinNode::Archived(ArchivedNode::SetFields)
            | BuiltinNode::Archived(ArchivedNode::ValidateOutput)
            | BuiltinNode::Archived(ArchivedNode::PromptDecision)
            | BuiltinNode::Archived(ArchivedNode::PromptOutcome)
            | BuiltinNode::Archived(ArchivedNode::CollectEvidence)
            | BuiltinNode::Archived(ArchivedNode::QualityGate)
            | BuiltinNode::Archived(ArchivedNode::FilterMapList)
            | BuiltinNode::Archived(ArchivedNode::BuildText)
            | BuiltinNode::Archived(ArchivedNode::RequestInput)
            | BuiltinNode::Archived(ArchivedNode::Approval)
            | BuiltinNode::Wait
            | BuiltinNode::Human
            | BuiltinNode::Archived(ArchivedNode::FindWork)
            | BuiltinNode::Archived(ArchivedNode::UpdateProgress)
            | BuiltinNode::Archived(ArchivedNode::CompareModels)
            | BuiltinNode::Agent
            | BuiltinNode::Archived(ArchivedNode::LoadWorkContext)
            | BuiltinNode::Archived(ArchivedNode::ManageWork)
            | BuiltinNode::Archived(ArchivedNode::MaterializeBacklog)
            | BuiltinNode::Archived(ArchivedNode::RunBacklog)
            | BuiltinNode::Archived(ArchivedNode::VerifyTestPhase)
            | BuiltinNode::Archived(ArchivedNode::VerifyIntegratedChecks)
            | BuiltinNode::Archived(ArchivedNode::SearchPublicRepositories)
            | BuiltinNode::Archived(ArchivedNode::OpenOrUpdatePullRequest)
            | BuiltinNode::Archived(ArchivedNode::WaitForCi)
            | BuiltinNode::Archived(ArchivedNode::MergePullRequest)
            | BuiltinNode::Archived(ArchivedNode::SummarizeResult)
    ) || kind.generated_iteration()
}

pub(super) fn uses_control(definition: &WorkflowDefinition) -> bool {
    definition.nodes.iter().any(|node| {
        is_control(node.instance.kind)
            || matches!(
                node.instance.kind,
                BuiltinNode::Archived(ArchivedNode::McpTool) | BuiltinNode::TypedStart
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
            BuiltinNode::Archived(ArchivedNode::Implement)
                | BuiltinNode::Archived(ArchivedNode::Command)
                | BuiltinNode::Archived(ArchivedNode::VerifyTestPhase)
        ) || (node.instance.kind == BuiltinNode::Agent
            && node.definition.executor == super::Executor::WriteAgent)
    }) {
        let node = step.instance.id.as_str();
        if super::write_scope::writer(step) {
            super::write_scope::approval_node(nodes, wires, node)?;
            continue;
        }
        let provider = source(node, "release").context("writing node requires a pinned release")?;
        let worktree =
            source(node, "worktree").context("writing node requires a pinned worktree")?;
        ensure!(
            kind(&provider.node) == Some(BuiltinNode::Archived(ArchivedNode::PrepareWorktree))
                && provider.port == "release"
                && worktree.node == provider.node
                && worktree.port == "worktree",
            "writing node {node} must use release and worktree from the same preparation"
        );
        let approver = source(&provider.node, "decision")
            .context("write preparation must consume a pinned implementation approval")?;
        ensure!(
            kind(&approver.node)
                == Some(BuiltinNode::Archived(ArchivedNode::ApproveImplementation))
                && approver.port == "decision",
            "write preparation must consume a pinned implementation approval"
        );
        ensure!(
            source(&provider.node, "task") == source(&approver.node, "task"),
            "write preparation and implementation approval must use the same work item"
        );
        if matches!(
            step.instance.kind,
            BuiltinNode::Archived(ArchivedNode::Implement) | BuiltinNode::Agent
        ) {
            ensure!(
                source(node, "plan") == source(&approver.node, "plan"),
                "writing node {node} must use the plan selected by its implementation approval"
            );
        }
    }
    Ok(())
}

pub(super) fn unit_nodes(definition: &WorkflowDefinition) -> HashSet<&str> {
    unit_nodes_for(&definition.nodes, &definition.wires)
}

pub(super) fn unit_nodes_for<'a>(nodes: &'a [PinnedNode], wires: &[Wire]) -> HashSet<&'a str> {
    let mut scoped: HashSet<_> = nodes
        .iter()
        .filter(|node| node.instance.kind.per_unit())
        .map(|node| node.instance.id.as_str())
        .collect();
    let control_ids: HashSet<_> = nodes
        .iter()
        .filter(|node| is_control(node.instance.kind))
        .map(|node| node.instance.id.as_str())
        .collect();
    loop {
        let len = scoped.len();
        for wire in wires {
            if scoped.contains(wire.from.node.as_str())
                && control_ids.contains(wire.to.node.as_str())
            {
                scoped.insert(
                    *control_ids
                        .get(wire.to.node.as_str())
                        .expect("control node"),
                );
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
    if definition.unsupported_node().is_some() {
        return Ok(());
    }
    let controls = uses_control(&definition);
    if !controls {
        return Ok(());
    }
    let mut state = Evaluation::new(db, &definition, &run)?;
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

pub(super) enum StageAvailability {
    Waiting,
    Skipped,
    Ready(super::NodeInputs),
}

pub(super) fn conversation_stage_availability(
    db: &Connection,
    definition: &WorkflowDefinition,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
) -> Result<StageAvailability> {
    let state = Evaluation::new(db, definition, run)?;
    match state.branch_gate(&node.instance.id, unit)? {
        Arrival::Ignored => return Ok(StageAvailability::Skipped),
        Arrival::Pending => return Ok(StageAvailability::Waiting),
        Arrival::Selected(_) => {}
    }
    let mut inputs = super::NodeInputs::new();
    let mut pending = false;
    for wire in definition
        .wires
        .iter()
        .filter(|wire| wire.to.node == node.instance.id)
    {
        match state.dependency(wire, unit)? {
            Arrival::Ignored => return Ok(StageAvailability::Skipped),
            Arrival::Pending => pending = true,
            Arrival::Selected(artifact) => {
                let data = node.definition.inputs.iter().any(|port| {
                    port.name == wire.to.port && matches!(port.value, PortType::Data(_))
                });
                if let (true, Some(artifact)) = (data, artifact) {
                    inputs.insert(wire.to.port.clone(), artifact.id);
                }
            }
        }
    }
    Ok(if pending {
        StageAvailability::Waiting
    } else {
        StageAvailability::Ready(inputs)
    })
}

pub(super) fn require_dependencies(
    db: &Connection,
    definition: &WorkflowDefinition,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    inputs: &super::NodeInputs,
) -> Result<bool> {
    definition.ensure_supported_nodes()?;
    if !uses_control(definition) {
        return Ok(false);
    }
    let state = Evaluation::new(db, definition, run)?;
    ensure!(
        matches!(state.branch_gate(node, unit)?, Arrival::Selected(_)),
        "The stage's Fork branch is not active"
    );
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
        .filter(|node| {
            node.instance.kind == BuiltinNode::Archived(ArchivedNode::ApproveImplementation)
        })
        .context("the selected stage does not approve implementation")?;
    state.approval_inputs(node)
}

struct Evaluation<'a> {
    db: &'a Connection,
    definition: &'a WorkflowDefinition,
    run: &'a WorkflowRun,
    unit_nodes: HashSet<&'a str>,
    nodes: HashMap<&'a str, &'a PinnedNode>,
    incoming: HashMap<&'a str, Vec<&'a Wire>>,
    receipts: HashMap<(String, Option<String>), ControlStageReceipt>,
    forks: Vec<super::orchestration::ForkScope<'a>>,
    memberships: HashMap<&'a str, Vec<(&'a str, &'a str)>>,
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
        let mut incoming = HashMap::<&str, Vec<&Wire>>::new();
        for wire in &definition.wires {
            incoming.entry(&wire.to.node).or_default().push(wire);
        }
        let forks = super::orchestration::fork_scopes(&definition.nodes, &definition.wires)?;
        let mut memberships = HashMap::<&str, Vec<(&str, &str)>>::new();
        for scope in &forks {
            for (&branch, nodes) in &scope.branches {
                for &node in nodes {
                    memberships.entry(node).or_default().push((scope.fork, branch));
                }
            }
        }
        Ok(Self {
            db,
            definition,
            run,
            unit_nodes: unit_nodes(definition),
            incoming,
            receipts,
            forks,
            memberships,
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
        // New receipts can unblock stages earlier in the published order.
        // Revisit downstream nodes regardless of their published order.
        loop {
            let before = self.receipts.len();
            let settled = self.settle_forks(unit)?;
            // An early Join may cancel queued branches as soon as the current batch settles.
            for node in &self.definition.nodes {
                if node.instance.kind == BuiltinNode::JoinResults
                    && self.unit_nodes.contains(node.instance.id.as_str()) == unit.is_some()
                    && let Some(receipt) = self.evaluate(node, unit)?
                {
                    self.save(receipt)?;
                }
            }
            let admitted = self.admit_forks(unit)?;
            for node in &self.definition.nodes {
                if self.unit_nodes.contains(node.instance.id.as_str()) == unit.is_some() {
                    if controls
                        && node.instance.kind != BuiltinNode::JoinResults
                        && let Some(receipt) = self.evaluate(node, unit)?
                    {
                        self.save(receipt)?;
                    }
                }
            }
            if !admitted && !settled && before == self.receipts.len() {
                return Ok(());
            }
        }
    }

    fn approval_inputs(&self, node: &PinnedNode) -> Result<ApprovalSelection> {
        match self.branch_gate(&node.instance.id, None)? {
            Arrival::Ignored => {
                return Ok(ApprovalSelection::Skipped {
                    reason: "The Fork branch was skipped or cancelled.".into(),
                });
            }
            Arrival::Pending => {
                return Ok(ApprovalSelection::Waiting {
                    reason: "Waiting for a Fork branch slot.".into(),
                });
            }
            Arrival::Selected(_) => {}
        }
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
        for &wire in self.incoming.get(node.instance.id.as_str()).into_iter().flatten() {
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
        match self.branch_gate(node_id, unit)? {
            Arrival::Pending => return Ok(None),
            Arrival::Ignored => {
                let cancelled = self.branch_cancelled(node_id, unit)?;
                return Ok(Some(ControlStageReceipt {
                    node_id: node_id.into(),
                    unit_id: unit.map(str::to_owned),
                    plan_revision: self.run.plan_revision.clone(),
                    state: ControlStageState::Skipped,
                    selected_ports: Vec::new(),
                    reason: Some(
                        if cancelled {
                            "The Join cancelled this Fork branch"
                        } else {
                            "No execution reached the Fork"
                        }
                        .into(),
                    ),
                    source_run_id: None,
                    dependencies: Vec::new(),
                    forwarded_outputs: Default::default(),
                }));
            }
            Arrival::Selected(_) => {}
        }
        let dependencies = self.incoming.get(node_id).into_iter().flatten()
            .map(|&wire| Ok((wire, self.dependency(wire, unit)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if matches!(
            node.instance.configuration,
            NodeConfiguration::Fork(_) | NodeConfiguration::JoinResults(_)
        ) || node.definition.configuration.is_loop_phase()
        {
            return Ok(self
                .evaluate_flow(node, unit, &dependencies)?
                .map(|outcome| ControlStageReceipt {
                    node_id: node_id.into(),
                    unit_id: unit.map(str::to_owned),
                    plan_revision: self.run.plan_revision.clone(),
                    state: outcome.state,
                    selected_ports: outcome.ports,
                    reason: outcome.reason,
                    source_run_id: None,
                    forwarded_outputs: Default::default(),
                    dependencies: dependencies
                        .iter()
                        .filter_map(|(wire, arrival)| {
                            if outcome.branches.as_ref().is_some_and(|branches| {
                                wire.to
                                    .port
                                    .split_once(':')
                                    .is_some_and(|(_, branch)| !branches.contains(branch))
                            }) {
                                return Some(ControlDependency {
                                    from: wire.from.clone(),
                                    to_port: wire.to.port.clone(),
                                    selected: false,
                                    artifact_id: None,
                                });
                            }
                            let (selected, artifact_id) = match arrival {
                                Arrival::Pending => return None,
                                Arrival::Ignored => (false, None),
                                Arrival::Selected(artifact) => {
                                    (true, artifact.as_ref().map(|value| value.id.clone()))
                                }
                            };
                            Some(ControlDependency {
                                from: wire.from.clone(),
                                to_port: wire.to.port.clone(),
                                selected,
                                artifact_id,
                            })
                        })
                        .collect(),
                }));
        }
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
        let ignored = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Ignored));
        let pending = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Pending));
        let selected = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Selected(_)));
        let joining_failures =
            matches!(node.instance.configuration, NodeConfiguration::JoinFailures);
        let (state, port, reason) = if ignored && (!joining_failures || !pending && !selected) {
            (
                ControlStageState::Skipped,
                None,
                Some("No execution reaches this stage through the selected branches"),
            )
        } else if pending {
            return Ok(None);
        } else {
            match node.instance.kind {
                End => {
                    let inputs = pinned_inputs();
                    if let Some(artifact) = inputs.get("result") {
                        let ArtifactValue::JsonObject(result) = &artifact.value else {
                            anyhow::bail!("End needs a structured result");
                        };
                        if let Some(schema) = node.definition.input_schemas.get("result")
                            && let Err(error) = super::schema::validate(
                                schema,
                                &serde_json::Value::Object(result.clone()),
                            )
                        {
                            diagnostic = Some(crate::diagnostics::preview(&error.to_string()));
                            (ControlStageState::Failed, None, None)
                        } else {
                            self.record_stage_artifact(
                                node_id,
                                unit,
                                "result",
                                artifact.value.clone(),
                                &inputs,
                            )?;
                            (ControlStageState::Completed, Some("completed"), None)
                        }
                    } else {
                        (ControlStageState::Completed, Some("completed"), None)
                    }
                }
                Agent if node.definition.executor == super::Executor::ConversationAgent => {
                    use super::conversation_turns::NativeOutcome;
                    match super::conversation_turns::native_outcome(self.db, self.run)? {
                        NativeOutcome::Waiting => return Ok(None),
                        NativeOutcome::Failed => (
                            ControlStageState::Failed,
                            Some("failed"),
                            Some("The conversation agent did not complete"),
                        ),
                        NativeOutcome::Completed(message) => {
                            let result = serde_json::Map::from_iter([(
                                "replyRevisionId".into(),
                                serde_json::Value::String(message.revision_id),
                            )]);
                            self.record_stage_artifact(
                                node_id,
                                unit,
                                "result",
                                ArtifactValue::JsonObject(result),
                                &pinned_inputs(),
                            )?;
                            (ControlStageState::Completed, Some("completed"), None)
                        }
                    }
                }
                Human => {
                    let original =
                        selected_artifact("input").and_then(|artifact| match &artifact.value {
                            ArtifactValue::JsonObject(value) => Some(value.clone()),
                            _ => None,
                        });
                    match super::decision_nodes::WorkflowDecisionRequest::for_node(node, original) {
                        Ok(request) => {
                            let Some(port) = super::decision_nodes::stage_port(
                                self.db,
                                self.run,
                                node,
                                unit,
                                &input_ids(),
                                request,
                            )?
                            else {
                                return Ok(None);
                            };
                            if port == "failed" {
                                (
                                    ControlStageState::Failed,
                                    Some(port),
                                    Some("The workflow decision expired"),
                                )
                            } else {
                                (ControlStageState::Completed, Some(port), None)
                            }
                        }
                        Err(error) => {
                            super::timed::cancel(self.db, self.run, node_id, unit)?;
                            diagnostic = Some(crate::diagnostics::preview(&error.to_string()));
                            (ControlStageState::Failed, Some("failed"), None)
                        }
                    }
                }
                Wait => {
                    let NodeConfiguration::Wait(config) = &node.instance.configuration else {
                        anyhow::bail!("Missing pinned Wait configuration");
                    };
                    if !super::timed::wait(self.db, self.run, node_id, unit, config)? {
                        return Ok(None);
                    }
                    if let Some(input) = selected_artifact("input") {
                        self.record_stage_artifact(
                            node_id,
                            unit,
                            "result",
                            input.value.clone(),
                            &pinned_inputs(),
                        )?;
                    }
                    (ControlStageState::Completed, Some("completed"), None)
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
                    (outcome.state, Some(outcome.port), None)
                }
                JoinResults
                    if matches!(node.instance.configuration, NodeConfiguration::JoinFailures) =>
                {
                    (ControlStageState::Completed, Some("joined"), None)
                }
                JoinResults
                    if matches!(
                        node.instance.configuration,
                        NodeConfiguration::LegacyReviewVerdict
                    ) =>
                {
                    let approved = selected_artifact("value").is_some_and(|artifact| {
                        matches!(&artifact.value, ArtifactValue::JsonObject(value)
                            if value.get("approved") == Some(&serde_json::Value::Bool(true)))
                    });
                    (
                        ControlStageState::Completed,
                        Some(if approved { "matched" } else { "unmatched" }),
                        None,
                    )
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
            source_run_id: source_run,
            forwarded_outputs: Default::default(),
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
        if matches!(
            source.instance.configuration,
            NodeConfiguration::LoopGate(_)
        ) && wire.from.port == "context"
            && receipt
                .is_some_and(|value| !value.selected_ports.iter().any(|port| port == "iterate"))
        {
            return Ok(Arrival::Ignored);
        }
        if source.instance.kind == BuiltinNode::Fork {
            let branch = super::orchestration::port_branch(&wire.from.port)
                .context("A Fork output lost its branch")?;
            use super::orchestration::BranchState;
            match super::orchestration::branch_state(
                self.db,
                self.run,
                source_id,
                source_unit,
                branch,
            )? {
                Some((BranchState::Queued, _)) => return Ok(Arrival::Pending),
                Some((BranchState::Cancelled, _)) => return Ok(Arrival::Ignored),
                None if receipt.is_some_and(|value| value.state == ControlStageState::Skipped) => {
                    return Ok(Arrival::Ignored);
                }
                None => return Ok(Arrival::Pending),
                _ => {}
            }
        }
        if receipt.is_some_and(|receipt| {
            receipt.state == ControlStageState::Skipped
                || receipt.state == ControlStageState::Failed
                    && port.value != PortType::Control
                    && !(matches!(
                        source.instance.configuration,
                        NodeConfiguration::JoinResults(_) | NodeConfiguration::LoopResult(_)
                    ) && wire.from.port == "results")
                    && !(source.instance.kind == BuiltinNode::Human && port.name == "response")
        }) {
            return Ok(Arrival::Ignored);
        }
        if port.value == PortType::Control && is_control(source.instance.kind) {
            return Ok(match receipt {
                Some(receipt) if receipt.selected_ports.contains(&wire.from.port) => {
                    Arrival::Selected(None)
                }
                Some(_) => Arrival::Ignored,
                None => Arrival::Pending,
            });
        }
        let artifact =
            self.source_artifact(&wire.from, source_unit, port.value == PortType::Control)?;
        if artifact.is_none()
            && receipt.is_some_and(|receipt| {
                receipt.state == ControlStageState::Failed
                    || matches!(source.instance.kind, BuiltinNode::Wait | BuiltinNode::Human)
            })
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
        let mut statement = self.db.prepare_cached(
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
        let pinned_output = self.nodes.get(source.node.as_str()).is_some_and(|node| {
            matches!(
                node.instance.kind,
                BuiltinNode::Archived(ArchivedNode::PromptDecision)
                    | BuiltinNode::Archived(ArchivedNode::PromptOutcome)
                    | BuiltinNode::Fork
                    | BuiltinNode::JoinResults
                    | BuiltinNode::Loop
            )
        });
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
            if pinned_output && !exact_receipt {
                continue;
            }
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
}
