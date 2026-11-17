use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::{
    providers::{ProviderLocation, TurnOutcome},
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{
        BuiltinNode, Endpoint, NodeConfiguration, NodeInstance, Wire, WorkflowApprovalStage,
        WorkflowGraph, WorkflowHostRequest, WorkflowHostSubmission, WorkflowRevision, WorkflowRun,
        builtin_delivery_graph,
    },
};

use super::*;

pub(super) mod github_fixture;

pub(super) const DESTINATION: &str = "fixture-integration";
const QUALITY_CHECK: &str = "fixture-regression";

fn wire(source: (&str, &str), target: (&str, &str)) -> Wire {
    Wire {
        from: Endpoint {
            node: source.0.into(),
            port: source.1.into(),
        },
        to: Endpoint {
            node: target.0.into(),
            port: target.1.into(),
        },
    }
}

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn config(value: Value) -> Result<NodeConfiguration> {
    Ok(serde_json::from_value(value)?)
}

fn literal_object(id: &str, value: Value) -> Result<NodeInstance> {
    let operations = value
        .as_object()
        .context("The fixture expects literal object fields")?
        .iter()
        .map(|(field, value)| {
            json!({"op":"set","path":format!("/{field}"),
            "value":{"source":"literal","value":value}})
        })
        .collect::<Vec<_>>();
    Ok(node(
        id,
        BuiltinNode::SetFields,
        config(json!({"kind":"set_fields","mode":"build","operations":operations}))?,
    ))
}

fn child_graph(github: bool) -> Result<WorkflowGraph> {
    let mut graph = builtin_delivery_graph(Default::default());
    let kept = [
        "approve",
        "prepare",
        "implement",
        "review-candidate",
        "verify-candidate",
        "accept",
        "commit",
        "merge",
    ];
    graph
        .nodes
        .retain(|item| kept.contains(&item.id.as_str()) && !(github && item.id == "merge"));
    graph.wires.retain(|wire| {
        graph.nodes.iter().any(|item| item.id == wire.from.node)
            && graph.nodes.iter().any(|item| item.id == wire.to.node)
    });
    graph.nodes.push(node("start", BuiltinNode::TypedStart, config(json!({
        "kind":"typed_start", "fields":[{"name":"backlog","valueType":"object","required":true}],
    }))?));
    graph.wires.extend([
        wire(("start", "start"), ("approve", "ready")),
        wire(("start", "plan"), ("approve", "plan")),
        wire(("start", "task"), ("approve", "task")),
        wire(("start", "task"), ("prepare", "task")),
        wire(("start", "plan"), ("implement", "plan")),
    ]);
    if github {
        graph.nodes.extend([
            node("pull", BuiltinNode::OpenOrUpdatePullRequest, NodeConfiguration::None),
            node("ci", BuiltinNode::WaitForCi, NodeConfiguration::None),
            node("release-approval", BuiltinNode::Approval, config(json!({
                "kind":"approval", "message":"Approve this tested high-risk revision for merge" }))?),
            node("merge", BuiltinNode::MergePullRequest, NodeConfiguration::None),
        ]);
        graph.wires.extend([
            wire(("commit", "verified"), ("pull", "ready")),
            wire(("commit", "git"), ("pull", "git")),
            wire(("pull", "completed"), ("ci", "ready")),
            wire(("pull", "pull"), ("ci", "pull")),
            wire(("ci", "approval_required"), ("release-approval", "ready")),
            wire(("ci", "ci"), ("release-approval", "evidence")),
            wire(("release-approval", "approved"), ("merge", "ready")),
            wire(("release-approval", "decision"), ("merge", "approval")),
            wire(("ci", "ci"), ("merge", "ci")),
        ]);
    }
    Ok(graph)
}

fn task(key: &str, priority: u16, dependencies: &[&str]) -> Value {
    json!({"key":key,"title":format!("Deliver {key}"),"description":"Follow the pinned requirements.",
        "priority":priority,"dependsOn":dependencies,"acceptanceCriteria":["The task output is observable"],
        "sddRequirements":["REQ-1"],"bddScenarios":["BDD-1"]})
}

async fn launch_backlog(
    store: &Store,
    repo: &ProjectCheckout,
    github: bool,
) -> Result<WorkflowRun> {
    let child = store
        .save_workflow_revision(
            repo.project_id.clone(),
            "saga-task".into(),
            child_graph(github)?,
        )
        .await?;
    launch_backlog_for(store, repo, child, if github { "ci" } else { "implement" }).await
}

