use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Map, Value, json};

use super::{Arrival, ControlStageState, Evaluation};
use crate::workflows::{
    ArtifactValue, Endpoint, NodeConfiguration, PinnedNode, Wire, WorkflowArtifact,
    orchestration::{
        self as flow, BranchState, ForkScope, JoinResultsConfiguration, JoinStrategy,
        LoopGateConfiguration, LoopLimit, LoopResultConfiguration, LoopStepConfiguration,
    },
};

#[cfg(test)]
mod tests;

type Dependencies<'a> = [(&'a Wire, Arrival)];

pub(super) struct FlowOutcome {
    pub state: ControlStageState,
    pub ports: Vec<String>,
    pub reason: Option<String>,
    pub branches: Option<BTreeSet<String>>,
}

impl FlowOutcome {
    fn completed(port: &str) -> Self {
        Self {
            state: ControlStageState::Completed,
            ports: vec![port.into()],
            reason: None,
            branches: None,
        }
    }

    fn failed(message: &str) -> Self {
        Self {
            state: ControlStageState::Failed,
            ports: vec!["failed".into()],
            reason: Some(message.into()),
            branches: None,
        }
    }

    fn skipped() -> Self {
        Self {
            state: ControlStageState::Skipped,
            ports: Vec::new(),
            reason: None,
            branches: None,
        }
    }
}

