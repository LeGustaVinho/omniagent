use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{
    ArtifactKind, BuiltinNode, ConditionPredicate, Endpoint, NodeConfiguration, NodeDefinition,
    NodeInstance, Port, PortType, Wire, WorkflowGraph, builtin_registry, compile_resolved_graph,
    iteration::validate_loop_body,
    subflow::{self, BoundSubflow},
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatFeedback {
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatUntilConfiguration {
    pub max_iterations: u16,
    pub ready_input: String,
    pub completion_output: String,
    pub evidence_output: String,
    pub until: ConditionPredicate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<RepeatFeedback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_attempt_budget: Option<u16>,
}

const MAX_EXPORTS: usize = 32;

impl RepeatUntilConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=32).contains(&self.max_iterations)
            && self
                .delivery_attempt_budget
                .is_none_or(|limit| (1..=32).contains(&limit))
            && [
                &self.ready_input,
                &self.completion_output,
                &self.evidence_output,
            ]
            .iter()
            .all(|port| !port.trim().is_empty())
            && self.until.valid()
            && self.exports.len() <= MAX_EXPORTS
            && self.exports.iter().all(|name| valid_export_name(name))
            && self.exports.iter().collect::<HashSet<_>>().len() == self.exports.len()
            && self.feedback.as_ref().is_none_or(|feedback| {
                feedback.input != self.ready_input
                    && !feedback.input.trim().is_empty()
                    && !feedback.output.trim().is_empty()
            })
    }
}

fn valid_export_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !matches!(name, "completed" | "failed")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepeatOutputConfiguration {
    pub(super) rounds: u16,
    pub(super) exports: BTreeMap<String, ArtifactKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RepeatSignal {
    Skipped,
    Waiting,
    Selected,
}

pub(super) enum RepeatSelection {
    Skipped,
    Waiting,
    MissingOutput,
    Approved(u16),
}

impl RepeatOutputConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=32).contains(&self.rounds)
            && (1..=MAX_EXPORTS).contains(&self.exports.len())
            && self.exports.keys().all(|name| valid_export_name(name))
    }

    fn define_ports(&self, definition: &mut NodeDefinition) {
        for (name, kind) in &self.exports {
            definition.outputs.push(Port {
                name: name.clone(),
                value: PortType::Data(*kind),
            });
        }
        for round in 1..=self.rounds {
            definition.inputs.push(Port {
                name: Self::round_port(round),
                value: PortType::Control,
            });
            for (name, kind) in &self.exports {
                definition.inputs.push(Port {
                    name: Self::value_port(round, name),
                    value: PortType::Data(*kind),
                });
            }
        }
    }

    fn round_port(round: u16) -> String {
        format!("round:{round}")
    }

    pub(super) fn value_port(round: u16, name: &str) -> String {
        format!("round:{round}:{name}")
    }

    pub(super) fn select(&self, signal: impl Fn(&str) -> RepeatSignal) -> Result<RepeatSelection> {
        let mut chosen = None;
        for round in 1..=self.rounds {
            match signal(&Self::round_port(round)) {
                RepeatSignal::Waiting => return Ok(RepeatSelection::Waiting),
                RepeatSignal::Selected => ensure!(
                    chosen.replace(round).is_none(),
                    "Multiple iterations reached the approval gate"
                ),
                RepeatSignal::Skipped => {}
            }
        }
        let Some(round) = chosen else {
            return Ok(RepeatSelection::Skipped);
        };
        let signals: Vec<_> = self
            .exports
            .keys()
            .map(|name| signal(&Self::value_port(round, name)))
            .collect();
        if signals.contains(&RepeatSignal::Waiting) {
            Ok(RepeatSelection::Waiting)
        } else if signals.contains(&RepeatSignal::Skipped) {
            Ok(RepeatSelection::MissingOutput)
        } else {
            Ok(RepeatSelection::Approved(round))
        }
    }
}

pub(super) fn matches_output_definition(base: &NodeDefinition, pinned: &NodeDefinition) -> bool {
    let exports = pinned
        .outputs
        .iter()
        .filter_map(|port| {
            let PortType::Data(kind) = port.value else {
                return None;
            };
            Some((port.name.clone(), kind))
        })
        .collect::<BTreeMap<_, _>>();
    let rounds = pinned
        .inputs
        .iter()
        .filter(|port| port.value == PortType::Control)
        .count();
    let Ok(rounds) = u16::try_from(rounds) else {
        return false;
    };
    let configuration = RepeatOutputConfiguration { exports, rounds };
    if !configuration.valid() {
        return false;
    }
    let mut definition = base.clone();
    configuration.define_ports(&mut definition);
    &definition == pinned
}

