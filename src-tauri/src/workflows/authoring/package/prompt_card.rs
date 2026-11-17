use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};

use crate::{
    providers::SessionMode,
    workflows::{
        ArtifactKind, BuiltinNode, ConditionPredicate, Endpoint, NodeConfiguration, NodeDefinition,
        NodeInstance, Port, PortType, Wire, WorkflowGraph, builtin_registry,
        prompt::{PromptDecisionConfiguration, PromptOutcomeConfiguration, valid_identifier},
        stage_plan::StagePlanConfiguration,
    },
};

use super::{PackageArtifactBody, PromptCard, PublishedPackageArtifact};

const ENTRY: &str = "entry";
const EXECUTE: &str = "execute";
const FAILURE: &str = "failure";

pub(super) struct LoweredPrompt {
    pub(super) graph: WorkflowGraph,
    pub(super) pins: BTreeMap<String, NodeDefinition>,
    pub(super) dependencies: BTreeMap<String, PublishedPackageArtifact>,
    pub(super) inputs: BTreeMap<String, Endpoint>,
    pub(super) outputs: BTreeMap<String, Endpoint>,
}

struct PromptGraph {
    graph: WorkflowGraph,
    registry: HashMap<BuiltinNode, NodeDefinition>,
    pins: BTreeMap<String, NodeDefinition>,
    dependencies: BTreeMap<String, PublishedPackageArtifact>,
}

impl PromptGraph {
    fn new(name: &str) -> Self {
        Self {
            graph: WorkflowGraph {
                name: name.into(),
                nodes: Vec::new(),
                wires: Vec::new(),
            },
            registry: builtin_registry()
                .into_iter()
                .map(|definition| (definition.kind, definition))
                .collect(),
            pins: BTreeMap::new(),
            dependencies: BTreeMap::new(),
        }
    }

    fn node(
        &mut self,
        id: &str,
        kind: BuiltinNode,
        configuration: NodeConfiguration,
    ) -> Result<()> {
        let mut definition = self.registry[&kind].clone();
        if let NodeConfiguration::PromptOutcome(outcome) = &configuration {
            definition = outcome.definition(&definition)?;
        }
        self.pins.insert(id.into(), definition);
        self.graph.nodes.push(NodeInstance {
            id: id.into(),
            kind,
            configuration,
        });
        Ok(())
    }

    fn wire(&mut self, from: Endpoint, to: &str, port: &str) {
        self.graph.wires.push(Wire {
            from,
            to: endpoint(to, port),
        });
    }

    fn step(
        &mut self,
        id: &str,
        kind: BuiltinNode,
        configuration: NodeConfiguration,
        ready: Endpoint,
    ) -> Result<()> {
        self.node(id, kind, configuration)?;
        self.wire(ready, id, "ready");
        Ok(())
    }

    fn failure(&mut self, id: &str) {
        if output_is(&self.pins, id, "failed", PortType::Control) {
            self.wire(endpoint(id, "failed"), FAILURE, "branches");
        }
    }

    fn text(&mut self, id: &str, template: String, ready: Endpoint) -> Result<()> {
        let configuration =
            serde_json::from_value(json!({"kind":"build_text","template":template}))?;
        self.step(id, BuiltinNode::BuildText, configuration, ready)?;
        self.failure(id);
        Ok(())
    }

    fn field(
        &mut self,
        id: &str,
        name: &str,
        pointer: String,
        source: Endpoint,
        ready: Endpoint,
    ) -> Result<()> {
        let configuration = serde_json::from_value(json!({
            "kind":"set_fields","mode":"build","operations":[
                {"op":"set","path":format!("/{name}"),"value":{"source":"input","pointer":pointer}}
            ]
        }))?;
        self.step(id, BuiltinNode::SetFields, configuration, ready)?;
        self.wire(source, id, "source");
        self.failure(id);
        Ok(())
    }