async fn launch_backlog_for(
    store: &Store,
    repo: &ProjectCheckout,
    child: WorkflowRevision,
    failure: &str,
) -> Result<WorkflowRun> {
    let graph = WorkflowGraph {
        name: "Backlog execution and integrated audit".into(),
        nodes: vec![
            node(
                "start",
                BuiltinNode::TypedStart,
                config(json!({"kind":"typed_start","fields":[]}))?,
            ),
            literal_object(
                "plan",
                json!({"epicTitle":"Delivery saga", "tasks":[
                    task("A", 2, &[]), task("B", 10, &[]), task("C", 20, &["B"]),
                ]}),
            )?,
            literal_object(
                "sdd",
                json!({"requirements":[{"id":"REQ-1","description":"Project outputs are verifiable"}]}),
            )?,
            literal_object(
                "bdd",
                json!({"scenarios":[{"id":"BDD-1","then":"The task output can be read"}]}),
            )?,
            node(
                "materialize",
                BuiltinNode::MaterializeBacklog,
                NodeConfiguration::None,
            ),
            node(
                "execute",
                BuiltinNode::RunBacklog,
                config(json!({"kind":"run_backlog", "child":child,
                "maxParallel":1, "failed": {"node": failure, "port":"failed"}}))?,
            ),
            node(
                "audit",
                BuiltinNode::VerifyIntegratedChecks,
                config(json!({"kind":"integrated_checks","checks":[QUALITY_CHECK]}))?,
            ),
            literal_object(
                "remediation-plan",
                json!({"epicTitle":"Integrated remediation", "tasks":[task("FIX", 10, &[])]}),
            )?,
            node(
                "remediate",
                BuiltinNode::MaterializeBacklog,
                NodeConfiguration::None,
            ),
        ],
        wires: vec![
            wire(("start", "start"), ("plan", "ready")),
            wire(("start", "start"), ("sdd", "ready")),
            wire(("start", "start"), ("bdd", "ready")),
            wire(("plan", "completed"), ("materialize", "ready")),
            wire(("plan", "object"), ("materialize", "plan")),
            wire(("sdd", "object"), ("materialize", "sdd")),
            wire(("bdd", "object"), ("materialize", "bdd")),
            wire(("materialize", "completed"), ("execute", "ready")),
            wire(("materialize", "backlog"), ("execute", "backlog")),
            wire(("execute", "completed"), ("audit", "ready")),
            wire(("execute", "integrated"), ("audit", "integrated")),
            wire(("audit", "failed"), ("remediation-plan", "ready")),
            wire(("remediation-plan", "completed"), ("remediate", "ready")),
            wire(("remediation-plan", "object"), ("remediate", "plan")),
            wire(("sdd", "object"), ("remediate", "sdd")),
            wire(("bdd", "object"), ("remediate", "bdd")),
        ],
    };
    let version = store
        .save_workflow_revision(repo.project_id.clone(), "saga-parent".into(), graph)
        .await?;
    store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId": repo.project_id, "workflow": version, "checkoutId": repo.checkout_id,
            "requestId":"saga-parent", "inputs":{}, "automatic":true,
        }))?)
        .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Child {
    key: String,
    task_id: String,
    workflow_run_id: String,
}

async fn children(store: &Store, run: &WorkflowRun) -> Result<Vec<Child>> {
    let inspection = store
        .inspect_workflow_execution(run.project_id.clone(), run.id.clone())
        .await?
        .context("The backlog run disappeared")?;
    let value = serde_json::to_value(&inspection.backlog_executions)?;
    ensure!(
        value[0]["state"] != "failed",
        "The backlog scheduler failed: {}",
        value[0]["reason"]
    );
    Ok(serde_json::from_value(value[0]["tasks"].clone())?)
}

async fn wait_for_children(store: &Store, run: &WorkflowRun, count: usize) -> Result<Vec<Child>> {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            store.settle_workflow_backlogs().await?;
            let started = children(store, run).await?;
            if started.len() >= count {
                return Ok(started);
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    })
    .await
    .context("The backlog did not release the next ready task")?
}

async fn prepare_request(
    store: &Store,
    project: &str,
    child: &Child,
    root: &Path,
) -> Result<WorkflowHostSubmission> {
    let stage = store
        .workflow_approval_stage(
            project.into(),
            child.workflow_run_id.clone(),
            "approve".into(),
        )
        .await?;
    let WorkflowApprovalStage::Ready {
        plan_id,
        task_artifact_id,
        ..
    } = stage
    else {
        anyhow::bail!("The backlog child's current plan is not ready for approval: {stage:?}");
    };
    let scope = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.into(),
        run_id: child.workflow_run_id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let approval = store
        .execute_workflow_host(scope(
            "approve",
            NodeInputs::from([
                ("plan".into(), plan_id),
                ("task".into(), task_artifact_id.clone()),
            ]),
            WorkflowHostRequest::ApproveImplementation,
        ))
        .await?;
    Ok(scope(
        "prepare",
        NodeInputs::from([
            ("decision".into(), approval.id),
            ("task".into(), task_artifact_id),
        ]),
        WorkflowHostRequest::PrepareWorktree {
            default_root: root.into(),
        },
    ))
}

