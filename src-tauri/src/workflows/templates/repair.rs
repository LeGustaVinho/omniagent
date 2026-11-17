use super::{
    super::{ConditionPredicate, RepeatUntilConfiguration},
    *,
};

pub(super) fn fix_failed_checks() -> WorkflowPackage {
    use BuiltinNode::*;
    use PortType::{Control, Data};

    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let initialize = [
        "start",
        "context",
        "plan",
        "review-plan",
        "work",
        "approve",
        "prepare",
    ];
    graph.name = "Fix failed checks".into();
    graph
        .nodes
        .retain(|node| initialize.contains(&node.id.as_str()));
    graph.wires.retain(|wire| {
        initialize.contains(&wire.from.node.as_str()) && initialize.contains(&wire.to.node.as_str())
    });
    graph.nodes.push(node(
        "repair",
        RepeatUntil,
        NodeConfiguration::RepeatUntil(RepeatUntilConfiguration {
            max_iterations: 3,
            ready_input: "ready".into(),
            completion_output: "checked".into(),
            evidence_output: "checks".into(),
            until: ConditionPredicate::CandidateChecksPassed,
            feedback: None,
            exports: Vec::new(),
        }),
    ));
    graph.wires.extend([
        wire("prepare", "prepared", "repair", "ready"),
        wire("prepare", "worktree", "repair", "worktree"),
        wire("prepare", "release", "repair", "release"),
        wire("plan", "plan", "repair", "plan"),
    ]);
    let mut pipeline = member(
        "fix-failed-checks",
        "Fix failed checks",
        PackageDestination::Project,
        PackageArtifactBody::Pipeline { graph },
    );
    pipeline.references.insert(
        "repair".into(),
        PackageReference::Draft {
            key: "repair-body".into(),
        },
    );
    let inputs = [
        ("ready", Control),
        ("plan", Data(ArtifactKind::Plan)),
        ("worktree", Data(ArtifactKind::Worktree)),
        ("release", Data(ArtifactKind::Release)),
    ];
    let body = member(
        "repair-body",
        "Repair and verify",
        PackageDestination::Project,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Repair and verify".into(),
                nodes: vec![
                    node(
                        "entry",
                        Start,
                        NodeConfiguration::SubflowEntry {
                            outputs: inputs
                                .iter()
                                .map(|(name, value)| Port {
                                    name: (*name).into(),
                                    value: *value,
                                })
                                .collect(),
                        },
                    ),
                    node("implement", Implement, agent_role("implementer")),
                    node(
                        "verify",
                        VerifyCandidate,
                        NodeConfiguration::RequiredChecks {
                            names: vec![],
                            parallel_groups: vec![],
                        },
                    ),
                ],
                wires: vec![
                    wire("entry", "ready", "implement", "ready"),
                    wire("entry", "plan", "implement", "plan"),
                    wire("entry", "worktree", "implement", "worktree"),
                    wire("entry", "release", "implement", "release"),
                    wire("implement", "implemented", "verify", "ready"),
                    wire("implement", "candidate", "verify", "candidate"),
                ],
            },
            inputs: inputs
                .into_iter()
                .map(|(name, _)| (name.into(), endpoint("entry", name)))
                .collect(),
            outputs: BTreeMap::from([
                ("checked".into(), endpoint("verify", "verified")),
                ("checks".into(), endpoint("verify", "checks")),
            ]),
        },
    );
    WorkflowPackage {
        name: "Fix failed checks".into(),
        members: vec![pipeline, body],
    }
}