    fn condition(
        &mut self,
        id: &str,
        ready: Endpoint,
        source: Endpoint,
        predicate: ConditionPredicate,
    ) -> Result<()> {
        self.step(
            id,
            BuiltinNode::Condition,
            NodeConfiguration::Condition { predicate },
            ready,
        )?;
        self.wire(source, id, "value");
        Ok(())
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn output_is(
    definitions: &BTreeMap<String, NodeDefinition>,
    node: &str,
    port: &str,
    value: PortType,
) -> bool {
    definitions.get(node).is_some_and(|definition| {
        definition
            .outputs
            .iter()
            .any(|output| output.name == port && output.value == value)
    })
}

fn role<'a>(
    dependencies: &'a BTreeMap<String, PublishedPackageArtifact>,
    slot: &str,
    mode: SessionMode,
) -> Result<&'a PublishedPackageArtifact> {
    let artifact = dependencies
        .get(slot)
        .with_context(|| format!("Choose an exact Role revision for {slot}"))?;
    let PackageArtifactBody::Role { template } = &artifact.body else {
        bail!("The reference {slot} must select a Role revision");
    };
    ensure!(
        template.tool_scope == mode
            || mode == SessionMode::Plan && template.tool_scope == SessionMode::ReadOnly,
        "The Role {} needs {mode:?} for {slot}",
        template.name
    );
    Ok(artifact)
}

fn primary_mode(prompt: &PromptCard) -> SessionMode {
    match prompt.executor {
        BuiltinNode::Implement => SessionMode::ApprovedWrite,
        BuiltinNode::Plan | BuiltinNode::Agent => prompt
            .configuration
            .as_ref()
            .and_then(StagePlanConfiguration::from_node)
            .map_or_else(
                || {
                    if prompt.executor == BuiltinNode::Plan {
                        SessionMode::Plan
                    } else {
                        SessionMode::ReadOnly
                    }
                },
                |stage| stage.mode(),
            ),
        _ => SessionMode::ReadOnly,
    }
}

fn role_configuration(role: &PublishedPackageArtifact) -> NodeConfiguration {
    NodeConfiguration::AgentRole {
        role: role.name.clone(),
        model: None,
        requirements: Default::default(),
    }
}

fn literal(text: &str) -> String {
    text.replace("{{", r"\{{")
}

fn success_port(kind: BuiltinNode) -> &'static str {
    match kind {
        BuiltinNode::Plan => "planned",
        BuiltinNode::Implement => "implemented",
        BuiltinNode::ReviewPlan
        | BuiltinNode::ReviewCandidate
        | BuiltinNode::ReviewDiffCandidate => "reviewed",
        _ => "completed",
    }
}

fn structured(prompt: &PromptCard) -> bool {
    prompt.routes.as_slice() != ["concluido"]
        || !prompt.delegates.is_empty()
        || prompt
            .outputs
            .values()
            .any(|output| output.source.is_none())
        || prompt.output_schema.is_some()
}

pub(super) fn validate_shape(prompt: &PromptCard) -> Result<()> {
    ensure!(
        !prompt.text.trim().is_empty() && prompt.text.len() <= 24 * 1024,
        "Enter the Prompt text (up to 24 KiB)"
    );
    ensure!(
        matches!(
            prompt.executor,
            BuiltinNode::Agent
                | BuiltinNode::Plan
                | BuiltinNode::ReviewPlan
                | BuiltinNode::Implement
                | BuiltinNode::ReviewCandidate
                | BuiltinNode::ReviewDiffCandidate
        ),
        "Choose a Prompt executor"
    );
    ensure!(
        prompt.inputs.len() <= 32
            && prompt.outputs.len() <= 32
            && (1..=12).contains(&prompt.routes.len())
            && prompt.delegates.len() <= 16,
        "A Prompt supports 32 data ports per direction, 12 routes and 16 invited Roles"
    );
    ensure!(
        prompt
            .inputs
            .keys()
            .all(|name| name != "ready" && valid_identifier(name)),
        "Name each Prompt input with a unique identifier other than ready"
    );
    let reserved = ["falhou", "decision", "outcome", "evidence"];
    let mut outputs: BTreeSet<&str> = reserved.into_iter().collect();
    for name in prompt.routes.iter().chain(prompt.outputs.keys()) {
        ensure!(
            valid_identifier(name) && outputs.insert(name),
            "Choose a unique Prompt output or route identifier: {name}"
        );
    }
    let mut aliases = BTreeSet::new();
    for call in &prompt.delegates {
        ensure!(
            valid_identifier(&call.alias) && call.alias != "input" && aliases.insert(&call.alias),
            "Choose a unique alias for each invited Role"
        );
    }
    Ok(())
}

