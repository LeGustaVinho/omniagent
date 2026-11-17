use super::*;
use crate::workflows::decision_nodes::ApprovalConfiguration;

pub(super) const NAME: &str = "Integration regression, audits and remediation";
const KEY: &str = "integration-audit";
const REGRESSION_CHECK: &str = "Pipeline regression";
const BUILD_CHECK: &str = "Pipeline build";

struct Auditor {
    id: &'static str,
    role: &'static str,
    instruction: &'static str,
}

const AUDITORS: &[Auditor] = &[
    Auditor {
        id: "audit-requirements",
        role: "Integrated requirements auditor",
        instruction: "Audit the delivered behavior against the Master Plan and every applicable SDD requirement and acceptance criterion. Give exact IDs and verify behavior against the integrated SHA.",
    },
    Auditor {
        id: "audit-bdd",
        role: "Integrated BDD auditor",
        instruction: "Audit the positive, negative and boundary BDD scenarios. Cite each failing, missing or unverified scenario and the expected behavior.",
    },
    Auditor {
        id: "audit-architecture",
        role: "Integrated architecture auditor",
        instruction: "Audit the integrated changes against the approved design, dependencies, interfaces, architecture, and reuse conventions. Identify contradictions and unresolved integration trade-offs.",
    },
    Auditor {
        id: "audit-code-quality",
        role: "Integrated code quality auditor",
        instruction: "Inspect the integrated changes for maintainability, security, performance, dependency risk, consistency and regression exposure. Include concrete repository paths and evidence.",
    },
];

