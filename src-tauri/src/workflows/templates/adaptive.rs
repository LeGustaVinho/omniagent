use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::{
    jobs::JobRole,
    model_catalog::ModelSelection,
    providers::{Provider, SessionMode},
};

use super::{
    super::{
        ConditionPredicate, RepeatFeedback, RepeatUntilConfiguration,
        authoring::package::{PromptCard, PromptDelegate, PromptInput, PromptOutput},
        builtin_registry,
        role_templates::{ConversationTemplate, TemplateReviewPolicy},
    },
    *,
};

use BuiltinNode::*;

const TASK: &str = "Task delivery — Adaptive";
const DIFF: &str = "Review diff — Adaptive";
const REPAIR: &str = "Fix failed checks — Adaptive";
const LIBRARY: &str = "Delivery library — Adaptive";
const PLANNING: &str = "Super Pipeline — Planning — Adaptive";
const DELIVERY: &str = "Super Pipeline — Delivery — Adaptive";
const AUDIT: &str = "Super Pipeline — Audit — Adaptive";
const COMPLETED: &str = "concluido";
const FAILED: &str = "falhou";

pub(super) fn catalog() -> Vec<WorkflowFactoryEntry> {
    use WorkflowFactoryTemplate::*;
    [
        (
            TaskDeliveryAdaptive,
            TASK,
            vec![TASK, "Plan the delivery", "Implement and assess", "Deliver"],
        ),
        (ReviewDiffAdaptive, DIFF, vec![DIFF]),
        (
            FixFailedChecksAdaptive,
            REPAIR,
            vec![REPAIR, "Diagnose, repair and verify"],
        ),
        (
            DeliveryLibraryAdaptive,
            LIBRARY,
            vec![
                "Plan",
                "Implement",
                "Review",
                "Build",
                "Lint",
                "Test",
                "Review and test",
            ],
        ),
        (
            SuperAiDevelopmentPlanningAdaptive,
            PLANNING,
            vec![
                PLANNING,
                super_pipeline::MASTER_PLAN,
                super_pipeline::SPECIFICATION,
                super_pipeline::BACKLOG,
            ],
        ),
        (
            SuperAiDevelopmentDeliveryAdaptive,
            DELIVERY,
            vec![
                DELIVERY,
                super_pipeline::MASTER_PLAN,
                super_pipeline::SPECIFICATION,
                super_pipeline::BACKLOG,
                super_pipeline::DELIVERY_MEMBER,
                super_pipeline::DELIVERY_REVIEW,
                super_pipeline::PULL_REQUEST_ATTEMPT,
            ],
        ),
        (
            SuperAiDevelopmentAuditAdaptive,
            AUDIT,
            vec![
                AUDIT,
                super_pipeline::MASTER_PLAN,
                super_pipeline::SPECIFICATION,
                super_pipeline::BACKLOG,
                super_pipeline::DELIVERY_MEMBER,
                super_pipeline::DELIVERY_REVIEW,
                super_pipeline::PULL_REQUEST_ATTEMPT,
                super_pipeline::AUDIT_ROUND,
            ],
        ),
    ]
    .into_iter()
    .map(|(id, name, members)| WorkflowFactoryEntry {
        id,
        name,
        members,
        revision: FACTORY_REVISION,
    })
    .collect()
}

pub(super) fn package(template: WorkflowFactoryTemplate) -> Result<WorkflowPackage> {
    use WorkflowFactoryTemplate::*;
    match template {
        TaskDeliveryAdaptive => task(),
        ReviewDiffAdaptive => review_diff(),
        FixFailedChecksAdaptive => fix_checks(),
        DeliveryLibraryAdaptive => library(),
        SuperAiDevelopmentPlanningAdaptive => {
            super_package(super_pipeline::Scope::Planning, PLANNING)
        }
        SuperAiDevelopmentDeliveryAdaptive => {
            super_package(super_pipeline::Scope::Delivery, DELIVERY)
        }
        SuperAiDevelopmentAuditAdaptive => super_package(super_pipeline::Scope::Audit, AUDIT),
        _ => bail!("Select an Adaptive factory template"),
    }
}

fn draft_reference(key: &str) -> PackageReference {
    PackageReference::Draft { key: key.into() }
}

fn role_member(
    key: &str,
    name: &str,
    instructions: &str,
    executor: BuiltinNode,
    configuration: Option<&NodeConfiguration>,
    destination: PackageDestination,
) -> PackageMember {
    let configured_model = match configuration {
        Some(NodeConfiguration::AgentRole { model, .. }) => model.clone(),
        _ => None,
    };
    let tool_scope = match executor {
        Implement => SessionMode::ApprovedWrite,
        _ => SessionMode::ReadOnly,
    };
    let template = ConversationTemplate {
        name: name.into(),
        role: JobRole::Pipeline,
        model: configured_model.unwrap_or_else(|| ModelSelection {
            provider: Provider::Codex,
            profile_id: "desktop-direct:codex".into(),
            model_id: "gpt-6.1-sol".into(),
        }),
        effort: None,
        instructions: instructions.into(),
        tool_scope,
        completion_contract: "Return the required result and identify the evidence used.".into(),
        review_policy: TemplateReviewPolicy::Independent,
        requirements: Default::default(),
    };
    member(
        key,
        name,
        destination,
        PackageArtifactBody::Role { template },
    )
}

fn graph(workflow: &mut PackageMember) -> Result<&mut WorkflowGraph> {
    match &mut workflow.body {
        PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } => {
            Ok(graph)
        }
        _ => bail!("Adaptive template member {} has no graph", workflow.key),
    }
}

fn rename_root(package: &mut WorkflowPackage, name: &str) -> Result<()> {
    package.name = name.into();
    let root = package
        .members
        .first_mut()
        .context("A factory needs its root member")?;
    root.name = name.into();
    graph(root)?.name = name.into();
    Ok(())
}

fn card(
    members: &mut Vec<PackageMember>,
    owner: &mut PackageMember,
    id: &str,
    name: &str,
    role_name: &str,
    prompt: PromptCard,
) {
    let key = format!("{}-{id}-prompt", owner.key);
    add_card(members, &key, name, role_name, owner.destination, prompt);
    owner.references.insert(id.into(), draft_reference(&key));
}

fn add_card(
    members: &mut Vec<PackageMember>,
    key: &str,
    name: &str,
    role_name: &str,
    destination: PackageDestination,
    prompt: PromptCard,
) {
    let role_key = format!("{key}-role");
    members.push(role_member(
        &role_key,
        role_name,
        "Work from the pinned task and supplied evidence.",
        prompt.executor,
        prompt.configuration.as_ref(),
        destination,
    ));
    let mut authored = member(
        key,
        name,
        destination,
        PackageArtifactBody::Prompt { prompt },
    );
    authored
        .references
        .insert("role".into(), draft_reference(&role_key));
    members.push(authored);
}

fn native_ports(kind: BuiltinNode) -> Result<(Vec<Port>, Vec<Port>)> {
    let native = builtin_registry()
        .into_iter()
        .find(|entry| entry.kind == kind)
        .with_context(|| format!("No native definition for {kind:?}"))?;
    Ok((native.inputs, native.outputs))
}

fn native_prompt(
    executor: BuiltinNode,
    text: &str,
    configuration: NodeConfiguration,
) -> Result<PromptCard> {
    let (inputs, outputs) = native_ports(executor)?;
    Ok(PromptCard {
        text: text.into(),
        executor,
        inputs: inputs
            .into_iter()
            .filter_map(|port| {
                let PortType::Data(kind) = port.value else {
                    return None;
                };
                Some((
                    port.name.clone(),
                    PromptInput {
                        kind,
                        target: Some(port.name),
                    },
                ))
            })
            .collect(),
        outputs: outputs
            .into_iter()
            .filter_map(|port| {
                let PortType::Data(kind) = port.value else {
                    return None;
                };
                Some((
                    port.name.clone(),
                    PromptOutput {
                        kind,
                        source: Some(port.name),
                    },
                ))
            })
            .collect(),
        routes: vec![COMPLETED.into()],
        delegates: vec![],
        output_schema: None,
        configuration: Some(configuration),
    })
}