async fn artifact(
    store: &Store,
    project: &str,
    run: &str,
    node: &str,
    port: &str,
    unit: Option<&str>,
) -> Result<WorkflowArtifact> {
    let view = store
        .inspect_workflow_execution(project.into(), run.into())
        .await?
        .context("The fixture workflow is unavailable")?;
    let found = view
        .outputs
        .into_iter()
        .find(|output| {
            output.current
                && output.artifact.node_instance_id == node
                && output.artifact.port == port
                && output.artifact.unit_id.as_deref() == unit
        })
        .map(|output| output.artifact);
    if let Some(output) = found {
        return Ok(output);
    }
    let control = view
        .control
        .iter()
        .find(|stage| stage.node_id == node && stage.unit_id.as_deref() == unit);
    let waits = serde_json::to_value(view.automatic_agent_waits)?;
    anyhow::bail!("Missing current artifact {node}.{port} (control {control:?}, pending {waits})")
}

struct FixedDelivery {
    unit: WorkflowDeliveryUnit,
    git: WorkflowArtifact,
    commit: VerifiedCommit,
}

async fn current_child(
    store: &Store,
    repo: &ProjectCheckout,
    child: &Child,
) -> Result<(WorkflowRun, WorkflowDeliveryUnit, ManagedWorktree)> {
    let project = &repo.project_id;
    let run_id = &child.workflow_run_id;
    let run = store
        .workflow_run(project.clone(), run_id.clone())
        .await?
        .context("Missing child run")?;
    let mut units = store
        .workflow_delivery_units(project.clone(), child.task_id.clone(), run_id.clone())
        .await?;
    ensure!(
        units.len() == 1,
        "Each backlog task must have exactly one pinned logical unit"
    );
    let unit = units.pop().unwrap();
    let managed = store
        .live_managed_worktree(project.clone(), child.task_id.clone())
        .await?
        .context("No managed worktree exists for the approved task")?
        .worktree;
    Ok((run, unit, managed))
}

async fn capture_child(
    store: &Store,
    repo: &ProjectCheckout,
    scope: (&WorkflowRun, &WorkflowDeliveryUnit, &ManagedWorktree),
    writer_node: &str,
    change: (&str, &str),
) -> Result<WorkflowArtifact> {
    let (run, unit, managed) = scope;
    let (project, run_id) = (&run.project_id, &run.id);
    let unit_id = unit.unit.id.as_str();
    let release = artifact(store, project, run_id, "prepare", "release", None).await?;
    let ArtifactValue::Release(authorization) = &release.value else {
        unreachable!()
    };
    let worktree = artifact(store, project, run_id, "prepare", "worktree", None).await?;
    let mut request = submission(
        repo,
        run,
        writer_node,
        Some(unit.plan_revision.clone()),
        SessionMode::ApprovedWrite,
    );
    request.unit_id = Some(unit_id.into());
    request.scope.checkout = managed.root.clone();
    request.request.task_id = Some(unit.task_id.clone());
    request.request.checkout_id = Some(managed.checkout_id.clone());
    request.dispatch.release_id = Some(authorization.release.id.clone());
    request.artifacts = NodeInputs::from([
        ("plan".into(), unit.plan_revision.clone()),
        ("worktree".into(), worktree.id),
        ("release".into(), release.id),
    ]);
    let writer = store.submit_workflow_agent(request).await?;
    let attempt = claim_fixture_job(store, writer, fixture_admission()).await?;
    let source_run = attempt.run.id;
    store.mark_agent_running(source_run.clone()).await?;
    ensure!(repository::capture_worktree_start(store, source_run.clone()).await?);
    fs::write(managed.root.join(change.0), change.1)?;
    repository::save_worktree_candidate(store, source_run.clone()).await?;
    store
        .finish_agent_attempt(source_run, RunStatus::Succeeded, None)
        .await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The candidate was not dispatched for publication")?
        .publish(store)
        .await?;
    artifact(
        store,
        project,
        run_id,
        writer_node,
        "candidate",
        Some(unit_id),
    )
    .await
}

