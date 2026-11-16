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

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConditionPredicate {
    Exists { pointer: String },
    Equals { pointer: String, value: Value },
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
            }
    }

    fn pointer(&self) -> &str {
        match self {
            Self::Exists { pointer } | Self::Equals { pointer, .. } => pointer,
        }
    }

    pub(crate) fn test(&self, value: &Value) -> bool {
        let found = value.pointer(self.pointer());
        match self {
            Self::Exists { .. } => found.is_some(),
            Self::Equals { value, .. } => found == Some(value),
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
        BuiltinNode::Condition | BuiltinNode::Parallel | BuiltinNode::Join | BuiltinNode::IterationLimit
    )
}

pub(super) fn uses_control(definition: &WorkflowDefinition) -> bool {
    definition
        .nodes
        .iter()
        .any(|node| is_control(node.instance.kind) || node.instance.kind == BuiltinNode::McpTool)
        || definition
            .wires
            .iter()
            .any(|wire| wire.from.port == "failed")
}

pub(super) fn validate_release_wires(nodes: &[PinnedNode], wires: &[Wire]) -> Result<()> {
    let source = |node: &str, port: &str| {
        wires
            .iter()
            .find(|wire| wire.to.node == node && wire.to.port == port)
            .map(|wire| wire.from.node.as_str())
    };
    for step in nodes.iter().filter(|node| {
        matches!(
            node.instance.kind,
            BuiltinNode::Implement | BuiltinNode::Command
        )
    }) {
        let node = step.instance.id.as_str();
        let provider = source(node, "release").context("writing node requires a pinned release")?;
        ensure!(
            Some(provider) == source(node, "worktree"),
            "writing node {node} must use release and worktree from the same preparation"
        );
        let approver = source(provider, "decision")
            .context("write preparation must consume a pinned implementation approval")?;
        ensure!(
            source(provider, "task") == source(approver, "task"),
            "write preparation and implementation approval must use the same work item"
        );
        if step.instance.kind == BuiltinNode::Implement {
            ensure!(
                source(node, "plan") == source(approver, "plan"),
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
    if !uses_control(&definition) {
        return Ok(());
    }
    let unit_ids = db.prepare(
        "SELECT unit_id FROM workflow_delivery_units WHERE project_id = ?1 AND pipeline_run_id = ?2 AND plan_revision = ?3 ORDER BY ordinal",
    )?.query_map(params![project, pipeline, run.plan_revision], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut state = Evaluation::new(db, &definition, &run)?;
    for unit in std::iter::once(None).chain(unit_ids.iter().map(|id| Some(id.as_str()))) {
        state.settle(unit)?;
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
            "workflow command already selected an outcome"
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
    }
    Ok(())
}

enum Arrival {
    Selected(Option<WorkflowArtifact>),
    Ignored,
    Pending,
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

    fn settle(&mut self, unit: Option<&str>) -> Result<()> {
        // Published node order is independent of topological order. A pass
        // only adds immutable receipts, so at most N passes can add work.
        loop {
            let before = self.receipts.len();
            for node in &self.definition.nodes {
                if self.unit_nodes.contains(node.instance.id.as_str()) == unit.is_some() {
                    if let Some(receipt) = self.evaluate(node, unit)? {
                        self.save(receipt)?;
                    }
                }
            }
            if before == self.receipts.len() {
                return Ok(());
            }
        }
    }

    fn evaluate(
        &self,
        node: &PinnedNode,
        unit: Option<&str>,
    ) -> Result<Option<ControlStageReceipt>> {
        use BuiltinNode::*;
        let node_id = node.instance.id.as_str();
        if self.receipt(node_id, unit).is_some() || node.instance.kind == Start {
            return Ok(None);
        }
        let dependencies = self
            .definition
            .wires
            .iter()
            .filter(|wire| wire.to.node == node_id)
            .map(|wire| Ok((wire, self.dependency(wire, unit)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let ignored = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Ignored));
        let pending = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Pending));
        let selected = dependencies
            .iter()
            .any(|(_, outcome)| matches!(outcome, Arrival::Selected(_)));
        let (state, port, reason, attempt) =
            if ignored && (node.instance.kind != Join || !pending && !selected) {
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
                    Condition => {
                        let NodeConfiguration::Condition { predicate } =
                            &node.instance.configuration
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
                        let value = serde_json::to_value(&artifact.value)?;
                        let port = if predicate.test(&value) {
                            "matched"
                        } else {
                            "unmatched"
                        };
                        (ControlStageState::Completed, Some(port), None, None)
                    }
                    Parallel => (ControlStageState::Completed, Some("branches"), None, None),
                    Join => (ControlStageState::Completed, Some("joined"), None, None),
                    IterationLimit => (ControlStageState::Failed, Some("failed"), Some("Reached the configured iteration limit"), None),
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
                            wire.from.node == node_id && wire.from.port == "failed"
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
                        let Some((id, state, _)) = self.mcp_attempt(node_id)? else { return Ok(None) };
                        match state.as_str() {
                            "confirmed" => (ControlStageState::Completed, Some("completed"), None, Some(id)),
                            "rejected" | "not_sent" => (ControlStageState::Failed, Some("failed"),
                                Some("The local MCP tool did not complete"), Some(id)),
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
            reason: reason.map(str::to_owned),
            source_run_id: attempt,
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
        if receipt.is_some_and(|receipt| receipt.state == ControlStageState::Skipped) {
            return Ok(Arrival::Ignored);
        }
        if port.value == PortType::Control
            && (is_control(source.instance.kind)
                || source.instance.kind == BuiltinNode::McpTool
                || source.instance.kind == BuiltinNode::Command && receipt.is_some())
        {
            return Ok(match receipt {
                Some(receipt) if receipt.selected_ports.contains(&wire.from.port) => {
                    Arrival::Selected(None)
                }
                Some(_) => Arrival::Ignored,
                None => Arrival::Pending,
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
            if receipt.is_some_and(|record| record.state == ControlStageState::Failed) { return Ok(Arrival::Ignored); }
            let artifact = match self.mcp_attempt(source_id)? {
                Some((_, state, Some(id))) if state == "confirmed" => super::mcp_tools::load_mcp_artifact(self.db, &self.run.project_id, &self.run.id, &id)?,
                _ => None,
            };
            return Ok(artifact.map_or(Arrival::Pending, |value| Arrival::Selected(Some(value))));
        }
        let artifact =
            self.source_artifact(&wire.from, source_unit, port.value == PortType::Control)?;
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
                   AND i.execution_key = ?5)
             FROM workflow_artifacts a LEFT JOIN workflow_host_completions h ON h.artifact_id = a.id
             WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.unit_id IS ?3
               AND (?4 IS NULL OR a.port_name = ?4)
             ORDER BY h.execution_key = ?5 DESC, a.port_name, a.id",
        )?;
        let key = self.run.plan_revision.as_deref().unwrap_or("");
        let mut rows = statement.query(params![
            self.run.id,
            source.node,
            unit,
            (!control).then_some(&source.port),
            key
        ])?;
        let mut selected = None;
        while let Some(row) = rows.next()? {
            let payload: Vec<u8> = row.get(0)?;
            let host_key: Option<String> = row.get(1)?;
            let receipt: bool = row.get(2)?;
            let artifact: WorkflowArtifact =
                serde_json::from_slice(&protection::unprotect(&payload)?)?;
            let valid = match &artifact.value {
                ArtifactValue::Plan { .. } => {
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
                        || receipt
                        || unit.is_some() && self.run.plan_revision.is_some()
                }
            };
            if valid {
                if control || host_key.as_deref() == Some(key) {
                    return Ok(Some(artifact));
                }
                if selected.replace(artifact).is_some() {
                    return Ok(None);
                }
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