fn native_stage(
    members: &mut Vec<PackageMember>,
    owner: &mut PackageMember,
    id: &str,
    name: &str,
    text: &str,
) -> Result<()> {
    let body = graph(owner)?;
    let stage = body
        .nodes
        .iter()
        .find(|stage| stage.id == id)
        .with_context(|| format!("The template lost stage {id}"))?;
    let executor = stage.kind;
    let configuration = stage.configuration.clone();
    let role = stage_role(stage).to_owned();
    let mut prompt = native_prompt(executor, text, configuration)?;
    // Optional native inputs are only visible when used by this factory.
    prompt
        .inputs
        .retain(|port, _| body.wires.iter().any(|wire| wire.to == endpoint(id, port)));
    let (_, outputs) = native_ports(executor)?;
    for output in outputs
        .iter()
        .filter(|port| port.value == PortType::Control)
    {
        redirect(
            owner,
            &endpoint(id, &output.name),
            &endpoint(
                id,
                if output.name == "failed" {
                    FAILED
                } else {
                    COMPLETED
                },
            ),
        )?;
    }
    *graph(owner)?
        .nodes
        .iter_mut()
        .find(|stage| stage.id == id)
        .with_context(|| format!("The template lost stage {id}"))? =
        node(id, Subflow, NodeConfiguration::None);
    card(members, owner, id, name, &role, prompt);
    Ok(())
}

fn stage_role(stage: &NodeInstance) -> &str {
    match &stage.configuration {
        NodeConfiguration::AgentRole { role, .. } => role,
        _ => "Pipeline assistant",
    }
}

fn redirect(owner: &mut PackageMember, from: &Endpoint, to: &Endpoint) -> Result<()> {
    for wire in &mut graph(owner)?.wires {
        if wire.from == *from {
            wire.from = to.clone();
        }
    }
    if let PackageArtifactBody::Subflow { outputs, .. } = &mut owner.body {
        for endpoint in outputs.values_mut() {
            if *endpoint == *from {
                *endpoint = to.clone();
            }
        }
    }
    Ok(())
}

fn subflow(
    key: &str,
    name: &str,
    destination: PackageDestination,
    fields: &[(&str, PortType)],
    outputs: &[(&str, &str, &str)],
) -> PackageMember {
    member(
        key,
        name,
        destination,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: name.into(),
                nodes: vec![node(
                    "start",
                    Start,
                    NodeConfiguration::SubflowEntry {
                        outputs: fields
                            .iter()
                            .map(|(name, value)| Port {
                                name: (*name).into(),
                                value: *value,
                            })
                            .collect(),
                    },
                )],
                wires: vec![],
            },
            inputs: fields
                .iter()
                .map(|(name, _)| ((*name).into(), endpoint("start", name)))
                .collect(),
            outputs: outputs
                .iter()
                .map(|(name, id, port)| ((*name).into(), endpoint(id, port)))
                .collect(),
        },
    )
}

fn group_stages(
    owner: &mut PackageMember,
    group: &mut PackageMember,
    id: &str,
    stages: &[&str],
    bindings: &[(&str, &str, &str)],
) -> Result<()> {
    let PackageArtifactBody::Subflow {
        graph: child,
        inputs,
        outputs,
    } = &mut group.body
    else {
        bail!("A factory group must have a subflow interface");
    };
    let includes = |id: &str| stages.contains(&id);
    let body = graph(owner)?;
    let existing = std::mem::take(&mut body.nodes);
    for stage in existing {
        if includes(&stage.id) {
            child.nodes.push(stage);
        } else {
            body.nodes.push(stage);
        }
    }
    let existing = std::mem::take(&mut body.wires);
    for mut wire in existing {
        match (includes(&wire.from.node), includes(&wire.to.node)) {
            (true, true) => child.wires.push(wire),
            (false, true) => {
                let (input, _, _) = bindings
                    .iter()
                    .find(|(_, node, port)| wire.from == endpoint(node, port))
                    .with_context(|| {
                        format!("Group {id} is missing a binding for {}", wire.from.port)
                    })?;
                wire.from = inputs
                    .get(*input)
                    .context("The group input is not declared")?
                    .clone();
                child.wires.push(wire);
            }
            (true, false) => {
                let (output, _) = outputs
                    .iter()
                    .find(|(_, source)| **source == wire.from)
                    .with_context(|| {
                        format!("Group {id} is missing a port for {}", wire.from.port)
                    })?;
                wire.from = endpoint(id, output);
                body.wires.push(wire);
            }
            (false, false) => body.wires.push(wire),
        }
    }
    body.nodes.push(node(id, Subflow, NodeConfiguration::None));
    body.wires.extend(
        bindings
            .iter()
            .map(|(input, source, port)| wire(source, port, id, input)),
    );
    for stage in stages {
        if let Some(reference) = owner.references.remove(*stage) {
            group.references.insert((*stage).into(), reference);
        }
    }
    owner
        .references
        .insert(id.into(), draft_reference(&group.key));
    Ok(())
}

fn structured(
    text: &str,
    inputs: &[(&str, ArtifactKind)],
    output: Option<(&str, Value)>,
) -> PromptCard {
    let (outputs, output_schema) = output.map_or_else(
        || (BTreeMap::new(), None),
        |(name, schema)| (
            BTreeMap::from([(name.into(), PromptOutput { kind: ArtifactKind::JsonObject, source: None })]),
            Some(json!({ "type":"object", "required":[name], "properties":{ (name):schema }, "additionalProperties":false })),
        ),
    );
    PromptCard {
        text: text.into(),
        executor: Agent,
        inputs: inputs
            .iter()
            .map(|(name, kind)| {
                (
                    (*name).into(),
                    PromptInput {
                        kind: *kind,
                        target: None,
                    },
                )
            })
            .collect(),
        outputs,
        routes: vec![COMPLETED.into()],
        delegates: vec![],
        output_schema,
        configuration: None,
    }
}

struct Specialist {
    alias: &'static str,
    name: &'static str,
    focus: &'static str,
    required: bool,
}

const CANDIDATE_SPECIALISTS: &[Specialist] = &[
    Specialist {
        alias: "correctness",
        name: "Independent correctness reviewer",
        focus: "Check the exact candidate against the acceptance criteria and the verified tests.",
        required: true,
    },
    Specialist {
        alias: "security",
        name: "Candidate security reviewer",
        focus: "Check changed trust boundaries, authorization, privacy, and failure handling.",
        required: false,
    },
    Specialist {
        alias: "performance",
        name: "Candidate performance reviewer",
        focus: "Check changed resource use, latency, and scaling behavior.",
        required: false,
    },
    Specialist {
        alias: "architecture",
        name: "Candidate architecture reviewer",
        focus: "Check integration contracts, dependencies, and fit with the approved design.",
        required: false,
    },
    Specialist {
        alias: "maintainability",
        name: "Candidate maintainability reviewer",
        focus: "Check readability, scope, and changes that will affect maintenance.",
        required: false,
    },
];

fn add_specialists(
    members: &mut Vec<PackageMember>,
    owner: &PackageMember,
    card_id: &str,
    specialists: &[Specialist],
    executor: BuiltinNode,
) -> Result<()> {
    let key = match owner.references.get(card_id) {
        Some(PackageReference::Draft { key }) => key.clone(),
        _ => bail!("The adaptive coordinator has no versioned card"),
    };
    let index = members
        .iter()
        .position(|member| member.key == key)
        .context("Missing adaptive coordinator")?;
    let destination = members[index].destination;
    for specialist in specialists {
        let role_key = format!("{key}-{}", specialist.alias);
        members.push(role_member(
            &role_key,
            specialist.name,
            specialist.focus,
            executor,
            None,
            destination,
        ));
        members[index].references.insert(
            format!("delegate-{}", specialist.alias),
            draft_reference(&role_key),
        );
    }
    let PackageArtifactBody::Prompt { prompt } = &mut members[index].body else {
        bail!("The adaptive coordinator is not a Prompt");
    };
    prompt
        .delegates
        .extend(specialists.iter().map(|specialist| PromptDelegate {
            alias: specialist.alias.into(),
            required: specialist.required,
        }));
    Ok(())
}