async fn commit_child(
    store: &Store,
    repo: &ProjectCheckout,
    child: &Child,
    file: &str,
    text: &str,
) -> Result<FixedDelivery> {
    let (run, unit, managed) = current_child(store, repo, child).await?;
    let (project, run_id, unit_id) = (&run.project_id, &run.id, unit.unit.id.as_str());
    let candidate = capture_child(
        store,
        repo,
        (&run, &unit, &managed),
        "implement",
        (file, text),
    )
    .await?;
    let ArtifactValue::Candidate(selection) = &candidate.value else {
        unreachable!()
    };
    let mut request = submission(
        repo,
        &run,
        "review-candidate",
        Some(unit.plan_revision.clone()),
        SessionMode::ReadOnly,
    );
    request.unit_id = Some(unit_id.into());
    request.scope.checkout = managed.root.clone();
    request.request.task_id = Some(child.task_id.clone());
    request.request.checkout_id = Some(managed.checkout_id.clone());
    request.artifacts = NodeInputs::from([("candidate".into(), candidate.id.clone())]);
    let reviewer = store.submit_workflow_agent(request).await?;
    let review_attempt = claim_fixture_job(store, reviewer, fixture_admission()).await?;
    seal_review(store, selection, &review_attempt).await?;
    let review_run = review_attempt.run.id;
    store
        .record_agent_observation(
            review_run.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    session_id: Some(format!("session-{review_run}")),
                    turn_id: Some("turn-1".into()),
                    ..Default::default()
                },
                data: ProviderEventData::TurnFinished {
                    outcome: TurnOutcome::Completed,
                },
            },
        )
        .await?;
    store
        .finish_observed_agent_attempt(
            review_run,
            RunStatus::Succeeded,
            Some(
                r#"{"verdict":"approved","criteria":[{"index":1,"status":"met"}],"findings":[]}"#
                    .into(),
            ),
            None,
            true,
        )
        .await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The scoped review did not reach publication")?
        .publish(store)
        .await?;
    let review = artifact(
        store,
        project,
        run_id,
        "review-candidate",
        "review",
        Some(unit_id),
    )
    .await?;
    store
        .next_workflow_unit_action()
        .await?
        .context("The candidate check was not scheduled")?
        .execute(store)
        .await?;
    let checks = artifact(
        store,
        project,
        run_id,
        "verify-candidate",
        "checks",
        Some(unit_id),
    )
    .await?;
    let invoke = |node: &str, inputs, request| WorkflowUnitSubmission {
        project_id: project.clone(),
        run_id: run_id.clone(),
        node_id: node.into(),
        unit_id: unit_id.into(),
        task_id: child.task_id.clone(),
        inputs,
        request,
    };
    let WorkflowUnitOutcome::Artifact {
        artifact: acceptance,
    } = store
        .execute_workflow_unit(invoke(
            "accept",
            NodeInputs::from([
                ("candidate".into(), candidate.id.clone()),
                ("review".into(), review.id),
                ("checks".into(), checks.id),
            ]),
            WorkflowUnitRequest::AcceptCandidate {
                completion_criteria: unit.unit.completion_criteria.clone(),
            },
        ))
        .await?
    else {
        anyhow::bail!("The fixture operator acceptance did not record an artifact")
    };
    let WorkflowUnitOutcome::Artifact { artifact: git } = store
        .execute_workflow_unit(invoke(
            "commit",
            NodeInputs::from([
                ("candidate".into(), candidate.id),
                ("acceptance".into(), acceptance.id),
            ]),
            WorkflowUnitRequest::VerifyCommit { external_sha: None },
        ))
        .await?
    else {
        anyhow::bail!("The checked unit did not create a verified commit")
    };
    let ArtifactValue::Git(committed) = &git.value else {
        unreachable!()
    };
    let commit = store
        .verified_delivery_commit(project.clone(), child.task_id.clone(), committed.id.clone())
        .await?
        .context("The Git artifact lost its verified commit receipt")?;
    let plan = store
        .resolve_workflow_artifact(
            project.clone(),
            run_id.clone(),
            unit.plan_revision.clone(),
            crate::workflows::ArtifactKind::Plan,
        )
        .await?
        .context("Missing selected plan for Git provenance")?;
    let ArtifactValue::BacklogPlan(plan) = plan.value else {
        unreachable!()
    };
    let sdd = plan
        .source_artifacts
        .get("sdd")
        .context("The task lost its pinned SDD source")?;
    let message = self::git(&managed.root, &["show", "-s", "--format=%B", &commit.sha])?;
    for expected in [
        format!("OmniAgent-Task: {}", child.task_id),
        format!("OmniAgent-Backlog-Key: {}", child.key),
        format!("OmniAgent-Plan: {}", unit.plan_revision),
        format!("OmniAgent-SDD: {sdd}"),
        "OmniAgent-Requirements: [\"REQ-1\"]".into(),
    ] {
        assert!(
            message.lines().any(|line| line == expected),
            "Missing trailer {expected:?} in commit message {message:?}"
        );
    }
    Ok(FixedDelivery { unit, git, commit })
}