pub(super) fn append(
    flow: &mut Flow<'_>,
    context: Output<'_>,
    contracts: &[(&str, Output<'_>)],
) -> Result<()> {
    let mut ready = ("deliver-backlog", "completed");
    let mut integrated = ("deliver-backlog", "integrated");
    let rounds = [
        "integration-audit-1",
        "integration-audit-2",
        "integration-audit-3",
        "integration-audit-4",
    ];
    let mut failures = Vec::new();
    for (index, round) in rounds.into_iter().enumerate() {
        let limit = format!("{round}-configuration");
        flow.fixed(&limit, BuiltinNode::SetFields, configuration(json!({
            "kind":"set_fields", "mode":"build", "operations":[{"op":"set","path":"/allowRemediation",
                "value":{"source":"literal","value":index + 1 < rounds.len()}}]
        }))?, ready, &[]);
        let mut inputs = vec![
            ("integrated", integrated),
            ("context", context),
            ("configuration", (&limit, "object")),
        ];
        inputs.extend_from_slice(contracts);
        flow.fixed(
            round,
            BuiltinNode::Subflow,
            NodeConfiguration::None,
            (&limit, "completed"),
            &inputs,
        );
        flow.references
            .insert(round.into(), PackageReference::Draft { key: KEY.into() });
        failures.push((round.to_owned(), "failed"));
        failures.extend(finish_release(flow, round)?);
        ready = (round, "remediated");
        integrated = (round, "repairedIntegration");
    }
    let failed = failures
        .iter()
        .map(|(node, port)| (node.as_str(), *port))
        .collect::<Vec<_>>();
    flow.join("integration-audit-blocked", &failed);
    flow.summarize(
        "integration-audit-result", ("integration-audit-blocked", "joined"),
        "The integration audit stopped before producing release evidence. Inspect the verification, remediation and documentation stages.",
        &[],
    )?;
    Ok(())
}

fn finish_release(flow: &mut Flow<'_>, round: &str) -> Result<Vec<(String, &'static str)>> {
    let ready = (round, "approved");
    let integrated = (round, "integrated");
    let audit = (round, "audit");
    let documents = (round, "notes");
    let build = format!("{round}-build");
    verify_integrated(flow, &build, BUILD_CHECK, ready, integrated)?;
    let result = format!("{round}-release-evidence");
    let evidence = flow.collect(
        &result,
        (&build, "verified"),
        &[documents, integrated, audit, (&build, "evidence")],
    );
    let approval = format!("{round}-release-approval");
    flow.fixed(
        &approval, BuiltinNode::Approval, NodeConfiguration::Approval(ApprovalConfiguration {
            message: "Review the exact integrated revision, final build, release criteria, and documentation and cleanup evidence. Confirm readiness for this revision or reject to record outstanding work.".into(),
        }),
        (&result, "completed"), &[("evidence", evidence)],
    );
    flow.summarize(
        &format!("{round}-release-ready"), (&approval, "approved"),
        "Release readiness was approved for the recorded Git commit after the integration audit and final build.",
        &[evidence, (&approval, "decision")],
    )?;
    Ok(vec![
        (build.clone(), "failed"),
        (build, "blocked"),
        (result, "failed"),
        (approval, "rejected"),
    ])
}

pub(super) fn audit_round(presets: &mut Vec<PackageMember>) -> Result<PackageMember> {
    let fields = [
        ("ready", PortType::Control),
        (
            "integrated",
            PortType::Data(ArtifactKind::IntegratedRevision),
        ),
        ("context", PortType::Data(ArtifactKind::EvidenceSet)),
        ("sdd", PortType::Data(ArtifactKind::JsonObject)),
        ("bdd", PortType::Data(ArtifactKind::JsonObject)),
        ("design", PortType::Data(ArtifactKind::JsonObject)),
        ("strategy", PortType::Data(ArtifactKind::JsonObject)),
        ("configuration", PortType::Data(ArtifactKind::JsonObject)),
    ];
    let mut flow = Flow::subflow(NAME, presets, &fields);
    let ready = ("start", "ready");
    let integrated = ("start", "integrated");
    let context = ("start", "context");
    verify_integrated(&mut flow, "regression", REGRESSION_CHECK, ready, integrated)?;
    delivery::passed_check(&mut flow, "regression-passed", ("regression", "verified"))?;
    flow.join(
        "regression-recorded",
        &[("regression", "verified"), ("regression", "failed")],
    );
    let audited = ("regression-recorded", "joined");
    let mut reports = Vec::new();
    for auditor in AUDITORS {
        let instruction = format!(
            "Use the immutable integrated Git SHA in the evidence (for example git show SHA:path). Work in read-only mode. Report any missing Git object or unverifiable behavior as a finding. {}",
            auditor.instruction
        );
        reports.push(flow.generate_agent(
            auditor.id,
            auditor.role,
            &instruction,
            contracts::review(),
            audited,
            &[integrated, ("regression", "evidence"), context],
            AgentSetup::Direct,
        )?);
    }
    reports.push(("regression-passed", "object"));
    reports.push(("regression", "evidence"));
    let audit = flow.collect("audits", audited, &reports);
    let mut rules: Vec<_> = AUDITORS
        .iter()
        .enumerate()
        .map(|(index, audit)| {
            (
                audit.id.into(),
                equals(&format!("/{index}/value/evidence/approved"), json!(true)),
            )
        })
        .collect();
    rules.push((
        "integration regression".into(),
        equals("/4/value/evidence/passed", json!(true)),
    ));
    flow.gate("integration-gate", "audits", rules);
    let notes = flow.generate_agent(
        "review-documentation-cleanup", "Integration documentation and cleanup reviewer",
        "Inspect the exact integrated Git SHA using read-only tools and the integration audit. Produce the release summary and check README, architecture, usage, migration, tests and changelog for updates required by the approved changes. Check for temporary staged artifacts and outdated paths introduced by the delivered work. If any edits are needed, set needsChanges to true and report precise, testable findings with paths and relevant approved requirement or scenario IDs. Use only concrete evidence and preserve intentional files.",
        contracts::documentation_review(), ("integration-gate", "passed"), &[integrated, audit, context], AgentSetup::Direct,
    )?;
    flow.fixed(
        "documentation-changes",
        BuiltinNode::Condition,
        NodeConfiguration::Condition {
            predicate: equals("/evidence/needsChanges", json!(true)),
        },
        ("review-documentation-cleanup", "completed"),
        &[("value", notes)],
    );
    flow.join(
        "remediation-needed",
        &[
            ("integration-gate", "revise"),
            ("integration-gate", "blocked"),
            ("documentation-changes", "matched"),
        ],
    );
    flow.fixed(
        "can-remediate",
        BuiltinNode::Condition,
        NodeConfiguration::Condition {
            predicate: equals("/evidence/allowRemediation", json!(true)),
        },
        ("remediation-needed", "joined"),
        &[("value", ("start", "configuration"))],
    );
    let revision = flow.collect(
        "remediation-evidence",
        ("can-remediate", "matched"),
        &[
            ("regression", "evidence"),
            ("integration-gate", "decision"),
            audit,
            notes,
            context,
            integrated,
        ],
    );
    let proposal = flow.generate_agent(
        "write-remediation-backlog", "Integration remediation planner",
        "Turn actionable audit, regression, documentation, or cleanup findings into atomic tasks using existing SDD and BDD IDs. Use unique permanent task keys prefixed with 'integration-' that identify the integration SHA and specific findings. Preserve all accepted contract behavior. Specify tests that fail for the verified issue. For documentation and cleanup, consider executable examples, links, file presence or absence, or repository conventions tied to approved criteria. Include explicit dependency edges only when needed.",
        contracts::backlog(), ("remediation-evidence", "completed"), &[revision], AgentSetup::Direct,
    )?;
    let reviewed = flow.generate_agent(
        "review-remediation-backlog", "Integration remediation reviewer",
        "Verify that every remediation task addresses a concrete finding, has unique IDs and valid dependencies, references approved SDD and BDD scenarios, and has testable scope. Reject proposals for unavailable infrastructure or unverified findings.",
        contracts::review(), ("write-remediation-backlog", "completed"), &[proposal, revision], AgentSetup::Direct,
    )?;
    flow.collect(
        "remediation-review",
        ("can-remediate", "matched"),
        &[reviewed],
    );
    flow.gate(
        "remediation-gate",
        "remediation-review",
        [(
            "review".into(),
            equals("/0/value/evidence/approved", json!(true)),
        )],
    );
    flow.fixed(
        "remediation-backlog",
        BuiltinNode::MaterializeBacklog,
        NodeConfiguration::None,
        ("remediation-gate", "passed"),
        &[
            ("plan", proposal),
            ("sdd", ("start", "sdd")),
            ("bdd", ("start", "bdd")),
            ("design", ("start", "design")),
            ("strategy", ("start", "strategy")),
        ],
    );
    flow.fixed(
        "remediate",
        BuiltinNode::RunBacklog,
        delivery::dispatch_configuration()?,
        ("remediation-backlog", "completed"),
        &[("backlog", ("remediation-backlog", "backlog"))],
    );
    flow.references
        .insert("remediate".into(), delivery::package_reference());
    flow.join(
        "audit-failed",
        &[
            ("regression", "blocked"),
            ("review-documentation-cleanup-agent", "failed"),
            ("review-documentation-cleanup", "failed"),
            ("can-remediate", "unmatched"),
            ("remediation-gate", "revise"),
            ("remediation-gate", "blocked"),
            ("remediation-backlog", "failed"),
            ("remediate", "failed"),
        ],
    );
    let outputs = [
        ("approved", ("documentation-changes", "unmatched")),
        ("failed", ("audit-failed", "joined")),
        ("audit", audit),
        ("notes", notes),
        ("integrated", integrated),
        ("remediated", ("remediate", "completed")),
        ("repairedIntegration", ("remediate", "integrated")),
    ];
    Ok(flow.subflow_member(KEY, PackageDestination::Project, &fields, &outputs))
}

fn verify_integrated(
    flow: &mut Flow<'_>,
    id: &str,
    check: &str,
    ready: Output<'_>,
    integrated: Output<'_>,
) -> Result<()> {
    flow.fixed(
        id,
        BuiltinNode::VerifyIntegratedChecks,
        configuration(json!({"kind":"integrated_checks","checks":[check]}))?,
        ready,
        &[("integrated", integrated)],
    );
    Ok(())
}