pub(super) fn expand_repeat(
    body: BoundSubflow,
    configuration: &RepeatUntilConfiguration,
) -> Result<BoundSubflow> {
    ensure!(
        configuration.valid(),
        "A repeat needs a typed stopping predicate and 1 to 32 iterations"
    );
    ensure!(
        configuration.delivery_attempt_budget.is_none()
            || body.workflow.nodes.iter().any(|node| matches!(
                node.instance.kind,
                BuiltinNode::Implement | BuiltinNode::Command
            )),
        "A shared attempt budget requires an implementation or command in the repeated delivery body"
    );
    ensure!(
        body.workflow
            .nodes
            .len()
            .saturating_mul(usize::from(configuration.max_iterations))
            <= 4096,
        "The repeated body exceeds the expanded workflow size limit"
    );
    let input_type = |name: &str| {
        subflow::output(
            &body.workflow,
            body.inputs.get(name).with_context(|| {
                format!("Repeat input {name} is unavailable in the pinned body")
            })?,
        )
    };
    let output_type = |name: &str| {
        subflow::output(
            &body.workflow,
            body.outputs.get(name).with_context(|| {
                format!("Repeat output {name} is unavailable in the pinned body")
            })?,
        )
    };
    ensure!(
        input_type(&configuration.ready_input)? == PortType::Control,
        "The repeat ready input must be a control port"
    );
    ensure!(
        output_type(&configuration.completion_output)? == PortType::Control,
        "The repeat completion output must be a control port"
    );
    ensure!(
        matches!(
            output_type(&configuration.evidence_output)?,
            PortType::Data(_)
        ),
        "The repeat stopping condition must inspect a typed body output"
    );
    if let Some(feedback) = &configuration.feedback {
        let kind = input_type(&feedback.input)?;
        ensure!(
            matches!(kind, PortType::Data(_)) && kind == output_type(&feedback.output)?,
            "Repeat feedback needs an input and output of the same data type"
        );
    }
    validate_loop_body(
        &body,
        &body.inputs[&configuration.ready_input],
        None,
        &body.outputs[&configuration.completion_output],
        &body.outputs[&configuration.evidence_output],
    )?;
    let exports = configuration
        .exports
        .iter()
        .map(|name| {
            let PortType::Data(kind) = output_type(name)? else {
                anyhow::bail!("Repeat export {name} must be a typed data output");
            };
            Ok((name.clone(), kind))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let export_configuration = (!exports.is_empty()).then_some(RepeatOutputConfiguration {
        rounds: configuration.max_iterations,
        exports,
    });

    let node = |id: &str, kind, configuration| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    };
    let endpoint = |node: &str, port: &str| Endpoint {
        node: node.into(),
        port: port.into(),
    };
    let wire = |source: &str, output: &str, target: &str, input: &str| Wire {
        from: endpoint(source, output),
        to: endpoint(target, input),
    };
    let start_ports = body
        .inputs
        .keys()
        .map(|name| {
            Ok(Port {
                name: name.clone(),
                value: input_type(name)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let name = format!("Repeat {}", body.name);
    let stop = match &export_configuration {
        Some(configuration) => node(
            "stop",
            BuiltinNode::RepeatOutput,
            NodeConfiguration::RepeatOutput(configuration.clone()),
        ),
        None => node("stop", BuiltinNode::Join, NodeConfiguration::None),
    };
    let mut graph = WorkflowGraph {
        name: name.clone(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry {
                    outputs: start_ports,
                },
            ),
            stop,
            node(
                "limit",
                BuiltinNode::IterationLimit,
                NodeConfiguration::None,
            ),
        ],
        wires: Vec::new(),
    };
    let has_failure_join =
        export_configuration.is_some() || configuration.delivery_attempt_budget.is_some();
    if has_failure_join {
        graph
            .nodes
            .push(node("failure", BuiltinNode::Join, NodeConfiguration::None));
        graph
            .wires
            .push(wire("limit", "failed", "failure", "branches"));
        if export_configuration.is_some() {
            graph
                .wires
                .push(wire("stop", "failed", "failure", "branches"));
        }
    }
    let mut calls = BTreeMap::<String, BoundSubflow>::new();
    for index in 1..=configuration.max_iterations {
        let iteration = format!("iteration-{index}");
        let decide = format!("condition-{index}");
        graph.nodes.push(node(
            &iteration,
            BuiltinNode::Subflow,
            NodeConfiguration::None,
        ));
        graph.nodes.push(node(
            &decide,
            BuiltinNode::Condition,
            NodeConfiguration::Condition {
                predicate: configuration.until.clone(),
            },
        ));
        let previous = index - 1;
        let ready = if index == 1 {
            endpoint("start", &configuration.ready_input)
        } else {
            endpoint(&format!("condition-{previous}"), "unmatched")
        };
        let ready = if let Some(limit) = configuration.delivery_attempt_budget {
            let id = format!("attempt-{index}");
            graph.nodes.push(node(
                &id,
                BuiltinNode::DeliveryAttempt,
                NodeConfiguration::DeliveryAttempt {
                    ordinal: index,
                    limit,
                },
            ));
            graph.wires.push(Wire {
                from: ready,
                to: endpoint(&id, "ready"),
            });
            graph
                .wires
                .push(wire(&id, "exhausted", "failure", "branches"));
            endpoint(&id, "admitted")
        } else {
            ready
        };
        for input in body.inputs.keys() {
            let source = if input == &configuration.ready_input {
                ready.clone()
            } else if let Some(feedback) = &configuration.feedback
                && index > 1
                && input == &feedback.input
            {
                endpoint(&format!("iteration-{previous}"), &feedback.output)
            } else {
                endpoint("start", input)
            };
            graph.wires.push(Wire {
                from: source,
                to: endpoint(&iteration, input),
            });
        }
        graph.wires.extend([
            wire(
                &iteration,
                &configuration.completion_output,
                &decide,
                "ready",
            ),
            wire(&iteration, &configuration.evidence_output, &decide, "value"),
        ]);
        if let Some(configuration) = &export_configuration {
            graph.wires.push(wire(
                &decide,
                "matched",
                "stop",
                &RepeatOutputConfiguration::round_port(index),
            ));
            graph.wires.extend(configuration.exports.keys().map(|name| {
                wire(
                    &iteration,
                    name,
                    "stop",
                    &RepeatOutputConfiguration::value_port(index, name),
                )
            }));
        } else {
            graph
                .wires
                .push(wire(&decide, "matched", "stop", "branches"));
        }
        if index == configuration.max_iterations {
            graph
                .wires
                .push(wire(&decide, "unmatched", "limit", "ready"));
        }
        calls.insert(iteration, body.clone());
    }
    let registry: HashMap<_, _> = builtin_registry()
        .into_iter()
        .map(|pin| (pin.kind, pin))
        .collect();
    let definitions = graph
        .nodes
        .iter()
        .map(|node| {
            let mut definition = subflow::entry_definition(node, &registry[&node.kind])?;
            if let NodeConfiguration::RepeatOutput(configuration) = &node.configuration {
                configuration.define_ports(&mut definition);
            }
            Ok((node.id.clone(), definition))
        })
        .collect::<Result<_>>()?;
    let exports = match &export_configuration {
        Some(configuration) => configuration
            .exports
            .keys()
            .map(|name| (name.clone(), endpoint("stop", name)))
            .chain([
                ("completed".into(), endpoint("stop", "completed")),
                ("failed".into(), endpoint("failure", "joined")),
            ])
            .collect(),
        None => [
            ("completed".into(), endpoint("stop", "joined")),
            (
                "failed".into(),
                if has_failure_join {
                    endpoint("failure", "joined")
                } else {
                    endpoint("limit", "failed")
                },
            ),
        ]
        .into(),
    };
    let expanded = subflow::expand(graph, definitions, calls, &exports)?;
    let mut workflow = compile_resolved_graph(
        body.version.id.clone(),
        expanded.graph,
        |node| expanded.definitions.get(&node.id),
        false,
    )?;
    workflow.subflows = expanded.subflows;
    let inputs = body
        .inputs
        .keys()
        .map(|name| (name.clone(), endpoint("start", name)))
        .collect();
    subflow::validate_interface(&workflow, &inputs, &expanded.outputs)?;
    Ok(BoundSubflow {
        name,
        version: body.version,
        workflow,
        inputs,
        outputs: expanded.outputs,
    })
}