impl Evaluation<'_> {
    pub(super) fn settle_forks(&self, unit: Option<&str>) -> Result<bool> {
        let mut changed = false;
        for scope in &self.forks {
            if self.unit_nodes.contains(scope.fork) != unit.is_some()
                || !self
                    .receipt(scope.fork, unit)
                    .is_some_and(|receipt| receipt.state == ControlStageState::Completed)
                || self.branch_cancelled(scope.fork, unit)?
            {
                continue;
            }
            changed |=
                flow::start_branches(self.db, self.run, scope.fork, unit, &scope.config.branches)?;
            for branch in &scope.config.branches {
                let (state, _) = flow::branch_state(self.db, self.run, scope.fork, unit, branch)?
                    .context("The Fork lost a branch state")?;
                if state == BranchState::Running
                    && let Some(outcome) = self.settled_branch(scope, branch, unit)?
                {
                    changed |= flow::update_branch(
                        self.db, self.run, scope.fork, unit, branch, state, outcome,
                    )?;
                }
            }
        }
        Ok(changed)
    }

    pub(super) fn admit_forks(&self, unit: Option<&str>) -> Result<bool> {
        let mut changed = false;
        for scope in &self.forks {
            if self.unit_nodes.contains(scope.fork) != unit.is_some()
                || !self.receipt(scope.fork, unit).is_some_and(|receipt| receipt.state == ControlStageState::Completed)
                || self.branch_cancelled(scope.fork, unit)?
            {
                continue;
            }
            let mut queued = Vec::new();
            let mut active = 0usize;
            for branch in &scope.config.branches {
                match flow::branch_state(self.db, self.run, scope.fork, unit, branch)? {
                    Some((BranchState::Queued, _)) => queued.push(branch),
                    Some((BranchState::Running, _)) => active += 1,
                    _ => {}
                }
            }
            for branch in queued
                .into_iter()
                .take(scope.config.limit().saturating_sub(active))
            {
                changed |= flow::update_branch(
                    self.db,
                    self.run,
                    scope.fork,
                    unit,
                    branch,
                    BranchState::Queued,
                    BranchState::Running,
                )?;
            }
        }
        Ok(changed)
    }

    fn settled_branch(
        &self,
        scope: &ForkScope<'_>,
        branch: &str,
        unit: Option<&str>,
    ) -> Result<Option<BranchState>> {
        let nodes = &scope.branches[branch];
        let mut failed = false;
        let mut visited = false;
        for &id in nodes {
            match self.branch_stage_state(self.nodes[id], unit)? {
                BranchState::Failed => failed = true,
                BranchState::Completed => visited = true,
                BranchState::Skipped | BranchState::Cancelled => {}
                BranchState::Queued | BranchState::Running => return Ok(None),
            }
        }
        if let Some(join) = scope.join {
            let port = flow::branch_port(branch);
            let completion = self
                .incoming
                .get(join)
                .into_iter()
                .flatten()
                .copied()
                .find(|wire| wire.to.port == port)
                .context("A Fork branch lost its Join completion path")?;
            let outcome = self.observe_branch_completion(
                completion,
                &self.dependency(completion, unit)?,
                unit,
            );
            if outcome == BranchState::Completed {
                let result_port = flow::result_port(branch);
                if let Some(wire) = self
                    .incoming
                    .get(join)
                    .into_iter()
                    .flatten()
                    .copied()
                    .find(|wire| wire.to.port == result_port)
                {
                    match self.dependency(wire, unit)? {
                        Arrival::Selected(Some(_)) => {}
                        Arrival::Pending if !self.result_source_settled(wire, unit) => {
                            return Ok(None);
                        }
                        _ => return Ok(Some(BranchState::Failed)),
                    }
                }
            }
            return Ok(Some(match outcome {
                BranchState::Completed => BranchState::Completed,
                BranchState::Failed => BranchState::Failed,
                _ if failed => BranchState::Failed,
                _ => BranchState::Skipped,
            }));
        }
        Ok(Some(if failed {
            BranchState::Failed
        } else if visited {
            BranchState::Completed
        } else {
            BranchState::Skipped
        }))
    }

    fn branch_stage_state(&self, node: &PinnedNode, unit: Option<&str>) -> Result<BranchState> {
        let id = node.instance.id.as_str();
        let unit = self.unit_nodes.contains(id).then_some(unit).flatten();
        if let Some(receipt) = self.receipt(id, unit) {
            return Ok(match receipt.state {
                ControlStageState::Completed => BranchState::Completed,
                ControlStageState::Failed => BranchState::Failed,
                ControlStageState::Skipped => BranchState::Skipped,
            });
        }
        // Older host and provider stages publish a pinned artifact instead of a control receipt.
        for port in &node.definition.outputs {
            let wire = Wire {
                from: Endpoint {
                    node: id.into(),
                    port: port.name.clone(),
                },
                to: Endpoint {
                    node: String::new(),
                    port: String::new(),
                },
            };
            if matches!(self.dependency(&wire, unit)?, Arrival::Selected(_)) {
                return Ok(if port.name == "failed" {
                    BranchState::Failed
                } else {
                    BranchState::Completed
                });
            }
        }
        Ok(BranchState::Running)
    }

    pub(super) fn branch_gate(&self, node: &str, unit: Option<&str>) -> Result<Arrival> {
        let mut waiting = false;
        for &(fork, branch) in self.memberships.get(node).into_iter().flatten() {
            let scope_unit = self.unit_nodes.contains(fork).then_some(unit).flatten();
            let Some(receipt) = self.receipt(fork, scope_unit) else {
                waiting = true;
                continue;
            };
            match flow::branch_state(self.db, self.run, fork, scope_unit, branch)? {
                Some((BranchState::Cancelled, _)) => return Ok(Arrival::Ignored),
                Some((BranchState::Queued, _)) => waiting = true,
                None if receipt.state == ControlStageState::Skipped => {
                    return Ok(Arrival::Ignored);
                }
                None => waiting = true,
                _ => {}
            }
        }
        Ok(if waiting {
            Arrival::Pending
        } else {
            Arrival::Selected(None)
        })
    }

    pub(super) fn branch_cancelled(&self, node: &str, unit: Option<&str>) -> Result<bool> {
        for &(fork, branch) in self.memberships.get(node).into_iter().flatten() {
            let scope_unit = self.unit_nodes.contains(fork).then_some(unit).flatten();
            if self.receipt(fork, scope_unit).is_some()
                && flow::branch_state(self.db, self.run, fork, scope_unit, branch)?
                    .is_some_and(|(state, _)| state == BranchState::Cancelled)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn cancel_remaining(
        &self,
        fork: &str,
        unit: Option<&str>,
        selected: &BTreeSet<&str>,
    ) -> Result<()> {
        let scope = self
            .forks
            .iter()
            .find(|scope| scope.fork == fork)
            .context("The Join lost its Fork ownership")?;
        let scope_unit = self.unit_nodes.contains(fork).then_some(unit).flatten();
        for (branch, nodes) in &scope.branches {
            if selected.contains(branch) {
                continue;
            }
            let Some((state, _)) = flow::branch_state(self.db, self.run, fork, scope_unit, branch)?
            else {
                continue;
            };
            if state.settled()
                || !flow::update_branch(
                    self.db,
                    self.run,
                    fork,
                    scope_unit,
                    branch,
                    state,
                    BranchState::Cancelled,
                )?
            {
                continue;
            }
            for node in nodes {
                let node_unit = self.unit_nodes.contains(*node).then_some(unit).flatten();
                super::super::timed::cancel(self.db, self.run, node, node_unit)?;
                super::super::automation::cancel_branch_request(
                    self.db, self.run, node, node_unit,
                )?;
                let jobs = self.db.prepare(
                    "SELECT origin.job_id FROM job_pipeline_origins origin
                     LEFT JOIN workflow_job_inputs inputs ON inputs.job_id = origin.job_id
                     WHERE origin.project_id = ?1 AND origin.pipeline_run_id = ?2
                       AND origin.node_instance_id = ?3 AND origin.plan_revision IS ?4 AND inputs.unit_id IS ?5"
                )?.query_map(params![self.run.project_id, self.run.id, node, self.run.plan_revision, node_unit],
                    |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                for job in jobs {
                    crate::jobs::stop_branch_job(self.db, &self.run.project_id, &job)?;
                }
                if let NodeConfiguration::Fork(nested) = &self.nodes[node].instance.configuration {
                    for name in &nested.branches {
                        if let Some((current, _)) =
                            flow::branch_state(self.db, self.run, node, node_unit, name)?
                            && !current.settled()
                        {
                            flow::update_branch(
                                self.db,
                                self.run,
                                node,
                                node_unit,
                                name,
                                current,
                                BranchState::Cancelled,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn evaluate_flow(
        &self,
        node: &PinnedNode,
        unit: Option<&str>,
        dependencies: &Dependencies<'_>,
    ) -> Result<Option<FlowOutcome>> {
        use NodeConfiguration as Config;
        let id = node.instance.id.as_str();
        match &node.instance.configuration {
            Config::Fork(config) => {
                if let Some(outcome) = wait_or_skip(dependencies) {
                    return Ok(outcome);
                }
                let input = json_input(dependencies, "input")?;
                let artifacts = artifacts(dependencies);
                for branch in &config.branches {
                    self.record_stage_artifact(
                        id,
                        unit,
                        &flow::context_port(branch),
                        ArtifactValue::JsonObject(input.clone()),
                        &artifacts,
                    )?;
                }
                Ok(Some(FlowOutcome {
                    state: ControlStageState::Completed,
                    ports: config
                        .branches
                        .iter()
                        .map(|branch| flow::branch_port(branch))
                        .collect(),
                    reason: None,
                    branches: None,
                }))
            }
            Config::JoinResults(config) => self.join_results(id, unit, config, dependencies),
            Config::LoopGate(config) => self.loop_gate(id, unit, config, dependencies),
            Config::LoopStep(config) => self.loop_step(id, unit, config, dependencies),
            Config::LoopResult(config) => self.loop_results(id, unit, config, dependencies),
            _ => unreachable!("Only orchestration nodes use this evaluator"),
        }
    }

    fn observe_branch_completion(
        &self,
        completion: &Wire,
        arrival: &Arrival,
        unit: Option<&str>,
    ) -> BranchState {
        let id = completion.from.node.as_str();
        let source_unit = self.unit_nodes.contains(id).then_some(unit).flatten();
        let failed = self
            .receipt(id, source_unit)
            .is_some_and(|receipt| receipt.state == ControlStageState::Failed);
        match arrival {
            Arrival::Pending => BranchState::Running,
            Arrival::Selected(_) if failed || completion.from.port == "failed" => {
                BranchState::Failed
            }
            Arrival::Selected(_) => BranchState::Completed,
            Arrival::Ignored if failed => BranchState::Failed,
            Arrival::Ignored => BranchState::Skipped,
        }
    }

    fn result_source_settled(&self, wire: &Wire, unit: Option<&str>) -> bool {
        let id = wire.from.node.as_str();
        let unit = self.unit_nodes.contains(id).then_some(unit).flatten();
        !matches!(
            self.nodes[id].instance.configuration,
            NodeConfiguration::Fork(_)
        ) && self.receipt(id, unit).is_some()
    }

    fn join_results(
        &self,
        id: &str,
        unit: Option<&str>,
        config: &JoinResultsConfiguration,
        dependencies: &Dependencies<'_>,
    ) -> Result<Option<FlowOutcome>> {
        let mut observed = Vec::with_capacity(config.branches.len());
        for name in &config.branches {
            let (completion, arrival) = find(dependencies, &flow::branch_port(name))
                .context("Missing pinned Join branch")?;
            let mut state = self.observe_branch_completion(completion, arrival, unit);
            let mut order = None;
            if let Some(fork) = &config.fork {
                let scope_unit = self
                    .unit_nodes
                    .contains(fork.as_str())
                    .then_some(unit)
                    .flatten();
                if let Some((branch_state, settled_order)) =
                    flow::branch_state(self.db, self.run, fork, scope_unit, name)?
                {
                    state = branch_state;
                    order = settled_order;
                } else if !self
                    .receipt(fork, scope_unit)
                    .is_some_and(|receipt| receipt.state == ControlStageState::Skipped)
                {
                    state = BranchState::Queued;
                }
            }
            let mut result = None;
            if state == BranchState::Completed {
                match find(dependencies, &flow::result_port(name)) {
                    Some((_, Arrival::Selected(Some(artifact)))) => result = Some(artifact),
                    Some((wire, Arrival::Pending)) if !self.result_source_settled(wire, unit) => {
                        state = BranchState::Running
                    }
                    Some(_) => state = BranchState::Failed,
                    None => {}
                }
            }
            observed.push(ObservedBranch {
                name,
                state,
                order,
                result,
            });
        }
        let (outcome, selected) = choose_join(config, &mut observed)?;
        let Some(mut outcome) = outcome else {
            return Ok(None);
        };
        if outcome.state == ControlStageState::Skipped {
            return Ok(Some(outcome));
        }
        let mut branch_results = Map::new();
        let mut inputs = BTreeMap::new();
        for branch in &observed {
            if !selected.contains(branch.name) {
                continue;
            }
            let mut entry = json!({"status": branch.state.key()});
            if let Some(artifact) = branch.result {
                let ArtifactValue::JsonObject(value) = &artifact.value else {
                    anyhow::bail!("The Join branch result is outside its pinned JSON contract");
                };
                entry["result"] = json!(value);
                inputs.insert(flow::result_port(branch.name), artifact.clone());
            }
            if branch.state == BranchState::Failed {
                entry["error"] = json!("The branch did not produce its requested result");
            }
            branch_results.insert(branch.name.into(), entry);
        }
        self.record_stage_artifact(
            id,
            unit,
            "results",
            ArtifactValue::JsonObject(Map::from_iter([(
                "branches".into(),
                Value::Object(branch_results),
            )])),
            &inputs,
        )?;
        if config.cancel_remaining
            && let Some(fork) = &config.fork
        {
            self.cancel_remaining(fork, unit, &selected)?;
        }
        outcome.branches = Some(selected.into_iter().map(str::to_owned).collect());
        Ok(Some(outcome))
    }

    fn loop_gate(
        &self,
        id: &str,
        unit: Option<&str>,
        config: &LoopGateConfiguration,
        dependencies: &Dependencies<'_>,
    ) -> Result<Option<FlowOutcome>> {
        if let Some(outcome) = wait_or_skip(dependencies) {
            return Ok(outcome);
        }
        let mut context = json!({"input": json_input(dependencies, "input")?, "index": config.index, "limit": config.max_iterations});
        if config.index > 1 {
            context["previous"] = json!(json_input(dependencies, "previous")?);
        }
        if config
            .break_when
            .as_ref()
            .is_some_and(|condition| condition.test(&context))
        {
            return Ok(Some(FlowOutcome::completed("break")));
        }
        let Value::Object(context) = context else {
            unreachable!()
        };
        self.record_stage_artifact(
            id,
            unit,
            "context",
            ArtifactValue::JsonObject(context),
            &artifacts(dependencies),
        )?;
        Ok(Some(FlowOutcome::completed("iterate")))
    }

    fn loop_step(
        &self,
        id: &str,
        unit: Option<&str>,
        config: &LoopStepConfiguration,
        dependencies: &Dependencies<'_>,
    ) -> Result<Option<FlowOutcome>> {
        match find(dependencies, "started").map(|(_, value)| value) {
            Some(Arrival::Ignored) => return Ok(Some(FlowOutcome::skipped())),
            Some(Arrival::Selected(_)) => {}
            _ => return Ok(None),
        }
        if matches!(
            find(dependencies, "failed"),
            Some((_, Arrival::Selected(_)))
        ) {
            return Ok(Some(FlowOutcome::failed("The Loop Function failed")));
        }
        let (Some((completed, complete)), Some((result_wire, result))) =
            (find(dependencies, "completed"), find(dependencies, "value"))
        else {
            anyhow::bail!("The Loop Function lost its pinned outputs");
        };
        if matches!(complete, Arrival::Ignored)
            && matches!(find(dependencies, "failed"), Some((_, Arrival::Pending)))
        {
            return Ok(None);
        }
        if self.observe_branch_completion(completed, complete, unit) == BranchState::Failed
            || matches!(complete, Arrival::Ignored)
            || matches!(result, Arrival::Ignored)
            || matches!(result, Arrival::Pending) && self.result_source_settled(result_wire, unit)
        {
            return Ok(Some(FlowOutcome::failed(
                "The Loop Function ended without an iteration result",
            )));
        }
        if !matches!(
            (complete, result),
            (Arrival::Selected(_), Arrival::Selected(Some(_)))
        ) {
            return Ok(None);
        }
        // A Function can return through an early Join while other work in the same
        // invocation is still running. Keep iterations sequential across that boundary.
        let prefix = id
            .rsplit_once("::")
            .map_or(String::new(), |(scope, _)| format!("{scope}::"));
        let invocation = format!("{prefix}iteration-{}", config.index);
        let body = self
            .definition
            .subflows
            .iter()
            .find(|scope| scope.path == invocation)
            .context("The Loop lost its pinned iteration subflow")?;
        for child in &body.nodes {
            if !self
                .branch_stage_state(self.nodes[child.as_str()], unit)?
                .settled()
            {
                return Ok(None);
            }
        }
        let values = json_input(dependencies, "value")?;
        let mut decision = json_input(dependencies, "context")?.clone();
        decision.insert("result".into(), json!(values));
        let port = if config
            .exit_when
            .as_ref()
            .is_some_and(|condition| condition.test(&Value::Object(decision)))
        {
            "exit"
        } else if config.index == config.max_iterations {
            "limit"
        } else {
            "next"
        };
        self.record_stage_artifact(
            id,
            unit,
            "result",
            ArtifactValue::JsonObject(values.clone()),
            &artifacts(dependencies),
        )?;
        Ok(Some(FlowOutcome::completed(port)))
    }

    fn loop_results(
        &self,
        id: &str,
        unit: Option<&str>,
        config: &LoopResultConfiguration,
        dependencies: &Dependencies<'_>,
    ) -> Result<Option<FlowOutcome>> {
        let mut stop = None;
        for index in 1..=config.rounds {
            for reason in ["break", "exit", "limit", "failed"] {
                if matches!(
                    find(dependencies, &format!("{reason}:{index}")),
                    Some((_, Arrival::Selected(_)))
                ) {
                    ensure!(
                        stop.replace((index, reason)).is_none(),
                        "The Loop selected multiple termination paths"
                    );
                }
            }
        }
        let Some((index, reason)) = stop else {
            return Ok(dependencies
                .iter()
                .all(|(_, arrival)| matches!(arrival, Arrival::Ignored))
                .then(FlowOutcome::skipped));
        };
        let last = if matches!(reason, "break" | "failed") {
            index - 1
        } else {
            index
        };
        let mut values = Vec::new();
        let mut inputs = BTreeMap::new();
        for ordinal in 1..=last {
            let port = format!("result:{ordinal}");
            let (_, Arrival::Selected(Some(artifact))) =
                find(dependencies, &port).context("A finished Loop lost an iteration result")?
            else {
                return Ok(None);
            };
            values.push(Value::Object(json_input(dependencies, &port)?.clone()));
            inputs.insert(port, artifact.clone());
        }
        let result = Map::from_iter([
            ("iterations".into(), Value::Array(values)),
            ("reason".into(), json!(reason)),
        ]);
        self.record_stage_artifact(
            id,
            unit,
            "results",
            ArtifactValue::JsonObject(result),
            &inputs,
        )?;
        let outcome = match (reason, config.limit) {
            ("failed", _) => FlowOutcome::failed("A Loop iteration failed"),
            ("limit", LoopLimit::Fail) => {
                FlowOutcome::failed("The Loop reached its configured iteration limit")
            }
            ("limit", LoopLimit::LimitReached) => FlowOutcome::completed("limit_reached"),
            _ => FlowOutcome::completed("completed"),
        };
        Ok(Some(outcome))
    }
}

struct ObservedBranch<'a> {
    name: &'a str,
    state: BranchState,
    order: Option<i64>,
    result: Option<&'a WorkflowArtifact>,
}

fn choose_join<'a>(
    config: &JoinResultsConfiguration,
    branches: &mut [ObservedBranch<'a>],
) -> Result<(Option<FlowOutcome>, BTreeSet<&'a str>)> {
    let mut selected = BTreeSet::new();
    let pending = branches
        .iter()
        .filter(|branch| !branch.state.settled())
        .count();
    if branches
        .iter()
        .all(|branch| branch.state == BranchState::Skipped)
    {
        return Ok((Some(FlowOutcome::skipped()), selected));
    }
    let eligible = |state| {
        state == BranchState::Completed || config.count_failures && state == BranchState::Failed
    };
    if config.strategy == JoinStrategy::All {
        if pending != 0 {
            return Ok((None, selected));
        }
        selected.extend(branches.iter().map(|branch| branch.name));
        let outcome = if branches
            .iter()
            .any(|branch| branch.state != BranchState::Skipped && !eligible(branch.state))
        {
            FlowOutcome::failed("A required Join branch failed")
        } else {
            FlowOutcome::completed("joined")
        };
        return Ok((Some(outcome), selected));
    }
    let required = match config.strategy {
        JoinStrategy::Any => 1,
        JoinStrategy::Quorum => usize::from(config.quorum.context("The Join lost its quorum")?),
        JoinStrategy::All => unreachable!(),
    };
    let count = branches
        .iter()
        .filter(|branch| eligible(branch.state))
        .count();
    if count >= required {
        branches.sort_by_key(|branch| branch.order.unwrap_or(i64::MAX));
        selected.extend(
            branches
                .iter()
                .filter(|branch| eligible(branch.state))
                .take(required)
                .map(|branch| branch.name),
        );
        Ok((Some(FlowOutcome::completed("joined")), selected))
    } else if count + pending < required
        && branches
            .iter()
            .any(|branch| branch.state.settled() && branch.state != BranchState::Skipped)
    {
        selected.extend(
            branches
                .iter()
                .filter(|branch| branch.state.settled())
                .map(|branch| branch.name),
        );
        Ok((
            Some(FlowOutcome::failed(
                "The Join cannot reach its required branch count",
            )),
            selected,
        ))
    } else {
        Ok((None, selected))
    }
}

fn find<'a>(dependencies: &'a Dependencies<'a>, port: &str) -> Option<(&'a Wire, &'a Arrival)> {
    dependencies
        .iter()
        .find(|(wire, _)| wire.to.port == port)
        .map(|(wire, signal)| (*wire, signal))
}

fn json_input<'a>(
    dependencies: &'a Dependencies<'a>,
    port: &str,
) -> Result<&'a Map<String, Value>> {
    let Some((_, Arrival::Selected(Some(artifact)))) = find(dependencies, port) else {
        anyhow::bail!("The orchestration node lost its pinned JSON input {port}");
    };
    let ArtifactValue::JsonObject(value) = &artifact.value else {
        anyhow::bail!("The orchestration node needs a JSON object on {port}");
    };
    Ok(value)
}

fn artifacts(dependencies: &Dependencies<'_>) -> BTreeMap<String, WorkflowArtifact> {
    dependencies
        .iter()
        .filter_map(|(wire, signal)| match signal {
            Arrival::Selected(Some(artifact)) => Some((wire.to.port.clone(), artifact.clone())),
            _ => None,
        })
        .collect()
}

fn wait_or_skip(dependencies: &Dependencies<'_>) -> Option<Option<FlowOutcome>> {
    if dependencies
        .iter()
        .any(|(_, arrival)| matches!(arrival, Arrival::Ignored))
    {
        Some(Some(FlowOutcome::skipped()))
    } else if dependencies
        .iter()
        .any(|(_, arrival)| matches!(arrival, Arrival::Pending))
    {
        Some(None)
    } else {
        None
    }
}