fn add_stage(
    owner: &mut PackageMember,
    id: &str,
    kind: BuiltinNode,
    configuration: NodeConfiguration,
    inputs: &[(&str, &str, &str)],
) -> Result<()> {
    let graph = graph(owner)?;
    graph.nodes.push(node(id, kind, configuration));
    graph.wires.extend(
        inputs
            .iter()
            .map(|(source, port, input)| wire(source, port, id, input)),
    );
    Ok(())
}

fn add_prompt_stage(
    members: &mut Vec<PackageMember>,
    owner: &mut PackageMember,
    id: &str,
    title: &str,
    role_name: &str,
    prompt: PromptCard,
    inputs: &[(&str, &str, &str)],
) -> Result<()> {
    add_stage(owner, id, Subflow, NodeConfiguration::None, inputs)?;
    card(members, owner, id, title, role_name, prompt);
    Ok(())
}

fn review_diff() -> Result<WorkflowPackage> {
    let mut package = diff::review_diff();
    rename_root(&mut package, DIFF)?;
    let mut top = package.members.remove(0);
    let mut evaluate = structured(
        "Review the selected frozen diff. Select further reviewers only for risks visible in the change. Include findings and the paths or commits that justify each specialty. The correctness reviewer runs on every diff.",
        &[("git", ArtifactKind::GitInspection)],
        None,
    );
    evaluate.outputs.insert(
        "review".into(),
        PromptOutput {
            kind: ArtifactKind::Review,
            source: Some("correctness.review".into()),
        },
    );
    let root = graph(&mut top)?;
    root.nodes.retain(|stage| stage.id != "review");
    root.wires.retain(|wire| wire.to.node != "review");
    redirect(
        &mut top,
        &endpoint("review", "reviewed"),
        &endpoint("review", COMPLETED),
    )?;
    add_prompt_stage(
        &mut package.members,
        &mut top,
        "review",
        "Assess the diff",
        "Diff review lead",
        evaluate,
        &[("fan-out", "branches", "ready"), ("read-git", "git", "git")],
    )?;
    add_specialists(
        &mut package.members,
        &top,
        "review",
        CANDIDATE_SPECIALISTS,
        ReviewDiffCandidate,
    )?;
    let summary = structured(
        "Consolidate the native diff report and the specialist findings. Distinguish verified checks, actionable findings and any follow-up. Cite the evidence you used and identify the frozen candidate.",
        &[
            ("report", ArtifactKind::DiffReport),
            ("specialists", ArtifactKind::EvidenceSet),
        ],
        Some((
            "summary",
            json!({
                "type":"object", "required":["summary"], "additionalProperties":false,
                "properties":{"summary":{"type":"string","minLength":1,"maxLength":4000}}
            }),
        )),
    );
    add_prompt_stage(
        &mut package.members,
        &mut top,
        "consolidate",
        "Consolidate the findings",
        "Diff report editor",
        summary,
        &[
            ("review", COMPLETED, "ready"),
            ("report", "report", "report"),
            ("review", "evidence", "specialists"),
        ],
    )?;
    add_stage(
        &mut top,
        "summary-text",
        BuildText,
        serde_json::from_value(json!({
            "kind":"build_text", "template":"{{summary}}",
        }))?,
        &[
            ("consolidate", COMPLETED, "ready"),
            ("consolidate", "summary", "values"),
        ],
    )?;
    add_stage(
        &mut top,
        "result",
        SummarizeResult,
        NodeConfiguration::None,
        &[
            ("summary-text", "completed", "ready"),
            ("summary-text", "text", "summary"),
            ("report", "report", "evidence"),
            ("review", "evidence", "evidence"),
        ],
    )?;
    add_failure_summary(
        &mut top,
        "diff-review-failed",
        &[("review", FAILED), ("consolidate", FAILED)],
        "Diff review stopped. Open the selected candidate, reviewer receipts and diagnostic for details.",
    )?;
    package.members.insert(0, top);
    Ok(package)
}

fn library() -> Result<WorkflowPackage> {
    let mut package = delivery_library();
    package.name = LIBRARY.into();
    let mut original_cards = package.members.drain(..3).collect::<Vec<_>>();
    let mut checks = package
        .members
        .pop()
        .context("Missing reusable review and test flow")?;
    for stage in &mut original_cards {
        let PackageArtifactBody::Preset {
            node_kind,
            configuration,
            ..
        } = &stage.body
        else {
            bail!("A delivery library starter is missing its native node");
        };
        let text = match node_kind {
            Plan => {
                "Plan the requested work from the supplied repository context. Include implementation and verification steps."
            }
            Implement => {
                "Implement the pinned plan in the prepared worktree. Verify that the resulting candidate satisfies its acceptance criteria."
            }
            ReviewCandidate => {
                "Review the frozen candidate against the approved plan and checks. Record concrete findings and an independent verdict."
            }
            _ => bail!("Unsupported delivery library starter"),
        };
        let mut prompt = native_prompt(*node_kind, text, configuration.clone())?;
        prompt.inputs.remove("brief");
        let role_name = stage.name.clone();
        add_card(
            &mut package.members,
            &stage.key,
            &stage.name,
            &role_name,
            stage.destination,
            prompt,
        );
    }
    let mut prompt = structured(
        "Assess risk in the frozen candidate. Call the specialists needed for concrete candidate changes and give each an explicit focus.",
        &[("candidate", ArtifactKind::Candidate)],
        None,
    );
    prompt.outputs.insert(
        "review".into(),
        PromptOutput {
            kind: ArtifactKind::Review,
            source: Some("correctness.review".into()),
        },
    );
    let flow = graph(&mut checks)?;
    flow.nodes
        .retain(|stage| !matches!(stage.id.as_str(), "review" | "join"));
    flow.wires
        .retain(|wire| !matches!(wire.to.node.as_str(), "review" | "join"));
    add_prompt_stage(
        &mut package.members,
        &mut checks,
        "review",
        "Assess the candidate",
        "Candidate review lead",
        prompt,
        &[
            ("fan-out", "branches", "ready"),
            ("entry", "candidate", "candidate"),
        ],
    )?;
    add_specialists(
        &mut package.members,
        &checks,
        "review",
        CANDIDATE_SPECIALISTS,
        ReviewCandidate,
    )?;
    add_stage(
        &mut checks,
        "checks-passed",
        Condition,
        NodeConfiguration::Condition {
            predicate: ConditionPredicate::CandidateChecksPassed,
        },
        &[("check", "verified", "ready"), ("check", "checks", "value")],
    )?;
    add_stage(
        &mut checks,
        "verified",
        SetFields,
        flag("passed")?,
        &[("checks-passed", "matched", "ready")],
    )?;
    add_stage(
        &mut checks,
        "evidence",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("entry", "ready", "ready"),
            ("review", "outcome", "evidence"),
            ("verified", "object", "evidence"),
        ],
    )?;
    add_stage(
        &mut checks,
        "accept",
        QualityGate,
        quality(&[
            (
                "all selected reviews approved",
                "/0/value/evidence/approved",
                json!(true),
            ),
            (
                "candidate checks passed",
                "/1/value/evidence/passed",
                json!(true),
            ),
        ]),
        &[
            ("evidence", "completed", "ready"),
            ("evidence", "evidence", "evidence"),
        ],
    )?;
    add_stage(
        &mut checks,
        "failed",
        Join,
        NodeConfiguration::None,
        &[
            ("review", FAILED, "branches"),
            ("accept", "blocked", "branches"),
            ("accept", "revise", "branches"),
        ],
    )?;
    if let PackageArtifactBody::Subflow { outputs, .. } = &mut checks.body {
        outputs.insert("reviews".into(), endpoint("review", "evidence"));
        outputs.insert("complete".into(), endpoint("accept", "passed"));
        outputs.insert("falhou".into(), endpoint("failed", "joined"));
    }
    package.members.push(checks);
    Ok(package)
}

