use super::*;

pub(super) const NAME: &str = "Backlog task delivery";
pub(super) const REVIEW: &str = "Implement, refactor and verify candidate";

const KEY: &str = "task-delivery";
const REVIEW_KEY: &str = "candidate-review";
const TEST_CHECK: &str = "Pipeline test";
const BUILD_CHECK: &str = "Pipeline build";
const CANDIDATE_CHECKS: &[&str] = &[
    "Pipeline unit",
    "Pipeline integration",
    "Pipeline functional",
    "Pipeline static",
    "Pipeline dependencies",
    "Pipeline candidate regression",
];

pub(super) fn dispatch_configuration() -> Result<NodeConfiguration> {
    configuration(json!({
        "kind": "run_backlog",
        "child": { "id": "", "revision": "" },
        "maxParallel": 3,
        "failed": { "node": "delivery-failed", "port": "joined" }
    }))
}

pub(super) fn package_reference() -> PackageReference {
    PackageReference::Draft { key: KEY.into() }
}

pub(super) fn members(presets: &mut Vec<PackageMember>) -> Result<[PackageMember; 2]> {
    Ok([delivery(presets)?, verify_candidate(presets)?])
}

fn delivery(presets: &mut Vec<PackageMember>) -> Result<PackageMember> {
    let mut flow = Flow::new(NAME, presets);
    flow.add(
        "start",
        BuiltinNode::TypedStart,
        configuration(json!({
            "kind": "typed_start",
            "fields": [{ "name": "backlog", "valueType": "object", "required": true }]
        }))?,
    );
    let start = ("start", "start");
    let plan = ("start", "plan");
    let task = ("start", "task");
    flow.fixed(
        "approved-test-strategy", BuiltinNode::SetFields, configuration(json!({
            "kind": "set_fields", "mode": "build",
            "operations": [
                {"op": "set", "path": "/testStrategy", "value": {"source": "input", "pointer": "/backlog/specificationEvidence/strategy/testStrategy"}},
                {"op": "set", "path": "/requiredChecks", "value": {"source": "input", "pointer": "/backlog/specificationEvidence/strategy/requiredChecks"}}
            ]
        }))?, start, &[("source", ("start", "inputs"))],
    );
    flow.fixed(
        "approve-task-scope",
        BuiltinNode::ApproveImplementation,
        NodeConfiguration::None,
        start,
        &[("plan", plan), ("task", task)],
    );
    flow.fixed(
        "prepare-task-worktree",
        BuiltinNode::PrepareWorktree,
        NodeConfiguration::None,
        ("approve-task-scope", "approved"),
        &[
            ("task", task),
            ("decision", ("approve-task-scope", "decision")),
        ],
    );
    let released = ("prepare-task-worktree", "prepared");
    let release = ("prepare-task-worktree", "release");
    let worktree = ("prepare-task-worktree", "worktree");
    flow.fixed(
        "verify-baseline",
        BuiltinNode::VerifyTestPhase,
        test_phase("baseline")?,
        released,
        &[("release", release), ("worktree", worktree)],
    );
    let tests = flow.generate(
        "specify-red-tests", "TDD specification author",
        "Read the pinned task and its acceptance criteria, SDD requirements and BDD scenarios. Inspect the repository's test conventions. For a CI repair, also inspect the recorded failure and the differences between the previous source and current destination. Propose tests for RED and the exact assertion failure expected before implementing each behavior. List only test paths compatible with the configured check, and use the test identifiers the registered runner reports.",
        contracts::red_tests(), ("verify-baseline", "verified"),
        &[plan, ("start", "inputs"), ("verify-baseline", "evidence")],
    )?;
    implement(
        &mut flow,
        "write-failing-tests",
        "TDD RED test writer",
        "Write only the tests identified by the specification. Match its paths, test names and behavior-specific expected assertion failures. Preserve existing tests and leave production code unchanged.",
        ("specify-red-tests", "completed"),
        plan,
        release,
        worktree,
        &[tests],
    )?;
    let red_candidate = ("write-failing-tests", "candidate");
    flow.fixed(
        "verify-red",
        BuiltinNode::VerifyTestPhase,
        test_phase("red")?,
        ("write-failing-tests", "implemented"),
        &[
            ("candidate", red_candidate),
            ("prior", ("verify-baseline", "evidence")),
            ("release", release),
            ("worktree", worktree),
            ("tests", tests),
        ],
    );
    let red_evidence = ("verify-red", "evidence");
    let context = flow.collect(
        "task-evidence",
        ("verify-red", "verified"),
        &[("start", "inputs"), plan, tests],
    );
    let initial_feedback = flow.collect(
        "initial-red-evidence",
        ("verify-red", "verified"),
        &[red_evidence],
    );
    flow.fixed(
        "implementation",
        BuiltinNode::RepeatUntil,
        NodeConfiguration::RepeatUntil(RepeatUntilConfiguration {
            max_iterations: ITERATIONS,
            ready_input: "ready".into(),
            completion_output: "done".into(),
            evidence_output: "decision".into(),
            until: equals("/evidence/decision", json!("passed")),
            feedback: Some(RepeatFeedback {
                input: "feedback".into(),
                output: "feedback".into(),
            }),
            exports: ["candidate", "review", "checks", "quality", "build", "risk"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }),
        ("task-evidence", "completed"),
        &[
            ("plan", plan),
            ("release", release),
            ("worktree", worktree),
            ("red", red_evidence),
            ("strategy", ("approved-test-strategy", "object")),
            ("context", context),
            ("feedback", initial_feedback),
        ],
    );
    flow.references.insert(
        "implementation".into(),
        PackageReference::Draft {
            key: REVIEW_KEY.into(),
        },
    );
    let candidate = ("implementation", "candidate");
    flow.fixed("accept-candidate", BuiltinNode::AcceptCandidate, configuration(json!({
        "kind": "candidate_acceptance", "buildCheck": BUILD_CHECK, "minimumReviews": 5, "requireGreen": true,
    }))?, ("implementation", "completed"), &[
        ("candidate", candidate), ("review", ("implementation", "review")),
        ("checks", ("implementation", "checks")), ("quality", ("implementation", "quality")),
        ("build", ("implementation", "build")),
    ]);
    flow.fixed(
        "verify-commit",
        BuiltinNode::VerifyCommit,
        NodeConfiguration::None,
        ("accept-candidate", "accepted"),
        &[
            ("candidate", candidate),
            ("acceptance", ("accept-candidate", "acceptance")),
        ],
    );
    let pull_request_failures = pull_request::append(
        &mut flow,
        ("verify-commit", "verified"),
        ("verify-commit", "git"),
        ("implementation", "risk"),
    );
    let mut failures = vec![
        ("approved-test-strategy", "failed"),
        ("verify-baseline", "failed"),
        ("verify-baseline", "blocked"),
        ("specify-red-tests", "failed"),
        ("write-failing-tests", "failed"),
        ("verify-red", "failed"),
        ("verify-red", "blocked"),
        ("implementation", "failed"),
        ("accept-candidate", "failed"),
        ("verify-commit", "failed"),
    ];
    failures.extend(pull_request_failures);
    flow.join("delivery-failed", &failures);
    Ok(flow.pipeline(KEY))
}

fn verify_candidate(presets: &mut Vec<PackageMember>) -> Result<PackageMember> {
    use ArtifactKind::{EvidenceSet, JsonObject, Plan, Release, TestPhaseEvidence, Worktree};
    use PortType::{Control, Data};
    let fields = [
        ("ready", Control),
        ("plan", Data(Plan)),
        ("release", Data(Release)),
        ("worktree", Data(Worktree)),
        ("red", Data(TestPhaseEvidence)),
        ("strategy", Data(JsonObject)),
        ("context", Data(EvidenceSet)),
        ("feedback", Data(EvidenceSet)),
    ];
    let mut flow = Flow::subflow(REVIEW, presets, &fields);
    let start = ("start", "ready");
    let plan = ("start", "plan");
    let release = ("start", "release");
    let worktree = ("start", "worktree");
    let red = ("start", "red");
    let context = [("start", "context"), ("start", "feedback")];
    implement(
        &mut flow,
        "implement-green",
        "TDD GREEN implementer",
        "Implement the smallest production change satisfying the pinned requirements and the verified RED tests. Keep the RED tests and assertions intact. Address actionable findings in the prior round's evidence and any verified CI failure from the pinned repair plan.",
        start,
        plan,
        release,
        worktree,
        &context,
    )?;
    flow.fixed(
        "verify-green",
        BuiltinNode::VerifyTestPhase,
        test_phase("green")?,
        ("implement-green", "implemented"),
        &[
            ("candidate", ("implement-green", "candidate")),
            ("prior", red),
            ("release", release),
            ("worktree", worktree),
        ],
    );
    implement(
        &mut flow,
        "refactor",
        "TDD refactor specialist",
        "Refactor only where needed for readability, maintainability and architectural fit. Preserve behavior and the verified RED tests. If no refactoring is warranted, leave the current candidate unchanged.",
        ("verify-green", "verified"),
        plan,
        release,
        worktree,
        &[context[0], ("verify-green", "evidence")],
    )?;
    let candidate = ("refactor", "candidate");
    flow.fixed(
        "freeze-verified-green",
        BuiltinNode::VerifyTestPhase,
        test_phase("green")?,
        ("refactor", "implemented"),
        &[
            ("candidate", candidate),
            ("prior", red),
            ("release", release),
            ("worktree", worktree),
        ],
    );
    let ready = ("freeze-verified-green", "verified");
    let reviewers = [
        (
            "review-correctness",
            "Candidate correctness reviewer",
            "Review functional correctness against the pinned criteria and scenarios. Trace behavior through the exact candidate.",
        ),
        (
            "review-maintainability",
            "Candidate readability and maintainability reviewer",
            "Review readability, complexity, naming, change scope and maintainability. Use concrete candidate evidence.",
        ),
        (
            "review-security",
            "Candidate security reviewer",
            "Review security, privacy, permissions, trust boundaries, inputs, failure paths and abuse cases.",
        ),
        (
            "review-performance",
            "Candidate performance reviewer",
            "Review time, space, I/O, contention, resource use and foreseeable performance regressions.",
        ),
        (
            "review-architecture",
            "Candidate architecture and consistency reviewer",
            "Review architectural fit, dependency boundaries, reuse, API contracts and consistency with repository patterns.",
        ),
    ];
    for (id, role, instruction) in reviewers {
        let brief = flow.brief(id, instruction, ready, &[context[0]])?;
        flow.fixed(
            id,
            BuiltinNode::ReviewCandidate,
            agent_role(role),
            (brief.as_str(), "completed"),
            &[
                ("candidate", candidate),
                ("brief", (brief.as_str(), "text")),
            ],
        );
    }
    flow.fixed(
        "verify-quality",
        BuiltinNode::VerifyCandidate,
        configuration(json!({
            "kind": "staged_checks",
            "names": CANDIDATE_CHECKS,
            "parallelGroups": CANDIDATE_CHECKS.iter().map(|name| [*name]).collect::<Vec<_>>(),
            "deferredNames": [BUILD_CHECK, TEST_CHECK, "Pipeline regression"],
            "strategyChecks": contracts::OPTIONAL_STRATEGY_CHECKS,
        }))?,
        ready,
        &[
            ("candidate", candidate),
            ("strategy", ("start", "strategy")),
        ],
    );
    flow.fixed(
        "quality-checks",
        BuiltinNode::Condition,
        NodeConfiguration::Condition {
            predicate: ConditionPredicate::CandidateChecksPassed,
        },
        ("verify-quality", "verified"),
        &[("value", ("verify-quality", "checks"))],
    );
    passed_check(
        &mut flow,
        "quality-checks-passed",
        ("quality-checks", "matched"),
    )?;
    // The named check policy controls QA, static, dependency, and regression execution.
    let green = ("freeze-verified-green", "evidence");
    let mut reports: Vec<_> = reviewers.iter().map(|(id, _, _)| (*id, "report")).collect();
    let review_evidence = flow.collect("risk-review-evidence", ready, &reports);
    let risk = flow.generate_agent(
        "classify-merge-risk", "Candidate merge risk reviewer",
        "Classify the change using the structured candidate review reports, task scope and verified test evidence. Set highRisk when it changes security or compliance boundaries, handles sensitive data, performs destructive or irreversible operations, introduces material delivery or dependency risk, or when evidence is insufficient to safely classify the change.",
        contracts::risk(), ("risk-review-evidence", "completed"),
        &[review_evidence, context[0], candidate, green], false,
    )?;
    reports.push(green);
    reports.push(("verify-quality", "checks"));
    reports.push(("quality-checks-passed", "object"));
    reports.push(risk);
    flow.collect("candidate-evidence", start, &reports);
    let mut criteria: BTreeMap<_, _> = reviewers
        .iter()
        .enumerate()
        .map(|(index, (id, _, _))| {
            (
                id.to_string(),
                equals(
                    &format!("/{index}/value/evidence/verdict"),
                    json!("approved"),
                ),
            )
        })
        .collect();
    criteria.insert(
        "required quality checks".into(),
        equals("/7/value/evidence/passed", json!(true)),
    );
    flow.gate("quality-gate", "candidate-evidence", criteria);
    flow.fixed(
        "build-approved-tree",
        BuiltinNode::VerifyCandidate,
        NodeConfiguration::RequiredChecks {
            names: vec![BUILD_CHECK.into()],
            parallel_groups: vec![],
        },
        ("quality-gate", "passed"),
        &[("candidate", candidate)],
    );
    flow.fixed(
        "build-passed",
        BuiltinNode::Condition,
        NodeConfiguration::Condition {
            predicate: ConditionPredicate::CandidateChecksPassed,
        },
        ("build-approved-tree", "verified"),
        &[("value", ("build-approved-tree", "checks"))],
    );
    passed_check(&mut flow, "build-check-passed", ("build-passed", "matched"))?;
    flow.collect(
        "attempt-evidence",
        start,
        &[
            ("quality-gate", "decision"),
            ("build-check-passed", "object"),
            ("build-approved-tree", "checks"),
            ("candidate-evidence", "evidence"),
        ],
    );
    flow.gate(
        "attempt-gate",
        "attempt-evidence",
        [
            (
                "quality".into(),
                equals("/0/value/evidence/decision", json!("passed")),
            ),
            (
                "build".into(),
                equals("/1/value/evidence/passed", json!(true)),
            ),
        ],
    );
    flow.join(
        "round-completed",
        &[
            ("attempt-gate", "passed"),
            ("attempt-gate", "revise"),
            ("attempt-gate", "blocked"),
        ],
    );
    let outputs = [
        ("done", ("round-completed", "joined")),
        ("decision", ("attempt-gate", "decision")),
        ("feedback", ("attempt-evidence", "evidence")),
        ("quality", ("quality-gate", "decision")),
        ("candidate", candidate),
        ("review", ("review-correctness", "review")),
        ("checks", ("verify-quality", "checks")),
        ("build", ("build-approved-tree", "checks")),
        ("risk", risk),
    ];
    Ok(flow.subflow_member(REVIEW_KEY, PackageDestination::Project, &fields, &outputs))
}

pub(super) fn passed_check(flow: &mut Flow<'_>, id: &str, ready: Output<'_>) -> Result<()> {
    flow.fixed(
        id,
        BuiltinNode::SetFields,
        configuration(json!({
            "kind":"set_fields", "mode":"build",
            "operations":[{"op":"set","path":"/passed","value":{"source":"literal","value":true}}]
        }))?,
        ready,
        &[],
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn implement(
    flow: &mut Flow<'_>,
    id: &str,
    role: &str,
    instruction: &str,
    ready: Output<'_>,
    plan: Output<'_>,
    release: Output<'_>,
    worktree: Output<'_>,
    evidence: &[Output<'_>],
) -> Result<()> {
    let source = flow.brief(id, instruction, ready, evidence)?;
    flow.fixed(
        id,
        BuiltinNode::Implement,
        agent_role(role),
        (source.as_str(), "completed"),
        &[
            ("plan", plan),
            ("release", release),
            ("worktree", worktree),
            ("brief", (source.as_str(), "text")),
        ],
    );
    Ok(())
}

fn test_phase(phase: &str) -> Result<NodeConfiguration> {
    let paths = if phase == "red" {
        json!([
            "tests/",
            "test/",
            "*.test.ts",
            "*.test.tsx",
            "*.spec.ts",
            "*.spec.tsx"
        ])
    } else {
        json!([])
    };
    configuration(json!({
        "kind":"verify_test_phase", "phase": phase, "check": TEST_CHECK, "allowedTestPaths": paths
    }))
}
