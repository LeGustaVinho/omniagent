use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::super::{
    BuiltinNode, Endpoint, NodeConfiguration, NodeInstance, Wire, WorkflowGraph, builtin_registry,
    compile_resolved_graph, iteration, schema,
    subflow::{self, BoundSubflow},
};
use super::{
    CONTROL, LoopConfiguration, LoopGateConfiguration, LoopResultConfiguration,
    LoopStepConfiguration, OBJECT, define, ports,
};

pub(in crate::workflows) fn expand_loop(
    body: BoundSubflow,
    config: &LoopConfiguration,
) -> Result<BoundSubflow> {
    ensure!(
        config.valid(),
        "Loop needs valid predicates and an iteration bound from 1 through 32"
    );
    ensure!(
        body.workflow
            .nodes
            .len()
            .saturating_mul(usize::from(config.max_iterations))
            <= 4096,
        "The expanded Loop body exceeds the workflow size limit"
    );
    ensure!(
        body.inputs.len() == 2,
        "A Loop Function needs exactly its ready and iteration context inputs"
    );
    let input = |name: &str| {
        body.inputs
            .get(name)
            .with_context(|| format!("Loop body input {name} is unavailable"))
    };
    let output = |name: &str| {
        body.outputs
            .get(name)
            .with_context(|| format!("Loop body output {name} is unavailable"))
    };
    let ready = input(&config.ready_input)?;
    let context = input(&config.context_input)?;
    let completed = output(&config.completion_output)?;
    let result = output(&config.result_output)?;
    ensure!(
        subflow::output(&body.workflow, ready)? == CONTROL
            && subflow::output(&body.workflow, context)? == OBJECT
            && subflow::output(&body.workflow, completed)? == CONTROL
            && subflow::output(&body.workflow, result)? == OBJECT,
        "Map the Loop ready and completed control ports and the context and result JSON objects"
    );
    let failure = config.failure_output.as_deref().or_else(|| {
        (!matches!(config.completion_output.as_str(), "failed")
            && body.outputs.contains_key("failed"))
        .then_some("failed")
    });
    if let Some(name) = failure {
        ensure!(
            subflow::output(&body.workflow, output(name)?)? == CONTROL,
            "The Loop body's failure output must be a control port"
        );
    }
    iteration::validate_loop_body(&body, ready, Some(context), completed, result)?;

    let output_schema = |endpoint: &Endpoint| {
        body.workflow
            .nodes
            .iter()
            .find(|node| node.instance.id == endpoint.node)
            .and_then(|node| node.definition.output_schemas.get(&endpoint.port))
    };
    let body_context = output_schema(context);
    let body_result = output_schema(result);
    let input_schema = body_context
        .map(|schema| {
            schema
                .pointer("/properties/input")
                .context("The Loop context Schema needs an input object field")
        })
        .transpose()?;
    let context_schema = |has_previous| -> Result<Option<Value>> {
        let Some(input_schema) = input_schema else {
            return Ok(None);
        };
        ensure!(
            input_schema.get("type").and_then(Value::as_str) == Some("object"),
            "The Loop context input field must be a JSON object"
        );
        let mut properties = serde_json::Map::from_iter([
            ("input".into(), input_schema.clone()),
            ("index".into(), json!({"type":"integer"})),
            ("limit".into(), json!({"type":"integer"})),
        ]);
        let mut required = vec!["input", "index", "limit"];
        if has_previous {
            properties.insert(
                "previous".into(),
                body_result
                    .context("A typed repeated Function needs a Schema for its previous result")?
                    .clone(),
            );
            required.push("previous");
        }
        let shape = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
        if let Some(target) = body_context {
            schema::compatible(&shape, target)
                .context("The generated Loop context does not match the Function input")?;
        }
        Ok(Some(shape))
    };
    let name = format!("Loop {}", body.name);
    let entry = ports(&[("ready", CONTROL), ("input", OBJECT)]);
    let mut graph = WorkflowGraph {
        name: name.clone(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry { outputs: entry },
            ),
            node(
                "collect",
                BuiltinNode::LoopResult,
                NodeConfiguration::LoopResult(LoopResultConfiguration {
                    rounds: config.max_iterations,
                    limit: config.limit,
                }),
            ),
        ],
        wires: Vec::new(),
    };
    let mut calls = BTreeMap::new();
    let mut contexts = BTreeMap::new();
    for index in 1..=config.max_iterations {
        let gate = format!("gate-{index}");
        let call = format!("iteration-{index}");
        let step = format!("step-{index}");
        let previous = format!("step-{}", index - 1);
        contexts.insert(gate.clone(), context_schema(index > 1)?);
        graph.nodes.extend([
            node(
                &gate,
                BuiltinNode::LoopGate,
                NodeConfiguration::LoopGate(LoopGateConfiguration {
                    index,
                    max_iterations: config.max_iterations,
                    break_when: config.break_when.clone(),
                }),
            ),
            node(&call, BuiltinNode::Subflow, NodeConfiguration::None),
            node(
                &step,
                BuiltinNode::LoopStep,
                NodeConfiguration::LoopStep(LoopStepConfiguration {
                    index,
                    max_iterations: config.max_iterations,
                    exit_when: config.exit_when.clone(),
                    failure_input: failure.is_some(),
                }),
            ),
        ]);
        graph.wires.extend([
            wire("start", "input", &gate, "input"),
            wire(&gate, "iterate", &call, &config.ready_input),
            wire(&gate, "context", &call, &config.context_input),
            wire(&gate, "iterate", &step, "started"),
            wire(&gate, "context", &step, "context"),
            wire(&call, &config.completion_output, &step, "completed"),
            wire(&call, &config.result_output, &step, "value"),
            wire(&gate, "break", "collect", &format!("break:{index}")),
            wire(&step, "exit", "collect", &format!("exit:{index}")),
            wire(&step, "limit", "collect", &format!("limit:{index}")),
            wire(&step, "failed", "collect", &format!("failed:{index}")),
            wire(&step, "result", "collect", &format!("result:{index}")),
        ]);
        if let Some(failure) = failure {
            graph.wires.push(wire(&call, failure, &step, "failed"));
        }
        if index == 1 {
            graph.wires.push(wire("start", "ready", &gate, "ready"));
        } else {
            graph.wires.extend([
                wire(&previous, "next", &gate, "ready"),
                wire(&previous, "result", &gate, "previous"),
            ]);
        }
        calls.insert(call, body.clone());
    }
    let registry: HashMap<_, _> = builtin_registry()
        .into_iter()
        .map(|definition| (definition.kind, definition))
        .collect();
    let definitions = graph
        .nodes
        .iter()
        .map(|node| {
            let mut definition = subflow::entry_definition(node, &registry[&node.kind])?;
            define(node, &mut definition)?;
            if let Some(shape) = input_schema {
                if node.kind == BuiltinNode::Start {
                    definition
                        .output_schemas
                        .insert("input".into(), shape.clone());
                }
                if node.kind == BuiltinNode::LoopGate {
                    definition
                        .input_schemas
                        .insert("input".into(), shape.clone());
                }
            }
            if let Some(Some(shape)) = contexts.get(&node.id) {
                definition
                    .output_schemas
                    .insert("context".into(), shape.clone());
            }
            if let Some(shape) = body_result {
                if node.kind == BuiltinNode::LoopStep {
                    definition
                        .output_schemas
                        .insert("result".into(), shape.clone());
                }
                if node.kind == BuiltinNode::LoopResult {
                    definition.output_schemas.insert("results".into(), json!({
                    "type":"object", "properties": {
                        "iterations":{"type":"array","items":shape}, "reason":{"type":"string"}
                     }, "required":["iterations","reason"], "additionalProperties":false,
                }));
                }
            }
            Ok((node.id.clone(), definition))
        })
        .collect::<Result<_>>()?;
    let exports = ["completed", "limit_reached", "failed", "results"]
        .into_iter()
        .map(|name| (name.into(), endpoint("collect", name)))
        .collect();
    let expanded = subflow::expand(graph, definitions, calls, &exports)?;
    let mut workflow = compile_resolved_graph(
        body.version.id.clone(),
        expanded.graph,
        |node| expanded.definitions.get(&node.id),
        false,
    )?;
    workflow.subflows = expanded.subflows;
    let inputs = ["ready", "input"]
        .into_iter()
        .map(|name| (name.into(), endpoint("start", name)))
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

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn wire(source: &str, out: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(source, out),
        to: endpoint(target, input),
    }
}