fn task() -> Result<WorkflowPackage> {
    use ArtifactKind::{Candidate, Checks, EvidenceSet, Review, Worktree};
    use PortType::{Control, Data};
    let mut package = task_delivery();
    rename_root(&mut package, TASK)?;
    let mut top = package.members.remove(0);
    let root = graph(&mut top)?;
    root.nodes.retain(|stage| {
        !matches!(
            stage.id.as_str(),
            "implement" | "review-candidate" | "verify-candidate"
        )
    });
    root.wires.retain(|wire| {
        !matches!(
            wire.to.node.as_str(),
            "implement" | "review-candidate" | "verify-candidate"
        )
    });
    redirect(
        &mut top,
        &endpoint("verify-candidate", "verified"),
        &endpoint("implementation", "completed"),
    )?;
    redirect(
        &mut top,
        &endpoint("verify-candidate", "checks"),
        &endpoint("implementation", "checks"),
    )?;
    redirect(
        &mut top,
        &endpoint("implement", "candidate"),
        &endpoint("implementation", "candidate"),
    )?;
    redirect(
        &mut top,
        &endpoint("review-candidate", "review"),
        &endpoint("implementation", "review"),
    )?;
    add_stage(
        &mut top,
        "initial-candidate-context",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("prepare", "prepared", "ready"),
            ("plan", "plan", "evidence"),
        ],
    )?;
    add_stage(
        &mut top,
        "implementation",
        RepeatUntil,
        NodeConfiguration::RepeatUntil(RepeatUntilConfiguration {
            max_iterations: 3,
            ready_input: "ready".into(),
            completion_output: "done".into(),
            evidence_output: "quality".into(),
            until: eq("/evidence/decision", json!("passed")),
            feedback: Some(RepeatFeedback {
                input: "feedback".into(),
                output: "feedback".into(),
            }),
            exports: ["candidate", "review", "checks"].map(str::to_owned).into(),
            delivery_attempt_budget: None,
        }),
        &[
            ("initial-candidate-context", "completed", "ready"),
            ("initial-candidate-context", "evidence", "feedback"),
            ("plan", "plan", "plan"),
            ("prepare", "worktree", "worktree"),
            ("prepare", "release", "release"),
        ],
    )?;
    top.references.insert(
        "implementation".into(),
        draft_reference("implementation-round"),
    );
    let fields = [
        ("ready", Control),
        ("plan", Data(ArtifactKind::Plan)),
        ("worktree", Data(Worktree)),
        ("release", Data(ArtifactKind::Release)),
        ("feedback", Data(EvidenceSet)),
    ];
    let mut round = subflow(
        "implementation-round",
        "Implement and assess",
        PackageDestination::Project,
        &fields,
        &[
            ("done", "evaluation-complete", "joined"),
            ("quality", "candidate-gate", "decision"),
            ("feedback", "attempt-evidence", "evidence"),
            ("candidate", "implement", "candidate"),
            ("review", "assess", "review"),
            ("checks", "verify-candidate", "checks"),
        ],
    );
    let implementation = native_prompt(
        Implement,
        "Implement the pinned plan in the prepared worktree. Address the findings recorded for the previous candidate and preserve the approved scope.",
        agent_role("implementer"),
    )?;
    add_stage(
        &mut round,
        "implementation-feedback",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("start", "ready", "ready"),
            ("start", "feedback", "evidence"),
        ],
    )?;
    add_prompt_stage(
        &mut package.members,
        &mut round,
        "implement",
        "Implement",
        "Implementer",
        implementation,
        &[
            ("implementation-feedback", "completed", "ready"),
            ("start", "plan", "plan"),
            ("start", "release", "release"),
            ("start", "worktree", "worktree"),
            ("implementation-feedback", "text", "brief"),
        ],
    )?;
    add_stage(
        &mut round,
        "verify-candidate",
        VerifyCandidate,
        NodeConfiguration::RequiredChecks {
            names: Vec::new(),
            parallel_groups: Vec::new(),
        },
        &[
            ("implement", COMPLETED, "ready"),
            ("implement", "candidate", "candidate"),
        ],
    )?;
    let mut assess = structured(
        "Evaluate the exact candidate against the pinned plan, recorded checks and findings. Use corrigir for a concrete defect, revisar to request further specialists, or entregar when the verified result is ready for the delivery gate. Name the risk each invited role should inspect. The independent correctness review always runs.",
        &[
            ("candidate", Candidate),
            ("plan", ArtifactKind::Plan),
            ("checks", Checks),
            ("feedback", EvidenceSet),
        ],
        None,
    );
    assess.routes = ["revisar", "corrigir", "entregar"]
        .map(str::to_owned)
        .into();
    assess.outputs.insert(
        "review".into(),
        PromptOutput {
            kind: Review,
            source: Some("correctness.review".into()),
        },
    );
    add_prompt_stage(
        &mut package.members,
        &mut round,
        "assess",
        "Assess the candidate",
        "Task owner",
        assess,
        &[
            ("verify-candidate", "verified", "ready"),
            ("implement", "candidate", "candidate"),
            ("verify-candidate", "checks", "checks"),
            ("start", "plan", "plan"),
            ("start", "feedback", "feedback"),
        ],
    )?;
    add_specialists(
        &mut package.members,
        &round,
        "assess",
        CANDIDATE_SPECIALISTS,
        ReviewCandidate,
    )?;
    add_stage(
        &mut round,
        "reviewed",
        Join,
        NodeConfiguration::None,
        &[
            ("assess", "revisar", "branches"),
            ("assess", "entregar", "branches"),
        ],
    )?;
    add_stage(
        &mut round,
        "delivery-intent",
        SetFields,
        flag("deliver")?,
        &[("reviewed", "joined", "ready")],
    )?;
    add_stage(
        &mut round,
        "attempt-evidence",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("start", "ready", "ready"),
            ("assess", "outcome", "evidence"),
            ("delivery-intent", "object", "evidence"),
            ("assess", "evidence", "evidence"),
            ("verify-candidate", "checks", "evidence"),
        ],
    )?;
    add_stage(
        &mut round,
        "candidate-gate",
        QualityGate,
        quality(&[
            (
                "selected reviewers",
                "/0/value/evidence/approved",
                json!(true),
            ),
            (
                "delivery requested",
                "/1/value/evidence/deliver",
                json!(true),
            ),
        ]),
        &[
            ("attempt-evidence", "completed", "ready"),
            ("attempt-evidence", "evidence", "evidence"),
        ],
    )?;
    add_stage(
        &mut round,
        "evaluation-complete",
        Join,
        NodeConfiguration::None,
        &[
            ("candidate-gate", "passed", "branches"),
            ("candidate-gate", "revise", "branches"),
            ("candidate-gate", "blocked", "branches"),
        ],
    )?;
    native_stage(
        &mut package.members,
        &mut top,
        "plan",
        "Plan the delivery",
        "Plan the delivery using the project context. State scope, acceptance criteria, verification and the implementation order.",
    )?;
    native_stage(
        &mut package.members,
        &mut top,
        "review-plan",
        "Review the plan",
        "Independently review the pinned plan for completeness, correctness and testability before work is released.",
    )?;
    let mut planning = subflow(
        "plan-delivery",
        "Plan the delivery",
        PackageDestination::Project,
        &[
            ("ready", Control),
            ("document", Data(ArtifactKind::Markdown)),
        ],
        &[
            ("prepared", "prepare", "prepared"),
            ("plan", "plan", "plan"),
            ("worktree", "prepare", "worktree"),
            ("release", "prepare", "release"),
            ("task", "work", "task"),
        ],
    );
    group_stages(
        &mut top,
        &mut planning,
        "plan-delivery",
        &[
            "context",
            "plan",
            "review-plan",
            "work",
            "approve",
            "prepare",
        ],
        &[
            ("ready", "start", "start"),
            ("document", "start", "document"),
        ],
    )?;
    let mut delivery = subflow(
        "deliver-candidate",
        "Deliver",
        PackageDestination::Project,
        &[
            ("ready", Control),
            ("candidate", Data(Candidate)),
            ("review", Data(Review)),
            ("checks", Data(Checks)),
            ("task", Data(ArtifactKind::Task)),
        ],
        &[
            ("complete", "result", "completed"),
            ("delivery", "result", "delivery"),
        ],
    );
    group_stages(
        &mut top,
        &mut delivery,
        "deliver",
        &["accept", "commit", "merge", "result"],
        &[
            ("ready", "implementation", "completed"),
            ("candidate", "implementation", "candidate"),
            ("review", "implementation", "review"),
            ("checks", "implementation", "checks"),
            ("task", "plan-delivery", "task"),
        ],
    )?;
    add_failure_summary(
        &mut top,
        "delivery-failed",
        &[("implementation", "failed")],
        "Delivery stopped before candidate acceptance. Open the candidate review results and failed stages for details.",
    )?;
    package.members.insert(0, top);
    package.members.extend([planning, round, delivery]);
    Ok(package)
}

