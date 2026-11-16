use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{
    BuiltinNode, ConditionPredicate, Endpoint, NodeConfiguration, NodeInstance, Port, PortType,
    Wire, WorkflowGraph, builtin_registry, compile_resolved_graph,
    subflow::{self, BoundSubflow},
};

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
}

impl RepeatUntilConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=32).contains(&self.max_iterations)
            && [
                &self.ready_input,
                &self.completion_output,
                &self.evidence_output,
            ]
            .iter()
            .all(|port| !port.trim().is_empty())
            && self.until.valid()
            && self.feedback.as_ref().is_none_or(|feedback| {
                feedback.input != self.ready_input
                    && !feedback.input.trim().is_empty()
                    && !feedback.output.trim().is_empty()
            })
    }
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
            node("stop", BuiltinNode::Join, NodeConfiguration::None),
            node(
                "limit",
                BuiltinNode::IterationLimit,
                NodeConfiguration::None,
            ),
        ],
        wires: Vec::new(),
    };
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
        for input in body.inputs.keys() {
            let source = if index > 1 && input == &configuration.ready_input {
                endpoint(&format!("condition-{previous}"), "unmatched")
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
            wire(&decide, "matched", "stop", "branches"),
        ]);
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
            Ok((
                node.id.clone(),
                subflow::entry_definition(node, &registry[&node.kind])?,
            ))
        })
        .collect::<Result<_>>()?;
    let exports = [
        ("completed".into(), endpoint("stop", "joined")),
        ("failed".into(), endpoint("limit", "failed")),
    ]
    .into();
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
