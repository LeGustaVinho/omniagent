use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{
    ArtifactKind, BuiltinNode, ConditionPredicate, NodeConfiguration, NodeDefinition, NodeInstance,
    PinnedNode, Port, PortType, Wire, WorkflowRun,
};

mod loop_body;
pub(super) use loop_body::expand_loop;

const MAX_BRANCHES: usize = 32;
const OBJECT: PortType = PortType::Data(ArtifactKind::JsonObject);
const CONTROL: PortType = PortType::Control;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkConfiguration {
    pub branches: Vec<String>,
    #[serde(default)]
    pub concurrency: Option<u16>,
}

impl ForkConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_branches(&self.branches)
            && self
                .concurrency
                .is_none_or(|value| value > 0 && usize::from(value) <= self.branches.len())
    }

    pub(super) fn limit(&self) -> usize {
        self.concurrency.map_or(self.branches.len(), usize::from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinStrategy {
    All,
    Any,
    Quorum,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JoinResultsConfiguration {
    pub branches: Vec<String>,
    pub strategy: JoinStrategy,
    #[serde(default)]
    pub quorum: Option<u16>,
    #[serde(default)]
    pub count_failures: bool,
    #[serde(default)]
    pub cancel_remaining: bool,
    #[serde(default)]
    pub fork: Option<String>,
}

impl JoinResultsConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_branches(&self.branches)
            && (self.strategy != JoinStrategy::Quorum
                || self
                    .quorum
                    .is_some_and(|quorum| quorum > 0 && usize::from(quorum) <= self.branches.len()))
            && (!self.cancel_remaining || self.fork.is_some())
            && self.fork.as_ref().is_none_or(|fork| !fork.is_empty())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopLimit {
    Complete,
    LimitReached,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopConfiguration {
    pub max_iterations: u16,
    #[serde(default)]
    pub break_when: Option<ConditionPredicate>,
    #[serde(default)]
    pub exit_when: Option<ConditionPredicate>,
    pub limit: LoopLimit,
    pub ready_input: String,
    pub context_input: String,
    pub completion_output: String,
    pub result_output: String,
    #[serde(default)]
    pub failure_output: Option<String>,
}

impl LoopConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_round(self.max_iterations)
            && [
                &self.ready_input,
                &self.context_input,
                &self.completion_output,
                &self.result_output,
            ]
            .into_iter()
            .all(|name| !name.trim().is_empty())
            && self.ready_input != self.context_input
            && self.completion_output != self.result_output
            && self.failure_output.as_ref().is_none_or(|name| {
                !name.trim().is_empty()
                    && name != &self.completion_output
                    && name != &self.result_output
            })
            && valid_predicate(&self.break_when)
            && valid_predicate(&self.exit_when)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopGateConfiguration {
    pub index: u16,
    pub max_iterations: u16,
    pub break_when: Option<ConditionPredicate>,
}

impl LoopGateConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_round(self.index)
            && valid_round(self.max_iterations)
            && self.index <= self.max_iterations
            && valid_predicate(&self.break_when)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopStepConfiguration {
    pub index: u16,
    pub max_iterations: u16,
    pub exit_when: Option<ConditionPredicate>,
    pub failure_input: bool,
}

impl LoopStepConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_round(self.index)
            && valid_round(self.max_iterations)
            && self.index <= self.max_iterations
            && valid_predicate(&self.exit_when)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoopResultConfiguration {
    pub rounds: u16,
    pub limit: LoopLimit,
}

impl LoopResultConfiguration {
    pub(super) fn valid(&self) -> bool {
        valid_round(self.rounds)
    }
}

fn valid_round(round: u16) -> bool {
    (1..=32).contains(&round)
}

fn valid_predicate(predicate: &Option<ConditionPredicate>) -> bool {
    predicate.as_ref().is_none_or(|predicate| {
        !matches!(predicate, ConditionPredicate::CandidateChecksPassed) && predicate.valid()
    })
}

fn valid_branches(branches: &[String]) -> bool {
    (2..=MAX_BRANCHES).contains(&branches.len())
        && branches.iter().collect::<BTreeSet<_>>().len() == branches.len()
        && branches.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 48
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

pub(super) fn branch_port(name: &str) -> String {
    format!("branch:{name}")
}

pub(super) fn context_port(name: &str) -> String {
    format!("context:{name}")
}

pub(super) fn result_port(name: &str) -> String {
    format!("result:{name}")
}

pub(super) fn port_branch(port: &str) -> Option<&str> {
    port.strip_prefix("branch:")
        .or_else(|| port.strip_prefix("context:"))
}

fn port(name: impl Into<String>, value: PortType) -> Port {
    Port {
        name: name.into(),
        value,
    }
}

fn ports(values: &[(&str, PortType)]) -> Vec<Port> {
    values
        .iter()
        .map(|(name, value)| port(*name, *value))
        .collect()
}

pub(super) fn define(node: &NodeInstance, definition: &mut NodeDefinition) -> Result<()> {
    use super::ConfigurationKind;
    use NodeConfiguration as Config;
    let phase = match &node.configuration {
        Config::LoopGate(_) => Some(ConfigurationKind::LoopGate),
        Config::LoopStep(_) => Some(ConfigurationKind::LoopStep),
        Config::LoopResult(_) => Some(ConfigurationKind::LoopResult),
        _ => None,
    };
    if node.kind == BuiltinNode::Loop
        && let Some(kind) = phase
        && let Some((executor, title)) = super::registry::loop_phase_definition(kind)
    {
        definition.configuration = kind;
        definition.executor = executor;
        definition.title = title.into();
    }
    let (inputs, outputs) = match &node.configuration {
        Config::Fork(config) => {
            ensure!(
                config.valid(),
                "Fork needs distinct branch names and a valid concurrency limit"
            );
            if let Some(schema) = definition.input_schemas.get("input") {
                definition.output_schemas.extend(
                    config
                        .branches
                        .iter()
                        .map(|branch| (context_port(branch), schema.clone())),
                );
            }
            (
                ports(&[("ready", CONTROL), ("input", OBJECT)]),
                config
                    .branches
                    .iter()
                    .flat_map(|name| {
                        [
                            port(branch_port(name), CONTROL),
                            port(context_port(name), OBJECT),
                        ]
                    })
                    .collect(),
            )
        }
        Config::JoinResults(config) => {
            ensure!(
                config.valid(),
                "Join needs distinct branch names, a valid quorum, and a Fork to cancel branches"
            );
            (
                config
                    .branches
                    .iter()
                    .flat_map(|name| {
                        [
                            port(branch_port(name), CONTROL),
                            port(result_port(name), OBJECT),
                        ]
                    })
                    .collect(),
                ports(&[
                    ("joined", CONTROL),
                    ("failed", CONTROL),
                    ("results", OBJECT),
                ]),
            )
        }
        Config::Loop(config) => {
            ensure!(
                config.valid(),
                "Use a Loop bound from 1 through 32 and valid JSON conditions"
            );
            (
                ports(&[("ready", CONTROL), ("input", OBJECT)]),
                loop_outputs(),
            )
        }
        Config::LoopGate(config) => {
            ensure!(config.valid(), "Invalid generated Loop gate");
            let mut inputs = ports(&[("ready", CONTROL), ("input", OBJECT)]);
            if config.index > 1 {
                inputs.push(port("previous", OBJECT));
            }
            (
                inputs,
                ports(&[
                    ("iterate", CONTROL),
                    ("break", CONTROL),
                    ("context", OBJECT),
                ]),
            )
        }
        Config::LoopStep(config) => {
            ensure!(config.valid(), "Invalid generated Loop iteration");
            let mut inputs = ports(&[
                ("started", CONTROL),
                ("completed", CONTROL),
                ("value", OBJECT),
                ("context", OBJECT),
            ]);
            if config.failure_input {
                inputs.push(port("failed", CONTROL));
            }
            (
                inputs,
                ports(&[
                    ("next", CONTROL),
                    ("exit", CONTROL),
                    ("limit", CONTROL),
                    ("failed", CONTROL),
                    ("result", OBJECT),
                ]),
            )
        }
        Config::LoopResult(config) => {
            ensure!(config.valid(), "Invalid generated Loop result");
            (
                (1..=config.rounds)
                    .flat_map(|index| {
                        [
                            port(format!("break:{index}"), CONTROL),
                            port(format!("exit:{index}"), CONTROL),
                            port(format!("limit:{index}"), CONTROL),
                            port(format!("failed:{index}"), CONTROL),
                            port(format!("result:{index}"), OBJECT),
                        ]
                    })
                    .collect(),
                loop_outputs(),
            )
        }
        _ => return Ok(()),
    };
    definition.inputs = inputs;
    definition.outputs = outputs;
    Ok(())
}

fn loop_outputs() -> Vec<Port> {
    ports(&[
        ("completed", CONTROL),
        ("limit_reached", CONTROL),
        ("failed", CONTROL),
        ("results", OBJECT),
    ])
}

pub(super) fn optional_input(node: &PinnedNode, name: &str) -> bool {
    matches!(
        node.instance.configuration,
        NodeConfiguration::JoinResults(_)
    ) && name.starts_with("result:")
}

pub(super) struct ForkScope<'a> {
    pub fork: &'a str,
    pub config: &'a ForkConfiguration,
    pub join: Option<&'a str>,
    pub branches: BTreeMap<&'a str, BTreeSet<&'a str>>,
}

pub(super) fn validate_graph(nodes: &[PinnedNode], wires: &[Wire]) -> Result<()> {
    let scopes = fork_scopes(nodes, wires)?;
    if !scopes.is_empty() {
        let unit_nodes = super::control::unit_nodes_for(nodes, wires);
        for scope in &scopes {
            let unit_scoped = unit_nodes.contains(scope.fork);
            ensure!(
                scope
                    .branches
                    .values()
                    .flatten()
                    .all(|node| unit_nodes.contains(node) == unit_scoped)
                    && scope
                        .join
                        .is_none_or(|join| unit_nodes.contains(join) == unit_scoped),
                "Fork {} and its branches must use the same delivery unit scope",
                scope.fork,
            );
        }
    }
    Ok(())
}

pub(super) fn fork_scopes<'a>(
    nodes: &'a [PinnedNode],
    wires: &'a [Wire],
) -> Result<Vec<ForkScope<'a>>> {
    let mut joins = HashMap::new();
    for node in nodes {
        if let NodeConfiguration::JoinResults(config) = &node.instance.configuration
            && let Some(fork) = &config.fork
        {
            ensure!(
                joins.insert(fork.as_str(), (node, config)).is_none(),
                "A Fork can have one owning Join"
            );
        }
    }
    let mut outgoing: HashMap<&str, Vec<&Wire>> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&Wire>> = HashMap::new();
    for wire in wires {
        outgoing.entry(&wire.from.node).or_default().push(wire);
        incoming.entry(&wire.to.node).or_default().push(wire);
    }
    let mut scopes = Vec::new();
    for node in nodes {
        let NodeConfiguration::Fork(config) = &node.instance.configuration else {
            continue;
        };
        let fork = node.instance.id.as_str();
        let join = joins.remove(fork);
        let stop = join.map(|(node, _)| node.instance.id.as_str());
        if let Some((_, config_join)) = join {
            ensure!(
                config.branches.iter().collect::<BTreeSet<_>>()
                    == config_join.branches.iter().collect(),
                "A paired Join must use each named Fork branch"
            );
        }
        let mut downstream = BTreeSet::new();
        let mut pending_downstream: Vec<_> = stop.into_iter().collect();
        while let Some(target) = pending_downstream.pop() {
            if downstream.insert(target) {
                pending_downstream.extend(
                    outgoing
                        .get(target)
                        .into_iter()
                        .flatten()
                        .map(|wire| wire.to.node.as_str()),
                );
            }
        }
        let mut branches = BTreeMap::new();
        let mut owned = BTreeSet::new();
        for branch in &config.branches {
            let mut pending: Vec<_> = outgoing
                .get(fork)
                .into_iter()
                .flatten()
                .filter_map(|wire| {
                    (port_branch(&wire.from.port) == Some(branch)).then_some(wire.to.node.as_str())
                })
                .collect();
            let mut branch_nodes = BTreeSet::new();
            while let Some(target) = pending.pop() {
                if downstream.contains(target) || !branch_nodes.insert(target) {
                    continue;
                }
                pending.extend(
                    outgoing
                        .get(target)
                        .into_iter()
                        .flatten()
                        .map(|wire| wire.to.node.as_str()),
                );
            }
            ensure!(
                !branch_nodes.is_empty(),
                "Connect branch {branch} on Fork {fork} to a stage"
            );
            ensure!(
                branch_nodes.is_disjoint(&owned),
                "Branches of Fork {fork} can only meet at its paired Join"
            );
            if let Some(join_id) = stop {
                let incoming = incoming.get(join_id).map(Vec::as_slice).unwrap_or_default();
                let completion = branch_port(branch);
                let result = result_port(branch);
                let count = incoming
                    .iter()
                    .filter(|wire| {
                        wire.to.port == completion
                            && branch_nodes.contains(wire.from.node.as_str())
                    })
                    .count();
                ensure!(
                    count == 1,
                    "Connect the completion of Fork branch {branch} to its paired Join port"
                );
                for wire in incoming.iter().filter(|wire| wire.to.port == result) {
                    ensure!(
                        branch_nodes.contains(wire.from.node.as_str()),
                        "Connect the result from the corresponding Fork branch {branch}"
                    );
                }
            }
            owned.extend(&branch_nodes);
            branches.insert(branch.as_str(), branch_nodes);
        }
        scopes.push(ForkScope {
            fork,
            config,
            join: stop,
            branches,
        });
    }
    ensure!(
        joins.is_empty(),
        "Select a Fork in the same workflow for this Join"
    );
    Ok(scopes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchState {
    Queued,
    Running,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl BranchState {
    pub(super) fn settled(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(state: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Running,
            Self::Completed,
            Self::Failed,
            Self::Skipped,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|value| value.key() == state)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkBranchStatus {
    pub fork_id: String,
    pub branch: String,
    pub unit_id: Option<String>,
    pub state: BranchState,
    pub settled_order: Option<i64>,
}

pub(super) fn branch_state(
    db: &Connection,
    run: &WorkflowRun,
    fork: &str,
    unit: Option<&str>,
    branch: &str,
) -> Result<Option<(BranchState, Option<i64>)>> {
    let row = db.prepare_cached(
        "SELECT state, settled_order FROM workflow_fork_branches
         WHERE pipeline_run_id = ?1 AND fork_id = ?2 AND plan_key = ?3 AND unit_key = ?4 AND branch = ?5",
    )?.query_row(
        params![run.id, fork, run.plan_revision.as_deref().unwrap_or(""), unit.unwrap_or(""), branch],
        |row| Ok((row.get::<_, String>(0)?, row.get(1)?)),
    ).optional()?;
    row.map(|(key, order)| {
        Ok((
            BranchState::parse(&key).context("Unknown Fork branch state")?,
            order,
        ))
    })
    .transpose()
}

pub(super) fn start_branches(
    db: &Connection,
    run: &WorkflowRun,
    fork: &str,
    unit: Option<&str>,
    names: &[String],
) -> Result<bool> {
    let mut changed = false;
    for name in names {
        changed |= db.execute(
            "INSERT INTO workflow_fork_branches VALUES (?1, ?2, ?3, ?4, ?5, 'queued', NULL)
             ON CONFLICT DO NOTHING",
            params![
                run.id,
                fork,
                run.plan_revision.as_deref().unwrap_or(""),
                unit.unwrap_or(""),
                name
            ],
        )? != 0;
    }
    Ok(changed)
}

pub(super) fn update_branch(
    db: &Connection,
    run: &WorkflowRun,
    fork: &str,
    unit: Option<&str>,
    branch: &str,
    previous: BranchState,
    next: BranchState,
) -> Result<bool> {
    Ok(db.execute(
        "UPDATE workflow_fork_branches SET state = ?6, settled_order = CASE WHEN ?7 THEN
             (SELECT coalesce(max(settled_order), 0) + 1 FROM workflow_fork_branches WHERE pipeline_run_id = ?1) ELSE NULL END
         WHERE pipeline_run_id = ?1 AND fork_id = ?2 AND plan_key = ?3 AND unit_key = ?4 AND branch = ?5 AND state = ?8",
        params![run.id, fork, run.plan_revision.as_deref().unwrap_or(""), unit.unwrap_or(""), branch,
            next.key(), next.settled(), previous.key()],
    )? != 0)
}

pub(super) fn branch_statuses(db: &Connection, run: &WorkflowRun) -> Result<Vec<ForkBranchStatus>> {
    db.prepare(
        "SELECT fork_id, branch, unit_key, state, settled_order FROM workflow_fork_branches
        WHERE pipeline_run_id = ?1 AND plan_key = ?2 ORDER BY fork_id, unit_key, branch",
    )?
    .query_map(
        params![run.id, run.plan_revision.as_deref().unwrap_or("")],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get(4)?,
            ))
        },
    )?
    .map(|row| {
        let (fork_id, branch, unit, key, settled_order) = row?;
        Ok(ForkBranchStatus {
            fork_id,
            branch,
            unit_id: (!unit.is_empty()).then_some(unit),
            state: BranchState::parse(&key).context("Unknown Fork branch state")?,
            settled_order,
        })
    })
    .collect()
}