fn eq(pointer: &str, value: Value) -> ConditionPredicate {
    ConditionPredicate::Equals {
        pointer: pointer.into(),
        value,
    }
}

fn flag(name: &str) -> Result<NodeConfiguration> {
    Ok(serde_json::from_value(json!({
        "kind": "set_fields", "mode": "build", "operations": [
            {"op": "set", "path": format!("/{name}"), "value": {"source":"literal","value":true}},
        ]
    }))?)
}

fn quality(criteria: &[(&str, &str, Value)]) -> NodeConfiguration {
    NodeConfiguration::QualityGate(super::super::data_nodes::QualityGateConfiguration {
        rules: criteria
            .iter()
            .map(|(name, path, value)| ((*name).into(), eq(path, value.clone())))
            .collect(),
    })
}

fn add_failure_summary(
    owner: &mut PackageMember,
    id: &str,
    failures: &[(&str, &str)],
    summary: &str,
) -> Result<()> {
    let failure = failures
        .iter()
        .map(|(node, port)| (*node, *port, "branches"))
        .collect::<Vec<_>>();
    add_stage(owner, id, Join, NodeConfiguration::None, &failure)?;
    let text = format!("{id}-text");
    add_stage(
        owner,
        &text,
        BuildText,
        serde_json::from_value(json!({
            "kind":"build_text", "template":summary,
        }))?,
        &[(id, "joined", "ready")],
    )?;
    add_stage(
        owner,
        &format!("{id}-summary"),
        SummarizeResult,
        NodeConfiguration::None,
        &[(&text, "completed", "ready"), (&text, "text", "summary")],
    )
}

fn fix_checks() -> Result<WorkflowPackage> {
    use ArtifactKind::EvidenceSet;
    use PortType::Data;
    let mut package = repair::fix_failed_checks();
    rename_root(&mut package, REPAIR)?;
    let mut top = package.members.remove(0);
    let mut round = package.members.remove(0);
    round.name = "Diagnose, repair and verify".into();
    graph(&mut round)?.name = round.name.clone();
    let PackageArtifactBody::Subflow {
        graph: body,
        inputs,
        outputs,
    } = &mut round.body
    else {
        bail!("Missing repair round");
    };
    let NodeConfiguration::SubflowEntry { outputs: ports } = &mut body.nodes[0].configuration
    else {
        bail!("Missing repair inputs");
    };
    ports.push(Port {
        name: "feedback".into(),
        value: Data(EvidenceSet),
    });
    inputs.insert("feedback".into(), endpoint("entry", "feedback"));
    outputs.insert("feedback".into(), endpoint("record-checks", "evidence"));
    outputs.insert("checked".into(), endpoint("record-checks", "completed"));
    let root = graph(&mut top)?;
    let repeat = root
        .nodes
        .iter_mut()
        .find(|stage| stage.id == "repair")
        .context("Missing repeat stage")?;
    let NodeConfiguration::RepeatUntil(repeat) = &mut repeat.configuration else {
        bail!("Missing repair guard");
    };
    repeat.feedback = Some(RepeatFeedback {
        input: "feedback".into(),
        output: "feedback".into(),
    });
    root.wires
        .retain(|wire| wire.to != endpoint("repair", "ready"));
    root.wires.extend([
        wire("record-failure", "completed", "repair", "ready"),
        wire("record-failure", "evidence", "repair", "feedback"),
    ]);
    add_stage(
        &mut top,
        "record-failure",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("prepare", "prepared", "ready"),
            ("start", "document", "evidence"),
        ],
    )?;
    let diagnosis = structured(
        "Diagnose the failure from the recorded check evidence. Identify the failure, the likely cause grounded in evidence, and the smallest safe repair. On the first round inspect the report supplied with the request. In later rounds base the diagnosis on the new check result.",
        &[("failure", EvidenceSet), ("plan", ArtifactKind::Plan)],
        Some((
            "diagnosis",
            json!({
                "type":"object", "required":["failure","focus"], "additionalProperties":false,
                "properties":{"failure":{"type":"string","minLength":1,"maxLength":1000},"focus":{"type":"string","minLength":1,"maxLength":2000}}
            }),
        )),
    );
    add_prompt_stage(
        &mut package.members,
        &mut round,
        "diagnose",
        "Diagnose the failure",
        "Failure investigator",
        diagnosis,
        &[
            ("entry", "ready", "ready"),
            ("entry", "feedback", "failure"),
            ("entry", "plan", "plan"),
        ],
    )?;
    let body = graph(&mut round)?;
    body.wires
        .retain(|wire| wire.to != endpoint("implement", "ready"));
    body.wires
        .push(wire("diagnose", COMPLETED, "implement", "ready"));
    add_stage(
        &mut round,
        "repair-focus",
        BuildText,
        serde_json::from_value(json!({
            "kind":"build_text", "template":"Recorded failure: {{failure}}\nRepair focus: {{focus}}",
        }))?,
        &[
            ("diagnose", COMPLETED, "ready"),
            ("diagnose", "diagnosis", "values"),
        ],
    )?;
    graph(&mut round)?
        .wires
        .push(wire("repair-focus", "text", "implement", "brief"));
    add_stage(
        &mut round,
        "record-checks",
        CollectEvidence,
        NodeConfiguration::None,
        &[
            ("verify", "verified", "ready"),
            ("verify", "checks", "evidence"),
        ],
    )?;
    native_stage(
        &mut package.members,
        &mut round,
        "implement",
        "Repair and verify",
        "Repair the recorded failure using the investigator's focus. Preserve the approved scope and leave the candidate ready for the registered checks.",
    )?;
    add_failure_summary(
        &mut top,
        "repair-failed",
        &[("repair", "failed")],
        "The registered checks still require attention. Inspect the recorded diagnoses, implementation attempts and checks.",
    )?;
    package.members.insert(0, top);
    package.members.push(round);
    Ok(package)
}

fn super_package(scope: super_pipeline::Scope, name: &str) -> Result<WorkflowPackage> {
    let mut package = super_pipeline::package(scope)?;
    rename_root(&mut package, name)?;
    let originals = std::mem::take(&mut package.members);
    let mut adaptive = Vec::new();
    for mut original in originals
        .into_iter()
        .filter(|entry| !matches!(entry.body, PackageArtifactBody::Preset { .. }))
    {
        if original.key == "candidate-review" {
            adapt_candidate_round(&mut adaptive, &mut original)?;
        }
        if original.key == "task-delivery" {
            bind_selected_reviews(&mut original)?;
        }
        if original.key == "integration-audit" {
            adapt_audit_round(&mut adaptive, &mut original)?;
        }
        collapse_generators(&mut adaptive, &mut original)?;
        if original.key == "specification-review" {
            adapt_specification_revisions(&mut adaptive, &mut original)?;
        }
        let implementations = graph(&mut original)?
            .nodes
            .iter()
            .filter(|stage| stage.kind == Implement)
            .map(|stage| stage.id.clone())
            .collect::<Vec<_>>();
        for id in implementations {
            let (title, text) = match id.as_str() {
                "write-failing-tests" => (
                    "Write the failing tests",
                    "Write only the approved RED tests. Match the exact paths, test names and expected failures from the test specification.",
                ),
                "implement-green" => (
                    "Implement the task",
                    "Implement the pinned task with the smallest production change that makes its verified RED tests pass. Address the previous findings and any recorded CI failure.",
                ),
                "refactor" => (
                    "Refactor the candidate",
                    "Refactor where the change will improve readability, maintainability or fit. Preserve behavior and the approved tests.",
                ),
                _ => (
                    "Implement",
                    "Implement the authorized work in the prepared worktree and preserve the pinned project scope.",
                ),
            };
            native_stage(&mut adaptive, &mut original, &id, title, text)?;
        }
        package.members.push(original);
    }
    package.members.extend(adaptive);
    Ok(package)
}