fn projected_schema(
    prompt: &PromptCard,
    projected: &BTreeMap<String, ArtifactKind>,
) -> Result<Value> {
    let mut properties = Map::new();
    for (name, kind) in projected {
        let json_type = match kind {
            ArtifactKind::JsonObject => "object",
            ArtifactKind::JsonList => "array",
            ArtifactKind::RenderedText => "string",
            _ => bail!("Prompt field {name} requires a typed native source"),
        };
        let mut schema = json!({"type":json_type});
        if *kind == ArtifactKind::JsonList {
            schema["maxItems"] = json!(crate::workflows::iteration::MAX_FOR_EACH_ITEMS);
        }
        properties.insert(name.clone(), schema);
    }
    let typed = json!({
        "type":"object", "properties":properties,
        "required":projected.keys().collect::<Vec<_>>(), "additionalProperties":false
    });
    Ok(match &prompt.output_schema {
        Some(schema) => json!({"allOf":[typed,schema]}),
        None => typed,
    })
}

fn execution_text(prompt: &PromptCard, output_schema: &Value) -> String {
    let mut text = prompt.text.clone();
    if structured(prompt) {
        let aliases = prompt
            .delegates
            .iter()
            .map(|call| call.alias.as_str())
            .collect::<Vec<_>>();
        text.push_str(&format!(
            "\n\nReturn one JSON object without Markdown fences. Use keys route, reason, outputs and delegations. \
             Choose route from {}. Explain the route in reason. Supply only the declared outputs conforming to {}. \
             Set delegations to an array of objects with role and focus. Select role only from {}; use [] when no additional role is needed.",
            json!(prompt.routes), output_schema, json!(aliases)
        ));
    }
    text
}

fn delegate_subject(prompt: &PromptCard) -> (BuiltinNode, Option<(&str, &str)>, &'static str) {
    for (kind, executor, port) in [
        (
            ArtifactKind::Candidate,
            BuiltinNode::ReviewCandidate,
            "candidate",
        ),
        (
            ArtifactKind::GitInspection,
            BuiltinNode::ReviewDiffCandidate,
            "git",
        ),
        (ArtifactKind::Plan, BuiltinNode::ReviewPlan, "plan"),
    ] {
        if let Some((name, _)) = prompt.inputs.iter().find(|(_, input)| input.kind == kind) {
            return (
                executor,
                Some((name, port)),
                if executor == BuiltinNode::ReviewPlan {
                    "review"
                } else {
                    "report"
                },
            );
        }
    }
    (BuiltinNode::Agent, None, "response")
}