async fn merge_locally(store: &Store, child: &Child, delivery: FixedDelivery) -> Result<String> {
    let project = &delivery.unit.project_id;
    let preview = store
        .delivery_merge_preview(
            project.clone(),
            child.task_id.clone(),
            child.workflow_run_id.clone(),
            delivery.unit.unit.id.clone(),
            delivery.commit.id.clone(),
        )
        .await?;
    let result = store
        .execute_workflow_unit(WorkflowUnitSubmission {
            project_id: project.clone(),
            run_id: child.workflow_run_id.clone(),
            node_id: "merge".into(),
            unit_id: delivery.unit.unit.id,
            task_id: child.task_id.clone(),
            inputs: NodeInputs::from([("git".into(), delivery.git.id)]),
            request: WorkflowUnitRequest::VerifyMerge {
                expected_source_branch: preview.source_branch,
                expected_source_sha: preview.source_sha,
                destination_branch: preview.destination_branch,
                expected_destination_sha: preview.expected_destination_sha,
                external_sha: None,
                drift_acceptance_id: None,
                prepare_drift: false,
                perform: true,
            },
        })
        .await?;
    ensure!(
        matches!(
            result,
            WorkflowUnitOutcome::Artifact { .. } | WorkflowUnitOutcome::MergeVerified { .. }
        ),
        "The native merge did not verify: {result:?}"
    );
    let (project, task, commit) = (project.clone(), child.task_id.clone(), delivery.commit.id);
    let evidence = store
        .execute(move |db| {
            repository::verified_merge::load_verified_delivery_merge(db, &project, &task, &commit)
        })
        .await?
        .context("The merge receipt was not persisted")?;
    Ok(evidence.integration_sha)
}