fn collapse_generators(members: &mut Vec<PackageMember>, owner: &mut PackageMember) -> Result<()> {
    let stages = graph(owner)?
        .nodes
        .iter()
        .filter_map(|stage| {
            let NodeConfiguration::ValidateOutput(schema) = &stage.configuration else {
                return None;
            };
            Some((stage.id.clone(), schema.schema.clone()))
        })
        .collect::<Vec<_>>();
    for (id, schema) in stages {
        let execute = format!("{id}-agent");
        let instructions = format!("{id}-instructions");
        let context = format!("{id}-prompt");
        let root = graph(owner)?;
        let Some(agent) = root.nodes.iter().find(|candidate| candidate.id == execute) else {
            // Validation may also check the native response of a delegated reviewer.
            continue;
        };
        let name = stage_role(agent).to_owned();
        let configuration = agent.configuration.clone();
        let reuse = matches!(&configuration, NodeConfiguration::AgentRole { requirements, .. } if requirements.review_reuse);
        let instruction = root
            .nodes
            .iter()
            .find(|candidate| candidate.id == instructions)
            .with_context(|| format!("Missing Prompt text for {id}"))?;
        let raw = serde_json::to_value(&instruction.configuration)?;
        let template = raw["template"]
            .as_str()
            .with_context(|| format!("Invalid Prompt text for {id}"))?;
        let prior = root
            .wires
            .iter()
            .find(|wire| wire.to == endpoint(&instructions, "ready"))
            .with_context(|| format!("Missing ready control for {id}"))?
            .from
            .clone();
        root.nodes.retain(|stage| {
            stage.id != execute && stage.id != instructions && (reuse || stage.id != id)
        });
        root.wires.retain(|wire| {
            ![&execute, &instructions, &id].contains(&&wire.to.node)
                && wire.from.node != instructions
        });
        root.wires.push(Wire {
            from: prior,
            to: endpoint(&context, "ready"),
        });
        owner.references.remove(&execute);
        if reuse {
            // The native reuse verifier needs a plain reviewed response with a
            // direct ValidateOutput successor and identical subject evidence.
            let mut prompt = structured(template, &[("context", ArtifactKind::EvidenceSet)], None);
            prompt.configuration = Some(configuration);
            prompt.outputs.insert(
                "response".into(),
                PromptOutput {
                    kind: ArtifactKind::AgentResponse,
                    source: Some("response".into()),
                },
            );
            redirect(
                owner,
                &endpoint(&execute, "failed"),
                &endpoint(&execute, FAILED),
            )?;
            add_prompt_stage(
                members,
                owner,
                &execute,
                &name,
                &name,
                prompt,
                &[
                    (&context, "completed", "ready"),
                    (&context, "evidence", "context"),
                ],
            )?;
            graph(owner)?.wires.extend([
                wire(&execute, COMPLETED, &id, "ready"),
                wire(&execute, "response", &id, "source"),
            ]);
            continue;
        }
        redirect(owner, &endpoint(&execute, "failed"), &endpoint(&id, FAILED))?;
        redirect(owner, &endpoint(&id, "failed"), &endpoint(&id, FAILED))?;
        redirect(
            owner,
            &endpoint(&id, "completed"),
            &endpoint(&id, COMPLETED),
        )?;
        deduplicate_wires(graph(owner)?);
        let task = template
            .split("\n\nUse the other collected entries as evidence.")
            .next()
            .unwrap_or_default();
        let mut prompt = structured(
            task,
            &[("context", ArtifactKind::EvidenceSet)],
            Some(("object", schema)),
        );
        prompt.configuration = Some(configuration);
        let display = match id.as_str() {
            "analyze-repository" => "Discover the repository",
            "analyze-requirements" => "Discover the requirements",
            "analyze-research" => "Review open-source research",
            "write-master-plan" => "Plan and revise",
            "write-backlog" => "Propose the backlog",
            "review-documentation-cleanup" => "Prepare the release",
            "write-remediation-backlog" => "Plan remediation",
            _ => &name,
        };
        add_prompt_stage(
            members,
            owner,
            &id,
            display,
            &name,
            prompt,
            &[
                (&context, "completed", "ready"),
                (&context, "evidence", "context"),
            ],
        )?;
    }
    Ok(())
}

fn deduplicate_wires(graph: &mut WorkflowGraph) {
    let mut connections = std::collections::HashSet::new();
    graph
        .wires
        .retain(|wire| connections.insert((wire.from.clone(), wire.to.clone())));
}

fn choose_object(pointers: &[&str]) -> Result<NodeConfiguration> {
    Ok(serde_json::from_value(json!({
        "kind":"set_fields", "mode":"build", "operations":[{"op":"choose_object","pointers":pointers}],
    }))?)
}

fn adapt_specification_revisions(
    members: &mut Vec<PackageMember>,
    owner: &mut PackageMember,
) -> Result<()> {
    let planner = structured(
        "Choose which approved planning artifacts need revision. Use the previous round's concrete findings and the master plan. Mark only the contracts affected by a finding or by a changed dependency. Keep every unaffected artifact exactly as saved. For example, a changed technical design needs a design revision and needs a test strategy revision only when its test assumptions change. The native cross-review and consistency gates continue to verify the resulting set.",
        &[
            ("feedback", ArtifactKind::EvidenceSet),
            ("context", ArtifactKind::EvidenceSet),
        ],
        Some((
            "selection",
            json!({
                "type":"object", "required":["sdd","bdd","design","strategy"], "additionalProperties":false,
                "properties":{
                    "sdd":{"type":"boolean"}, "bdd":{"type":"boolean"},
                    "design":{"type":"boolean"}, "strategy":{"type":"boolean"}
                }
            }),
        )),
    );
    // The first specification round receives master plan evidence. A previous
    // specification round has five reviews followed by its four typed drafts.
    add_stage(
        owner,
        "prior-specifications",
        Condition,
        NodeConfiguration::Condition {
            predicate: ConditionPredicate::Exists {
                pointer: "/evidence/5/value/evidence".into(),
            },
        },
        &[("start", "ready", "ready"), ("start", "feedback", "value")],
    )?;
    add_prompt_stage(
        members,
        owner,
        "choose-revisions",
        "Choose what to revise",
        "Specification revision planner",
        planner,
        &[
            ("prior-specifications", "matched", "ready"),
            ("start", "feedback", "feedback"),
            ("start", "context", "context"),
        ],
    )?;
    for (index, subject, writer) in [
        (5, "sdd", "write-sdd"),
        (6, "bdd", "write-bdd"),
        (7, "design", "write-design"),
        (8, "strategy", "write-test-strategy"),
    ] {
        select_specification_revision(members, owner, index, subject, writer)?;
    }
    Ok(())
}

