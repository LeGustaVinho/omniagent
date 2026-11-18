use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::{
    providers::SessionMode,
    workflows::{
        ArtifactKind, BuiltinNode, Endpoint, NodeConfiguration, NodeDefinition, NodeInstance, Port,
        PortType, Wire, WorkflowGraph, builtin_registry,
        registry::{configure_failure_join, configure_structured_agent},
        role_templates::{
            ConversationTemplate, TemplateReviewPolicy, validate_conversation_template,
        },
        stage_requirements::StageRequirementConfiguration,
        structured_agent::StructuredAgentConfiguration,
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
        }
    }

    fn node(
        &mut self,
        id: &str,
        kind: BuiltinNode,
        configuration: NodeConfiguration,
    ) -> Result<()> {
        let mut definition = self
            .registry
            .get(&kind)
            .context("The Prompt node is unavailable")?
            .clone();
        if matches!(configuration, NodeConfiguration::JoinFailures) {
            configure_failure_join(&mut definition);
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

pub(super) fn validate_shape(prompt: &PromptCard) -> Result<()> {
    ensure!(
        !prompt.text.trim().is_empty() && prompt.text.len() <= 24 * 1024,
        "Enter the Prompt text (up to 24 KiB)"
    );
    ensure!(
        prompt.schema_contract && prompt.executor == BuiltinNode::Agent,
        "Use a typed AI Node Agent with Input and Output Schemas"
    );
    ensure!(
        prompt.delegates.is_empty(),
        "Configure typed Prompt review using its Role"
    );
    Ok(())
}

pub(super) fn artifact_schema(artifact: &PublishedPackageArtifact) -> Result<Value> {
    let PackageArtifactBody::Schema { fields } = &artifact.body else {
        bail!("Select a Schema revision");
    };
    crate::workflows::schema::object(fields)
}

fn effective_role(
    slot: &str,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
    overrides: &BTreeMap<String, Value>,
) -> Result<ConversationTemplate> {
    let artifact = dependencies
        .get(slot)
        .with_context(|| format!("Choose a Role for {slot}"))?;
    let PackageArtifactBody::Role { template } = &artifact.body else {
        bail!("The reference {slot} must select a Role revision");
    };
    let mut value = serde_json::to_value(template)?;
    let fields = value
        .as_object_mut()
        .context("The Role needs an object configuration")?;
    for (key, value) in overrides {
        ensure!(
            matches!(
                key.as_str(),
                "name"
                    | "role"
                    | "model"
                    | "effort"
                    | "instructions"
                    | "toolScope"
                    | "completionContract"
                    | "reviewPolicy"
                    | "requirements"
            ),
            "Unknown Role override {key}"
        );
        fields.insert(key.clone(), value.clone());
    }
    let mut effective = serde_json::from_value(value).context("The Role overrides are invalid")?;
    validate_conversation_template(&mut effective)?;
    Ok(effective)
}

fn typed_agent(
    graph: &mut PromptGraph,
    id: &str,
    config: StructuredAgentConfiguration,
    role: &super::PackageArtifactVersion,
) -> Result<()> {
    config.validate()?;
    let write = config.effective_role.tool_scope == SessionMode::ApprovedWrite;
    let input = config.input_schema.clone();
    let output = config.output_schema.clone();
    graph.node(
        id,
        BuiltinNode::Agent,
        NodeConfiguration::AgentRole {
            role: config.effective_role.name.clone(),
            model: Some(config.effective_role.model.clone()),
            requirements: StageRequirementConfiguration {
                bound_role: Some(role.clone()),
                stage: config.effective_role.requirements.clone(),
                structured: Some(config),
                ..Default::default()
            },
        },
    )?;
    let definition = graph.pins.get_mut(id).context("Missing Agent definition")?;
    configure_structured_agent(definition, write);
    definition.input_schemas.insert("input".into(), input);
    definition.output_schemas.insert("result".into(), output);
    Ok(())
}

pub(super) fn lower(
    name: &str,
    prompt: &PromptCard,
    dependencies: &BTreeMap<String, PublishedPackageArtifact>,
) -> Result<LoweredPrompt> {
    validate_shape(prompt)?;
    let input = artifact_schema(
        dependencies
            .get("input_schema")
            .context("Select an Input Schema")?,
    )?;
    let output = artifact_schema(
        dependencies
            .get("output_schema")
            .context("Select an Output Schema")?,
    )?;
    let effective = effective_role("role", dependencies, &prompt.role_overrides)?;
    let policy = effective.review_policy;
    let write = effective.tool_scope == SessionMode::ApprovedWrite;
    let reviewer_role = if policy == TemplateReviewPolicy::CrossReview {
        ensure!(
            dependencies
                .get("reviewer")
                .map(|artifact| &artifact.version)
                != dependencies.get("role").map(|artifact| &artifact.version),
            "Choose a different Role for cross-review"
        );
        Some(effective_role("reviewer", dependencies, &BTreeMap::new())?)
    } else {
        None
    };
    let mut graph = PromptGraph::new(name);
    let object = PortType::Data(ArtifactKind::JsonObject);
    let mut entry_ports = vec![
        Port {
            name: "ready".into(),
            value: PortType::Control,
        },
        Port {
            name: "input".into(),
            value: object,
        },
    ];
    if write {
        for (port, kind) in [
            ("release", ArtifactKind::Release),
            ("worktree", ArtifactKind::Worktree),
            ("plan", ArtifactKind::Plan),
        ] {
            entry_ports.push(Port {
                name: port.into(),
                value: PortType::Data(kind),
            });
        }
    }
    graph.node(
        ENTRY,
        BuiltinNode::Start,
        NodeConfiguration::SubflowEntry {
            outputs: entry_ports.clone(),
        },
    )?;
    graph
        .pins
        .get_mut(ENTRY)
        .context("Missing Prompt entry")?
        .output_schemas
        .insert("input".into(), input.clone());
    let inputs = entry_ports
        .into_iter()
        .map(|port| (port.name.clone(), endpoint(ENTRY, &port.name)))
        .collect();
    graph.node(
        FAILURE,
        BuiltinNode::JoinResults,
        NodeConfiguration::JoinFailures,
    )?;
    typed_agent(
        &mut graph,
        EXECUTE,
        StructuredAgentConfiguration {
            prompt: prompt.text.clone(),
            input_schema: input,
            output_schema: output.clone(),
            effective_role: effective,
            reviewer_role: reviewer_role.clone(),
            review_verdict: false,
        },
        &dependencies["role"].version,
    )?;
    graph.wire(endpoint(ENTRY, "ready"), EXECUTE, "ready");
    graph.wire(endpoint(ENTRY, "input"), EXECUTE, "input");
    if write {
        for port in ["release", "worktree", "plan"] {
            graph.wire(endpoint(ENTRY, port), EXECUTE, port);
        }
    }
    graph.failure(EXECUTE);
    let candidate = endpoint(EXECUTE, "result");
    let completion = endpoint(EXECUTE, "completed");
    let (completed, result) = match policy {
        TemplateReviewPolicy::Independent => (completion, candidate),
        TemplateReviewPolicy::HumanApproval => {
            let approval = serde_json::from_value(json!({
                "kind": "human", "interaction": "approval",
                "message": "Approve this agent result?", "fields": []
            }))?;
            graph.step("approval", BuiltinNode::Human, approval, completion)?;
            graph.wire(candidate, "approval", "input");
            for port in ["rejected", "expired", "failed"] {
                graph.wire(endpoint("approval", port), FAILURE, "branches");
            }
            (
                endpoint("approval", "approved"),
                endpoint("approval", "approved_result"),
            )
        }
        TemplateReviewPolicy::CrossReview => {
            let reviewer = reviewer_role.context("Choose a reviewer Role")?;
            ensure!(
                reviewer.tool_scope == SessionMode::ReadOnly
                    && reviewer.review_policy == TemplateReviewPolicy::Independent,
                "The reviewer needs an independent read-only Role"
            );
            typed_agent(&mut graph, "reviewer", StructuredAgentConfiguration {
                prompt: "Review this result against your Role instructions. Approve only if it meets the requirements. Result: {{input}}".into(),
                input_schema: output.clone(),
                output_schema: json!({"type":"object", "properties":{"approved":{"type":"boolean"}, "reason":{"type":"string"}}, "required":["approved"], "additionalProperties":false}),
                effective_role: reviewer,
                reviewer_role: None,
                review_verdict: true,
            }, &dependencies["reviewer"].version)?;
            graph.wire(completion, "reviewer", "ready");
            graph.wire(candidate.clone(), "reviewer", "input");
            graph.failure("reviewer");
            graph.step(
                "return",
                BuiltinNode::End,
                NodeConfiguration::End,
                endpoint("reviewer", "completed"),
            )?;
            graph.wire(candidate, "return", "result");
            let end = graph
                .pins
                .get_mut("return")
                .context("Missing review return")?;
            end.input_schemas.insert("result".into(), output.clone());
            end.output_schemas.insert("result".into(), output);
            (
                endpoint("return", "completed"),
                endpoint("return", "result"),
            )
        }
    };
    Ok(LoweredPrompt {
        graph: graph.graph,
        pins: graph.pins,
        dependencies: BTreeMap::new(),
        inputs,
        outputs: BTreeMap::from([
            ("completed".into(), completed),
            ("failed".into(), endpoint(FAILURE, "joined")),
            ("result".into(), result),
        ]),
    })
}