pub(super) fn lower(
    name: &str,
    prompt: &PromptCard,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
) -> Result<LoweredPrompt> {
    validate_shape(prompt)?;
    let uses_response = structured(prompt);
    ensure!(
        !uses_response || prompt.executor == BuiltinNode::Agent,
        "Use the Agent executor for structured decisions and invited Roles. Native starters expose their verified outputs."
    );
    let main_role = role(dependencies, "role", primary_mode(prompt))?;
    ensure!(
        dependencies.len() == 1 + prompt.delegates.len(),
        "Prompt references must match its executing and invited Roles"
    );
    let mut graph = PromptGraph::new(name);
    let mut entry_ports = vec![Port {
        name: "ready".into(),
        value: PortType::Control,
    }];
    entry_ports.extend(prompt.inputs.iter().map(|(name, input)| Port {
        name: name.clone(),
        value: PortType::Data(input.kind),
    }));
    graph.node(
        ENTRY,
        BuiltinNode::Start,
        NodeConfiguration::SubflowEntry {
            outputs: entry_ports.clone(),
        },
    )?;
    graph.node(FAILURE, BuiltinNode::Join, NodeConfiguration::None)?;
    let start = endpoint(ENTRY, "ready");
    let inputs = entry_ports
        .into_iter()
        .map(|port| (port.name.clone(), endpoint(ENTRY, &port.name)))
        .collect();

    let projected = prompt
        .outputs
        .iter()
        .filter(|(_, output)| output.source.is_none())
        .map(|(name, output)| (name.clone(), output.kind))
        .collect();
    let output_schema = projected_schema(prompt, &projected)?;
    let mut configuration = prompt
        .configuration
        .clone()
        .unwrap_or_else(|| role_configuration(main_role));
    let NodeConfiguration::AgentRole { requirements, .. } = &mut configuration else {
        bail!("Prompt execution needs an Agent Role configuration");
    };
    ensure!(
        requirements.bound_role.is_none(),
        "Select the executing Role using the Prompt role reference"
    );
    let reuse = requirements.review_reuse;
    let text = execution_text(prompt, &output_schema);
    let default_ready = if prompt.executor == BuiltinNode::Agent {
        graph.text("instructions", literal(&text), start.clone())?;
        let context_inputs: Vec<_> = prompt
            .inputs
            .iter()
            .filter(|(_, input)| {
                input.target.is_none()
                    || input.target.as_deref() == Some("prompt")
                    || (reuse && input.target.as_deref() == Some("review_subject"))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        if reuse || !context_inputs.is_empty() {
            graph.step(
                "context",
                BuiltinNode::CollectEvidence,
                NodeConfiguration::None,
                endpoint("instructions", "completed"),
            )?;
            graph.wire(endpoint("instructions", "text"), "context", "evidence");
            for input in context_inputs {
                graph.wire(endpoint(ENTRY, input), "context", "evidence");
            }
            graph.failure("context");
            endpoint("context", "completed")
        } else {
            endpoint("instructions", "completed")
        }
    } else {
        requirements.prompt_text = Some(text);
        start
    };

    graph.step(EXECUTE, prompt.executor, configuration, default_ready)?;
    graph.dependencies.insert(EXECUTE.into(), main_role.clone());
    graph.failure(EXECUTE);
    if prompt.executor == BuiltinNode::Agent {
        let source = if graph.pins.contains_key("context") {
            "context"
        } else {
            "instructions"
        };
        graph.wire(endpoint(source, "text"), EXECUTE, "prompt");
        if reuse {
            graph.wire(endpoint("context", "evidence"), EXECUTE, "review_subject");
        }
    }
    for (name, input) in &prompt.inputs {
        let Some(target) = &input.target else {
            ensure!(
                prompt.executor == BuiltinNode::Agent,
                "Map input {name} to a typed port on the native executor"
            );
            continue;
        };
        if prompt.executor == BuiltinNode::Agent
            && (target == "prompt" || (reuse && target == "review_subject"))
        {
            continue;
        }
        graph.wire(endpoint(ENTRY, name), EXECUTE, target);
    }

    let mut outputs = BTreeMap::from([("falhou".into(), endpoint(FAILURE, "joined"))]);
    let main_ready = endpoint(EXECUTE, success_port(prompt.executor));
    if uses_response {
        graph.step(
            "decision",
            BuiltinNode::PromptDecision,
            NodeConfiguration::PromptDecision(PromptDecisionConfiguration {
                routes: prompt.routes.clone(),
                delegates: prompt
                    .delegates
                    .iter()
                    .map(|call| (call.alias.clone(), call.required))
                    .collect(),
                output_schema,
            }),
            main_ready,
        )?;
        graph.wire(endpoint(EXECUTE, "response"), "decision", "response");
        graph.failure("decision");
        outputs.insert("decision".into(), endpoint("decision", "decision"));

        let ready = endpoint("decision", "completed");
        if !prompt.delegates.is_empty() {
            graph.step(
                "calls",
                BuiltinNode::CollectEvidence,
                NodeConfiguration::None,
                ready.clone(),
            )?;
            graph.failure("calls");
            let (executor, subject, evidence_port) = delegate_subject(prompt);
            let mut invited_versions = BTreeSet::from([&main_role.version]);
            for call in &prompt.delegates {
                let stage = format!("delegate-{}", call.alias);
                let role = role(dependencies, &stage, SessionMode::ReadOnly)?;
                ensure!(
                    invited_versions.insert(&role.version),
                    "Invited Role {} needs an independent revision used once in this Prompt",
                    role.name
                );
                let branch = format!("select-{}", call.alias);
                let selection = format!("/selected/{}", call.alias);
                graph.condition(
                    &branch,
                    ready.clone(),
                    endpoint("decision", "decision"),
                    ConditionPredicate::Exists {
                        pointer: format!("/evidence{selection}"),
                    },
                )?;
                let fields = format!("focus-{}", call.alias);
                graph.field(
                    &fields,
                    "focus",
                    selection,
                    endpoint("decision", "decision"),
                    endpoint(&branch, "matched"),
                )?;
                let brief = format!("brief-{}", call.alias);
                graph.text(
                    &brief,
                    "Review the pinned subject using the supplied evidence.\nFocus of the invited review:\n{{focus}}".into(),
                    endpoint(&fields, "completed"),
                )?;
                graph.wire(endpoint(&fields, "object"), &brief, "values");
                let mut called = endpoint(&brief, "completed");
                let mut brief_output = endpoint(&brief, "text");
                if !prompt.inputs.is_empty() {
                    let context = format!("context-{}", call.alias);
                    graph.step(
                        &context,
                        BuiltinNode::CollectEvidence,
                        NodeConfiguration::None,
                        called,
                    )?;
                    graph.wire(brief_output, &context, "evidence");
                    for input in prompt.inputs.keys() {
                        graph.wire(endpoint(ENTRY, input), &context, "evidence");
                    }
                    graph.failure(&context);
                    called = endpoint(&context, "completed");
                    brief_output = endpoint(&context, "text");
                }
                let target = if executor == BuiltinNode::Agent {
                    "prompt"
                } else {
                    "brief"
                };
                let mut configuration = role_configuration(role);
                if let NodeConfiguration::AgentRole { requirements, .. } = &mut configuration {
                    requirements.prompt_delegate = true;
                    if executor != BuiltinNode::Agent {
                        requirements.prompt_text = Some(
                            "Review the pinned subject using the supplied focus and evidence."
                                .into(),
                        );
                    }
                }
                graph.step(&stage, executor, configuration, called)?;
                graph.wire(brief_output, &stage, target);
                if let Some((name, port)) = subject {
                    graph.wire(endpoint(ENTRY, name), &stage, port);
                }
                graph.dependencies.insert(stage.clone(), role.clone());
                graph.wire(endpoint(&stage, evidence_port), "calls", "evidence");
            }
            outputs.insert("evidence".into(), endpoint("calls", "evidence"));
        }
        let settle = if prompt.delegates.is_empty() {
            ready
        } else {
            endpoint("calls", "completed")
        };
        graph.step(
            "outcome",
            BuiltinNode::PromptOutcome,
            NodeConfiguration::PromptOutcome(PromptOutcomeConfiguration { outputs: projected }),
            settle,
        )?;
        graph.wire(endpoint("decision", "decision"), "outcome", "decision");
        if !prompt.delegates.is_empty() {
            graph.wire(endpoint("calls", "evidence"), "outcome", "evidence");
        }
        graph.failure("outcome");
        outputs.insert("outcome".into(), endpoint("outcome", "outcome"));
        for route in &prompt.routes {
            let stage = format!("route-{route}");
            graph.condition(
                &stage,
                endpoint("outcome", "completed"),
                endpoint("outcome", "outcome"),
                ConditionPredicate::Equals {
                    pointer: "/evidence/route".into(),
                    value: json!(route),
                },
            )?;
            outputs.insert(route.clone(), endpoint(&stage, "matched"));
        }
    } else {
        outputs.insert(prompt.routes[0].clone(), main_ready);
    }

    for (name, output) in &prompt.outputs {
        let source = match output.source.as_deref() {
            Some(source) => match source.split_once('.') {
                Some(("input", port)) => endpoint(ENTRY, port),
                Some((alias, port)) => endpoint(&format!("delegate-{alias}"), port),
                None => endpoint(EXECUTE, source),
            },
            None => endpoint("outcome", &format!("value-{name}")),
        };
        let value = if source.node == ENTRY {
            prompt.inputs.get(&source.port).map(|input| input.kind)
        } else {
            output_is(
                &graph.pins,
                &source.node,
                &source.port,
                PortType::Data(output.kind),
            )
            .then_some(output.kind)
        };
        ensure!(
            value == Some(output.kind),
            "Output {name} needs a source with its declared data type"
        );
        outputs.insert(name.clone(), source);
    }
    Ok(LoweredPrompt {
        graph: graph.graph,
        pins: graph.pins,
        dependencies: graph.dependencies,
        inputs,
        outputs,
    })
}