fn select_specification_revision(
    members: &[PackageMember],
    owner: &mut PackageMember,
    index: u8,
    subject: &str,
    writer: &str,
) -> Result<()> {
    let native_context = format!("{writer}-prompt");
    let draft = format!("{writer}-draft");
    let retained = format!("{writer}-retained");
    let prior = format!("{writer}-prior");
    let refresh = format!("{writer}-refresh");
    let ready = format!("{writer}-ready");
    let alternatives = format!("{writer}-alternatives");
    let selected = format!("{writer}-selected");
    let pointer = format!("/evidence/{index}/value/evidence");
    let reference = owner
        .references
        .remove(writer)
        .context("The specification writer is missing its Prompt")?;
    let PackageReference::Draft { key } = &reference else {
        bail!("The specification writer must reference its draft Prompt")
    };
    let PackageArtifactBody::Prompt { prompt } = &members
        .iter()
        .find(|member| &member.key == key)
        .context("The specification Prompt is missing")?
        .body
    else {
        bail!("The specification writer is not a Prompt");
    };
    let schema = prompt
        .output_schema
        .as_ref()
        .and_then(|schema| schema.pointer("/properties/object"))
        .context("The specification has no output contract")?
        .clone();
    let root = graph(owner)?;
    let stage = root
        .nodes
        .iter_mut()
        .find(|stage| stage.id == writer)
        .context("The specification stage is missing")?;
    stage.id = draft.clone();
    root.wires
        .retain(|wire| wire.to != endpoint(writer, "ready"));
    for wire in &mut root.wires {
        if wire.to.node == writer {
            wire.to.node = draft.clone();
        }
    }
    owner.references.insert(draft.clone(), reference);
    redirect(
        owner,
        &endpoint(writer, COMPLETED),
        &endpoint(writer, "completed"),
    )?;
    redirect(
        owner,
        &endpoint(writer, FAILED),
        &endpoint(writer, "failed"),
    )?;
    add_stage(
        owner,
        &prior,
        Condition,
        NodeConfiguration::Condition {
            predicate: ConditionPredicate::Exists {
                pointer: pointer.clone(),
            },
        },
        &[
            (&native_context, "completed", "ready"),
            ("start", "feedback", "value"),
        ],
    )?;
    add_stage(
        owner,
        &refresh,
        Condition,
        NodeConfiguration::Condition {
            predicate: eq(&format!("/evidence/{subject}"), json!(true)),
        },
        &[
            (&prior, "matched", "ready"),
            ("choose-revisions", "selection", "value"),
        ],
    )?;
    add_stage(
        owner,
        &ready,
        Join,
        NodeConfiguration::None,
        &[
            (&prior, "unmatched", "branches"),
            (&refresh, "matched", "branches"),
        ],
    )?;
    graph(owner)?
        .wires
        .push(wire(&ready, "joined", &draft, "ready"));
    add_stage(
        owner,
        &retained,
        SetFields,
        choose_object(&[&pointer])?,
        &[
            (&refresh, "unmatched", "ready"),
            ("start", "feedback", "source"),
        ],
    )?;
    add_stage(
        owner,
        &alternatives,
        CollectEvidence,
        NodeConfiguration::None,
        &[
            (&native_context, "completed", "ready"),
            (&draft, "object", "evidence"),
            (&retained, "object", "evidence"),
        ],
    )?;
    add_stage(
        owner,
        &selected,
        SetFields,
        choose_object(&["/evidence/0/value/evidence", "/evidence/1/value/evidence"])?,
        &[
            (&alternatives, "completed", "ready"),
            (&alternatives, "evidence", "source"),
        ],
    )?;
    add_stage(
        owner,
        writer,
        ValidateOutput,
        NodeConfiguration::ValidateOutput(super::super::data_nodes::ValidateOutputConfiguration {
            schema,
        }),
        &[
            (&selected, "completed", "ready"),
            (&selected, "object", "source"),
        ],
    )?;
    Ok(())
}

fn adapt_candidate_round(
    members: &mut Vec<PackageMember>,
    owner: &mut PackageMember,
) -> Result<()> {
    let root = graph(owner)?;
    root.nodes.retain(|stage| !stage.id.starts_with("review-"));
    root.wires.retain(|wire| {
        !wire.from.node.starts_with("review-") && !wire.to.node.starts_with("review-")
    });
    let mut approach = structured(
        "Assess the exact GREEN candidate and the recorded test strategy. Use corrigir when the candidate needs repair, revisar when the identified risks need specialists, or entregar when no additional specialty is required. Always retain the independent correctness review. Use security for trust and permission changes, performance for hot paths and resource constraints, architecture for shared contracts and dependencies, and maintainability for broad refactors. Give every invited specialist the concrete change to inspect.",
        &[
            ("candidate", ArtifactKind::Candidate),
            ("context", ArtifactKind::EvidenceSet),
            ("green", ArtifactKind::TestPhaseEvidence),
            ("strategy", ArtifactKind::JsonObject),
            ("checks", ArtifactKind::Checks),
        ],
        None,
    );
    approach.routes = ["revisar", "corrigir", "entregar"]
        .map(str::to_owned)
        .into();
    approach.outputs.insert(
        "review".into(),
        PromptOutput {
            kind: ArtifactKind::Review,
            source: Some("correctness.review".into()),
        },
    );
    add_prompt_stage(
        members,
        owner,
        "assess-candidate",
        "Assess the candidate",
        "Task owner",
        approach,
        &[
            ("verify-quality", "verified", "ready"),
            ("refactor", "candidate", "candidate"),
            ("start", "context", "context"),
            ("start", "strategy", "strategy"),
            ("freeze-verified-green", "evidence", "green"),
            ("verify-quality", "checks", "checks"),
        ],
    )?;
    add_specialists(
        members,
        owner,
        "assess-candidate",
        CANDIDATE_SPECIALISTS,
        ReviewCandidate,
    )?;
    add_stage(
        owner,
        "ready-to-deliver",
        Join,
        NodeConfiguration::None,
        &[
            ("assess-candidate", "revisar", "branches"),
            ("assess-candidate", "entregar", "branches"),
        ],
    )?;
    add_stage(
        owner,
        "delivery-intent",
        SetFields,
        flag("deliver")?,
        &[("ready-to-deliver", "joined", "ready")],
    )?;
    let root = graph(owner)?;
    root.wires.retain(|wire| {
        wire.to.node != "candidate-evidence" && wire.to.node != "risk-review-evidence"
    });
    root.wires.extend([
        wire("start", "ready", "candidate-evidence", "ready"),
        wire(
            "assess-candidate",
            "outcome",
            "candidate-evidence",
            "evidence",
        ),
        wire(
            "freeze-verified-green",
            "evidence",
            "candidate-evidence",
            "evidence",
        ),
        wire("verify-quality", "checks", "candidate-evidence", "evidence"),
        wire(
            "quality-checks-passed",
            "object",
            "candidate-evidence",
            "evidence",
        ),
        wire(
            "delivery-intent",
            "object",
            "candidate-evidence",
            "evidence",
        ),
        wire(
            "classify-merge-risk",
            "object",
            "candidate-evidence",
            "evidence",
        ),
        wire(
            "ready-to-deliver",
            "joined",
            "risk-review-evidence",
            "ready",
        ),
        wire(
            "assess-candidate",
            "outcome",
            "risk-review-evidence",
            "evidence",
        ),
        wire(
            "assess-candidate",
            "evidence",
            "risk-review-evidence",
            "evidence",
        ),
    ]);
    let gate = root
        .nodes
        .iter_mut()
        .find(|stage| stage.id == "quality-gate")
        .context("Missing quality gate")?;
    gate.configuration = quality(&[
        (
            "selected candidate reviews",
            "/0/value/evidence/approved",
            json!(true),
        ),
        ("required checks", "/3/value/evidence/passed", json!(true)),
        ("approved routing", "/4/value/evidence/deliver", json!(true)),
    ]);
    let PackageArtifactBody::Subflow { outputs, .. } = &mut owner.body else {
        bail!("Missing adaptive review subflow");
    };
    outputs.insert("review".into(), endpoint("assess-candidate", "review"));
    outputs.insert("selected".into(), endpoint("assess-candidate", "outcome"));
    Ok(())
}

fn bind_selected_reviews(owner: &mut PackageMember) -> Result<()> {
    let root = graph(owner)?;
    let repeat = root
        .nodes
        .iter_mut()
        .find(|stage| stage.id == "implementation")
        .context("Missing delivery verification loop")?;
    let NodeConfiguration::RepeatUntil(repeat) = &mut repeat.configuration else {
        bail!("Missing native delivery attempt limit");
    };
    repeat.exports.push("selected".into());
    let accept = root
        .nodes
        .iter_mut()
        .find(|stage| stage.id == "accept-candidate")
        .context("Missing candidate acceptance gate")?;
    let mut configuration = serde_json::to_value(&accept.configuration)?;
    configuration["reviewMode"] = json!("selected");
    configuration["minimumReviews"] = json!(1);
    accept.configuration = serde_json::from_value(configuration)?;
    root.wires.push(wire(
        "implementation",
        "selected",
        "accept-candidate",
        "selected_reviews",
    ));
    Ok(())
}