async fn register_policy(store: &Store, project: &str) -> Result<()> {
    store
        .set_delivery_milestone_policy(
            project.into(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedMerge,
                destination_branch: Some(DESTINATION.into()),
            },
        )
        .await?;
    store
        .set_candidate_gate_policy(
            project.into(),
            vec![
                CandidateGate {
                    name: "fixture-required-source-check".into(),
                    required: true,
                    action: CandidateGateAction::Command {
                        executable: repository::git_path()?,
                        arguments: ["cat-file", "-e", "HEAD:brief.md"]
                            .map(str::to_owned)
                            .to_vec(),
                        timeout_seconds: Some(15),
                    },
                },
                CandidateGate {
                    name: QUALITY_CHECK.into(),
                    required: false,
                    action: CandidateGateAction::Command {
                        executable: repository::git_path()?,
                        arguments: ["grep", "-q", "^original first$", "HEAD", "--", "first.txt"]
                            .map(str::to_owned)
                            .to_vec(),
                        timeout_seconds: Some(15),
                    },
                },
            ],
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn backlog_advances_by_priority_only_after_merges_and_audits_the_final_sha() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("saga.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let root = Path::new(&repo.checkout_root);
    let project = &repo.project_id;
    git(root, &["branch", DESTINATION])?;
    let original = git(root, &["rev-parse", "HEAD"])?;
    register_policy(&store, project).await?;
    let run = launch_backlog(&store, &repo, false).await?;
    store.settle_ready_agile_stages().await?;
    let first = wait_for_children(&store, &run, 1).await?;
    assert_eq!(first[0].key, "B");
    store
        .execute_workflow_host(prepare_request(&store, project, &first[0], temp.path()).await?)
        .await?;
    let delivery = commit_child(&store, &repo, &first[0], "first.txt", "task B\n").await?;
    store.settle_workflow_backlogs().await?;
    assert_eq!(children(&store, &run).await?.len(), 1);
    assert!(store.next_workflow_integrated_checks().await?.is_none());
    drop(store);

    let store = Store::open(database)?;
    let first_merge = merge_locally(&store, &first[0], delivery).await?;
    let next = wait_for_children(&store, &run, 2).await?;
    let dependent = next
        .iter()
        .find(|child| child.key == "C")
        .context("C was not chosen after B merged")?;
    assert!(next.iter().all(|child| child.key != "A"));
    store
        .execute_workflow_host(prepare_request(&store, project, dependent, temp.path()).await?)
        .await?;
    let dependent_worktree = store
        .live_managed_worktree(project.clone(), dependent.task_id.clone())
        .await?
        .unwrap()
        .worktree;
    assert_eq!(dependent_worktree.base_commit, first_merge);
    let delivery = commit_child(&store, &repo, dependent, "second.txt", "task C\n").await?;
    merge_locally(&store, dependent, delivery).await?;
    let last = wait_for_children(&store, &run, 3).await?;
    let independent = last
        .iter()
        .find(|child| child.key == "A")
        .context("The remaining task was not launched")?;
    store
        .execute_workflow_host(prepare_request(&store, project, independent, temp.path()).await?)
        .await?;
    let delivery = commit_child(&store, &repo, independent, "last.txt", "task A\n").await?;
    let final_merge = merge_locally(&store, independent, delivery).await?;
    assert_ne!(final_merge, original);
    let audit = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            store.settle_workflow_backlogs().await?;
            if let Some(stage) = store.next_workflow_integrated_checks().await? {
                return Ok::<_, anyhow::Error>(stage);
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    })
    .await
    .context("The integrated audit was not scheduled")??;
    audit.execute(&store).await?;
    store.settle_ready_agile_stages().await?;
    let integrated = artifact(&store, project, &run.id, "execute", "integrated", None).await?;
    let ArtifactValue::IntegratedRevision(revision) = integrated.value else {
        unreachable!()
    };
    assert_eq!(revision.sha, final_merge);
    assert_eq!(revision.integration_shas.len(), 3);
    let output = artifact(&store, project, &run.id, "audit", "evidence", None).await?;
    let ArtifactValue::IntegratedChecksEvidence(audit) = output.value else {
        unreachable!()
    };
    assert_eq!(audit.revision.sha, final_merge);
    assert_eq!(
        audit.tree.as_deref(),
        Some(git(root, &["rev-parse", &format!("{final_merge}^{{tree}}")])?.as_str())
    );
    assert_eq!(
        audit.verdict,
        repository::verification::test_phase::TestVerdict::Failed
    );
    let remediation = artifact(&store, project, &run.id, "remediate", "backlog", None).await?;
    let ArtifactValue::Backlog(remediation) = remediation.value else {
        unreachable!()
    };
    assert_eq!(
        serde_json::to_value(&remediation.tasks[0])?["specification"]["key"],
        "FIX"
    );
    Ok(())
}

async fn run_github_action(store: &Store, client: &github_fixture::LocalGithub) -> Result<()> {
    store
        .next_workflow_github_action()
        .await?
        .context("No scoped GitHub stage is ready")?
        .execute_with_client(store, client)
        .await
}

pub(super) async fn advance_github_poll(
    store: &Store,
    run: &str,
    node: &str,
    request_id: Option<&str>,
) -> Result<()> {
    let (run, node, request_id) = (
        run.to_owned(),
        node.to_owned(),
        request_id.map(str::to_owned),
    );
    store.execute(move |db| {
        // Advance only the scheduler's timer. The result, request and decision are left intact.
        let changed = db.execute(
            "UPDATE workflow_github_actions SET retry_after = 0 WHERE pipeline_run_id = ?1
             AND node_instance_id = ?2 AND port IS NULL AND remote_request_id IS ?3 AND reason IS NOT NULL",
            rusqlite::params![run, node, request_id],
        )?;
        ensure!(changed == 1, "The pending GitHub action or request identity was lost on reopen");
        Ok(())
    }).await
}

#[tokio::test]
async fn failed_ci_reopens_the_same_task_and_high_risk_merge_requires_exact_reapproval()
-> Result<()> {
    use crate::workflows::{
        decision_nodes::{WorkflowDecisionSubmission, WorkflowStageDecision},
        github::CiState,
    };

    let temp = tempfile::tempdir()?;
    let db = temp.path().join("github-saga.sqlite");
    let store = Store::open(db.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let root = Path::new(&repo.checkout_root);
    let project = &repo.project_id;
    git(root, &["branch", DESTINATION])?;
    let base = git(root, &["rev-parse", "HEAD"])?;
    let gh = github_fixture::LocalGithub::new(root, base.clone());
    register_policy(&store, project).await?;
    store
        .set_workflow_github_settings(project.clone(), github_fixture::settings())
        .await?;
    let backlog = launch_backlog(&store, &repo, true).await?;
    store.settle_ready_agile_stages().await?;
    let first = wait_for_children(&store, &backlog, 1).await?;
    let task = &first[0];
    assert_eq!(task.key, "B");
    let child_run = &task.workflow_run_id;
    store
        .execute_workflow_host_with_github(
            prepare_request(&store, project, task, temp.path()).await?,
            &gh,
        )
        .await?;
    let first_delivery = commit_child(
        &store,
        &repo,
        task,
        "first.txt",
        github_fixture::FAILED_CONTENT,
    )
    .await?;
    run_github_action(&store, &gh).await?;
    let first_pull = artifact(
        &store,
        project,
        child_run,
        "pull",
        "pull",
        Some(&first_delivery.unit.unit.id),
    )
    .await?;
    let ArtifactValue::GithubPullRequest(first_pull) = first_pull.value else {
        unreachable!()
    };
    assert!(first_pull.requires_human);
    assert_eq!(first_pull.pull.head_sha, first_delivery.commit.sha);
    run_github_action(&store, &gh).await?;
    assert!(store.next_workflow_github_action().await?.is_none());
    drop(store);

    let store = Store::open(db.clone())?;
    advance_github_poll(&store, child_run, "ci", None).await?;
    run_github_action(&store, &gh).await?;
    let revised = store
        .workflow_run(project.clone(), child_run.clone())
        .await?
        .context("The child was lost after failed CI")?;
    let plan = revised
        .plan_revision
        .context("The child lost its selected repair plan")?;
    if plan == first_delivery.unit.plan_revision {
        let run = child_run.clone();
        let reason: Option<String> = store.execute(move |db| Ok(db.query_row(
            "SELECT reason FROM workflow_github_actions WHERE pipeline_run_id = ?1 AND node_instance_id = 'ci' AND port = 'failed'",
            [run], |row| row.get(0),
        )?)).await?;
        anyhow::bail!("Failed CI did not select a new plan: {reason:?}");
    }
    let failed_ci = store.workflow_run_artifacts(project.clone(), child_run.clone()).await?.into_iter()
        .find(|artifact| matches!(&artifact.value, ArtifactValue::GithubCi(ci) if ci.snapshot.state == CiState::Failed))
        .context("Failed CI evidence was not retained")?;
    let repair = store
        .resolve_workflow_artifact(
            project.clone(),
            child_run.clone(),
            plan.clone(),
            crate::workflows::ArtifactKind::Plan,
        )
        .await?
        .context("The revised plan is unavailable")?;
    let ArtifactValue::BacklogPlan(repair) = repair.value else {
        unreachable!()
    };
    let repair = repair
        .repair
        .context("The revised plan lost its failed CI pins")?;
    assert_eq!(repair.attempt, 1);
    assert_eq!(repair.failed_ci_artifact_id, failed_ci.id);
    assert_eq!(repair.previous_source_sha, first_delivery.commit.sha);
    store.settle_workflow_backlogs().await?;
    assert_eq!(children(&store, &backlog).await?.len(), 1);
    assert_eq!(gh.merges(), 0);
    assert!(store.next_workflow_github_action().await?.is_none());
    drop(store);

    let store = Store::open(db.clone())?;
    let waiting = prepare_request(&store, project, task, temp.path()).await?;
    store
        .execute_workflow_host_with_github(waiting, &gh)
        .await?;
    let next = commit_child(&store, &repo, task, "first.txt", "repaired B\n").await?;
    assert_eq!(next.unit.plan_revision, plan);
    assert_eq!(next.commit.branch, first_delivery.commit.branch);
    assert_ne!(next.commit.sha, first_delivery.commit.sha);
    run_github_action(&store, &gh).await?;
    run_github_action(&store, &gh).await?;
    let checked = artifact(
        &store,
        project,
        child_run,
        "ci",
        "ci",
        Some(&next.unit.unit.id),
    )
    .await?;
    let ArtifactValue::GithubCi(ci) = &checked.value else {
        unreachable!()
    };
    assert_eq!(ci.snapshot.state, CiState::Passed);
    assert_eq!(ci.pull.pull.number, first_pull.pull.number);
    assert_eq!(ci.pull.pull.head_sha, next.commit.sha);
    assert_eq!(ci.pull.pull.base_sha, base);
    assert!(ci.pull.requires_human);
    assert!(store.next_workflow_github_action().await?.is_none());
    assert_eq!(gh.merges(), 0);
    assert_eq!(children(&store, &backlog).await?.len(), 1);
    let view = store
        .inspect_workflow_execution(project.clone(), child_run.clone())
        .await?
        .unwrap();
    let approval = view
        .pending_decisions
        .iter()
        .find(|request| request.node_id == "release-approval")
        .context("This high-risk CI result requires an operator")?;
    assert_eq!(approval.inputs["evidence"], checked.id);
    assert_eq!(approval.plan_revision.as_ref(), Some(&plan));
    store
        .resolve_workflow_stage_decision(WorkflowDecisionSubmission {
            project_id: project.clone(),
            run_id: child_run.clone(),
            request_id: approval.request_id.clone(),
            decision: WorkflowStageDecision::Approval { approved: true },
        })
        .await?;
    run_github_action(&store, &gh).await?;
    assert_eq!(gh.merges(), 0);
    assert!(store.next_workflow_github_action().await?.is_none());
    drop(store);

    let store = Store::open(db)?;
    advance_github_poll(
        &store,
        child_run,
        "merge",
        Some(github_fixture::MERGE_REQUEST),
    )
    .await?;
    run_github_action(&store, &gh).await?;
    assert_eq!(gh.merges(), 1);
    let merged = artifact(
        &store,
        project,
        child_run,
        "merge",
        "integration",
        Some(&next.unit.unit.id),
    )
    .await?;
    let ArtifactValue::Merge(merge) = merged.value else {
        unreachable!()
    };
    assert_eq!(merge.commit_evidence_id, next.commit.id);
    let launched = wait_for_children(&store, &backlog, 2).await?;
    let dependent = launched
        .iter()
        .find(|child| child.key == "C")
        .context("C remained blocked after the verified merge")?;
    store
        .execute_workflow_host_with_github(
            prepare_request(&store, project, dependent, temp.path()).await?,
            &gh,
        )
        .await?;
    let dependent_checkout = store
        .live_managed_worktree(project.clone(), dependent.task_id.clone())
        .await?
        .unwrap()
        .worktree;
    assert_eq!(
        dependent_checkout.base_commit,
        git(root, &["rev-parse", DESTINATION])?
    );
    assert_eq!(
        fs::read_to_string(dependent_checkout.root.join("first.txt"))?.trim(),
        "repaired B"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_native_build_routes_to_a_new_frozen_candidate_before_checks_can_pass()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("build-repair.sqlite"))?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let root = Path::new(&repo.checkout_root);
    git(root, &["branch", DESTINATION])?;
    register_policy(&store, project).await?;
    let name = "fixture-build";
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: name.into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: repository::git_path()?,
                    arguments: ["grep", "-q", "^build ready$", "HEAD", "--", "first.txt"]
                        .map(str::to_owned)
                        .to_vec(),
                    timeout_seconds: Some(15),
                },
            }],
        )
        .await?;
    let mut graph = child_graph(false)?;
    graph.nodes.retain(|node| {
        !matches!(
            node.id.as_str(),
            "review-candidate" | "accept" | "commit" | "merge"
        )
    });
    graph.wires.retain(|edge| {
        graph.nodes.iter().any(|node| node.id == edge.from.node)
            && graph.nodes.iter().any(|node| node.id == edge.to.node)
    });
    let check = NodeConfiguration::RequiredChecks {
        names: vec![name.into()],
        parallel_groups: Vec::new(),
    };
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "verify-candidate")
        .unwrap()
        .configuration = check.clone();
    let mut fix = graph
        .nodes
        .iter()
        .find(|node| node.id == "implement")
        .unwrap()
        .clone();
    fix.id = "fix-build".into();
    graph.nodes.push(fix);
    graph
        .nodes
        .push(node("verify-fix", BuiltinNode::VerifyCandidate, check));
    graph.wires.extend([
        wire(("verify-candidate", "failed"), ("fix-build", "ready")),
        wire(("start", "plan"), ("fix-build", "plan")),
        wire(("prepare", "worktree"), ("fix-build", "worktree")),
        wire(("prepare", "release"), ("fix-build", "release")),
        wire(("fix-build", "implemented"), ("verify-fix", "ready")),
        wire(("fix-build", "candidate"), ("verify-fix", "candidate")),
    ]);
    let child = store
        .save_workflow_revision(project.clone(), "saga-build-repair".into(), graph)
        .await?;
    let parent = launch_backlog_for(&store, &repo, child, "fix-build").await?;
    store.settle_ready_agile_stages().await?;
    let children = wait_for_children(&store, &parent, 1).await?;
    let task = &children[0];
    store
        .execute_workflow_host(prepare_request(&store, project, task, temp.path()).await?)
        .await?;
    let (run, unit, managed) = current_child(&store, &repo, task).await?;
    let mut candidates = Vec::new();
    for (writer, content, checked, passed) in [
        ("implement", "build broken\n", "verify-candidate", false),
        ("fix-build", "build ready\n", "verify-fix", true),
    ] {
        let candidate = capture_child(
            &store,
            &repo,
            (&run, &unit, &managed),
            writer,
            ("first.txt", content),
        )
        .await?;
        let ArtifactValue::Candidate(candidate) = candidate.value else {
            unreachable!()
        };
        store
            .next_workflow_unit_action()
            .await?
            .context("The candidate has no pending build check")?
            .execute(&store)
            .await?;
        let checks = artifact(
            &store,
            project,
            &run.id,
            checked,
            "checks",
            Some(&unit.unit.id),
        )
        .await?;
        let ArtifactValue::Checks(checks) = checks.value else {
            unreachable!()
        };
        let verification = store
            .candidate_verification(project.clone(), task.task_id.clone(), checks.id)
            .await?
            .context("The native build verification is unavailable")?;
        assert_eq!(verification.candidate_id, candidate.id);
        assert_eq!(verification.allows_acceptance(), passed);
        let view = store
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .unwrap();
        let receipt = view
            .control
            .iter()
            .find(|entry| entry.node_id == checked)
            .context("The build verdict was not finalized")?;
        assert_eq!(
            receipt.selected_ports,
            [if passed { "verified" } else { "failed" }]
        );
        assert!(view.outputs.iter().all(|entry| !matches!(
            entry.artifact.value,
            ArtifactValue::Acceptance(_) | ArtifactValue::Git(_)
        )));
        candidates.push(candidate);
    }
    assert_ne!(candidates[0].tree, candidates[1].tree);
    assert_eq!(candidates[0].base_commit, candidates[1].base_commit);
    Ok(())
}