const AUDIT_SPECIALISTS: &[Specialist] = &[
    Specialist {
        alias: "requirements",
        name: "Integrated requirements auditor",
        focus: "Audit behavior against the Master Plan and every applicable SDD requirement and acceptance criterion. Cite exact IDs from evidence at the integrated SHA.",
        required: true,
    },
    Specialist {
        alias: "bdd",
        name: "Integrated BDD auditor",
        focus: "Audit positive, negative and boundary BDD scenarios at the integrated SHA. Cite scenario IDs and expected behaviors for each finding.",
        required: true,
    },
    Specialist {
        alias: "architecture",
        name: "Integrated architecture auditor",
        focus: "Audit the approved design, dependencies, interfaces, and reuse conventions at the integrated SHA.",
        required: true,
    },
    Specialist {
        alias: "quality",
        name: "Integrated code quality auditor",
        focus: "Audit maintainability, permissions, performance and regression exposure using exact repository paths and the tested integration revision.",
        required: true,
    },
];

fn adapt_audit_round(members: &mut Vec<PackageMember>, owner: &mut PackageMember) -> Result<()> {
    let owner_key = owner.key.clone();
    let destination = owner.destination;
    let mut prompt = structured(
        "Coordinate the integration audit at the immutable integrated SHA. Read the regression outcome and approved specifications. Identify the requirements, scenarios, architectural boundaries and quality checks each auditor should focus on. Give precise review scopes and preserve the recorded evidence. The downstream native gate verifies all four auditor reports.",
        &[
            ("revision", ArtifactKind::IntegratedRevision),
            ("regression", ArtifactKind::IntegratedChecksEvidence),
            ("context", ArtifactKind::EvidenceSet),
        ],
        None,
    );
    let root = graph(owner)?;
    for specialist in AUDIT_SPECIALISTS {
        let target = match specialist.alias {
            "requirements" => "audit-requirements",
            "bdd" => "audit-bdd",
            "architecture" => "audit-architecture",
            _ => "audit-code-quality",
        };
        let agent = format!("{target}-agent");
        let instructions = format!("{target}-instructions");
        let context = format!("{target}-prompt");
        let validator = root
            .nodes
            .iter()
            .find(|stage| stage.id == target)
            .with_context(|| format!("Missing audit contract for {target}"))?;
        let NodeConfiguration::ValidateOutput(schema) = &validator.configuration else {
            bail!("Unexpected auditor contract for {target}");
        };
        let expected = schema.schema.to_string();
        let removed = [&agent, &instructions, &context];
        root.nodes.retain(|stage| !removed.contains(&&stage.id));
        root.wires.retain(|wire| {
            !removed.contains(&&wire.to.node) && !removed.contains(&&wire.from.node)
        });
        root.wires
            .push(wire("coordinate-audit", COMPLETED, target, "ready"));
        root.wires
            .push(wire("coordinate-audit", specialist.alias, target, "source"));
        prompt.outputs.insert(
            specialist.alias.into(),
            PromptOutput {
                kind: ArtifactKind::AgentResponse,
                source: Some(format!("{}.response", specialist.alias)),
            },
        );
        // Read-only invited auditors use the same checked report shape as the native audit stages.
        let instructions = format!(
            "{} Work in read-only mode. Read the pinned integration SHA with Git. Report missing Git objects as findings. Return one JSON object without Markdown fences conforming to this schema:\n{expected}",
            specialist.focus
        );
        let key = format!("{owner_key}-coordinate-audit-prompt-{}", specialist.alias);
        members.push(role_member(
            &key,
            specialist.name,
            &instructions,
            Agent,
            None,
            destination,
        ));
    }
    add_prompt_stage(
        members,
        owner,
        "coordinate-audit",
        "Audit the integration",
        "Integration audit lead",
        prompt,
        &[
            ("regression-recorded", "joined", "ready"),
            ("start", "integrated", "revision"),
            ("regression", "evidence", "regression"),
            ("start", "context", "context"),
        ],
    )?;
    let card_key = match &owner.references["coordinate-audit"] {
        PackageReference::Draft { key } => key,
        _ => unreachable!(),
    };
    let card = members
        .iter_mut()
        .find(|member| &member.key == card_key)
        .context("Missing audit Prompt")?;
    let PackageArtifactBody::Prompt { prompt } = &mut card.body else {
        bail!("The audit coordinator must be a Prompt");
    };
    for specialist in AUDIT_SPECIALISTS {
        card.references.insert(
            format!("delegate-{}", specialist.alias),
            draft_reference(&format!("{card_key}-{}", specialist.alias)),
        );
        prompt.delegates.push(PromptDelegate {
            alias: specialist.alias.into(),
            required: specialist.required,
        });
    }
    graph(owner)?
        .wires
        .push(wire("coordinate-audit", FAILED, "audit-failed", "branches"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::workflows::{
        authoring::package::WorkflowPackagePublishOutcome, tests::approved_delivery_repository,
    };

    use super::*;

    #[tokio::test]
    async fn all_adaptive_factories_publish_and_simulate_without_replacing_the_originals()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = Store::open(directory.path().join("adaptive-templates.sqlite"))?;
        let repo = approved_delivery_repository(&store, directory.path()).await?;
        let project = repo.project_id;
        let entries = workflow_factory_templates();
        let adaptive = catalog();
        assert_eq!(adaptive.len(), 7);
        assert_eq!(entries.len(), 14);
        for original in [
            WorkflowFactoryTemplate::TaskDelivery,
            WorkflowFactoryTemplate::ReviewDiff,
            WorkflowFactoryTemplate::FixFailedChecks,
            WorkflowFactoryTemplate::DeliveryLibrary,
            WorkflowFactoryTemplate::SuperAiDevelopmentPlanning,
            WorkflowFactoryTemplate::SuperAiDevelopmentDelivery,
            WorkflowFactoryTemplate::SuperAiDevelopmentAudit,
        ] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.id == original && entry.revision == FACTORY_REVISION)
            );
        }
        for entry in adaptive {
            let draft = store
                .create_workflow_factory_draft(project.clone(), entry.id, entry.revision)
                .await
                .with_context(|| format!("Create {}", entry.name))?;
            let prompt_count = draft
                .package
                .members
                .iter()
                .filter(|member| matches!(member.body, PackageArtifactBody::Prompt { .. }))
                .count();
            assert!(
                prompt_count >= 2,
                "{} has {prompt_count} authored cards",
                entry.name
            );
            let WorkflowPackagePublishOutcome::Published { publication } = store
                .publish_reviewed_workflow_package(
                    project.clone(),
                    draft.id.clone(),
                    draft.revision.clone(),
                )
                .await
                .with_context(|| format!("Publish {}", entry.name))?
            else {
                bail!("{} did not produce a publication", entry.name);
            };
            let main = if entry.id == WorkflowFactoryTemplate::DeliveryLibraryAdaptive {
                "review-and-test".to_owned()
            } else {
                draft.package.members[0].key.clone()
            };
            let sample = match entry.id {
                WorkflowFactoryTemplate::SuperAiDevelopmentPlanningAdaptive
                | WorkflowFactoryTemplate::SuperAiDevelopmentDeliveryAdaptive
                | WorkflowFactoryTemplate::SuperAiDevelopmentAuditAdaptive => {
                    json!({"inputs":{"inputs":{"prompt":"Plan and verify the project's export feature."}}})
                }
                WorkflowFactoryTemplate::DeliveryLibraryAdaptive => {
                    json!({"inputs":{"candidate":{"task":{"projectId":"sample","taskId":"sample"},"id":"sample-candidate"}}})
                }
                _ => {
                    json!({"inputs":{"document":"Review the saved evidence and plan the verified change."}})
                }
            };
            let trace = store
                .simulate_workflow_package_revision(
                    project.clone(),
                    draft.id,
                    draft.revision,
                    main.clone(),
                    serde_json::from_value(sample)?,
                )
                .await
                .with_context(|| format!("Simulate {}", entry.name))?;
            let trace = serde_json::to_value(trace)?;
            assert!(
                !trace["steps"]
                    .as_array()
                    .context("Missing simulation steps")?
                    .is_empty()
            );
            assert!(
                publication
                    .members
                    .iter()
                    .any(|published| published.key == main)
            );
        }
        assert!(store.list_workflow_runs(project).await?.is_empty());
        Ok(())
    }
}
