use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    providers::SessionMode,
    repository::{
        ProjectCheckout,
        verification::{CandidateGateOutcome, test_phase::TestVerdict},
    },
    store::Store,
    tasks::{DeliveryCompletionMilestone, DeliveryMilestonePolicy},
    workflows::{
        ArtifactKind, ArtifactValue, NodeInputs, WorkflowApprovalStage, WorkflowHostRequest,
        WorkflowHostSubmission, WorkflowRun,
        backlog::execution::{self as backlog_execution, BacklogExecution},
        decision_nodes::{
            WorkflowDecisionSubmission, WorkflowPendingDecision, WorkflowStageDecision,
        },
        github::{CiState, GithubDeliveryClient},
        inspection::WorkflowExecutionInspection,
    },
};

use super::{
    super::{
        backlog_saga::{
            DESTINATION, advance_github_poll,
            github_fixture::{LocalGithub, MERGE_REQUEST, settings},
        },
        git,
    },
    FixtureScript,
};

mod scenario;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Child {
    key: String,
    task_id: String,
    workflow_run_id: String,
}

#[derive(Default)]
struct Progress {
    children: BTreeMap<String, Child>,
    prepared: BTreeSet<(String, String)>,
    initial_launch: bool,
    resumed_ci: bool,
    resumed_merge: bool,
    resumed_repair: bool,
    approved_pulls: Vec<String>,
}

pub(super) async fn run_delivery_and_audit(
    store: &mut Store,
    database: &Path,
    temp: &Path,
    repo: &ProjectCheckout,
    run: &WorkflowRun,
    script: &FixtureScript,
    gh: &LocalGithub,
) -> Result<()> {
    scenario::register_checks(store, &repo.project_id).await?;
    store
        .set_delivery_milestone_policy(
            repo.project_id.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedMerge,
                destination_branch: Some(DESTINATION.into()),
            },
        )
        .await?;
    store
        .set_workflow_github_settings(repo.project_id.clone(), settings())
        .await?;
    store.set_job_queue_capacity(3).await?;
    let root = Path::new(&repo.checkout_root);
    let mut progress = Progress::default();
    let release = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            let started = store.settle_ready_agile_stages().await?;
            store.settle_workflow_backlogs().await?;
            let executions = store
                .execute({
                    let run = run.clone();
                    move |db| backlog_execution::inspect(db, &run)
                })
                .await?;
            if let Err(error) = progress.observe_children(&executions) {
                return Err(
                    error.context(progress.failure_evidence(store, &repo.project_id).await?)
                );
            }
            if has_decision(store, &run.id, "%-release-approval").await? {
                let view = inspect(store, &repo.project_id, &run.id).await?;
                let decision = view
                    .pending_decisions
                    .iter()
                    .find(|decision| decision.node_id.ends_with("-release-approval"))
                    .context(
                        "The final release gate is awaiting unavailable or superseded evidence",
                    )?;
                let release = prove_release(store, run, root, &view, decision).await?;
                approve_gate(store, &repo.project_id, &run.id, decision).await?;
                break Ok::<_, anyhow::Error>(release);
            }

            if resume_github_poll(store, database, run, &mut progress).await? {
                continue;
            }
            if progress
                .prepare_child(store, database, temp, repo, gh)
                .await?
            {
                continue;
            }
            if progress.approve_pull(store, repo, gh).await? {
                continue;
            }
            if let Some(action) = store.next_workflow_github_action().await? {
                action.execute_with_client(store, gh).await?;
                continue;
            }
            if let Some(phase) = store.next_workflow_test_phase().await? {
                phase.execute(store).await?;
                continue;
            }
            if let Some(delivery) = store.next_workflow_delivery_result().await? {
                delivery.publish(store).await?;
                continue;
            }
            if let Some(action) = store.next_workflow_unit_action().await? {
                action.execute(store).await?;
                continue;
            }
            if let Some(action) = store.next_workflow_integrated_checks().await? {
                action.execute(store).await?;
                continue;
            }
            if let Some(pending) = store.next_workflow_agent().await? {
                let (workflow, node) = pending.fixture_identity();
                let attempt = script
                    .claim(store, repo, &pending)
                    .await?
                    .context("Delivery and audit should enqueue their selected specialists")?;
                if let Some(child) = progress.children.get(workflow) {
                    let repair =
                        scenario::repair_attempt(store, &repo.project_id, workflow).await?;
                    if attempt.scope.mode == SessionMode::ApprovedWrite {
                        scenario::write_candidate(store, &attempt, child, node, repair).await?;
                    } else {
                        let answer = scenario::answer_child(
                            store,
                            &repo.project_id,
                            &attempt,
                            child,
                            node,
                            repair,
                        )
                        .await?;
                        script
                            .complete_response(store, repo, &attempt, &answer)
                            .await?;
                    }
                } else {
                    ensure!(
                        workflow == run.id,
                        "Unexpected fixture agent run {workflow}"
                    );
                    let answer = scenario::answer_audit(root, &attempt, node)?;
                    script
                        .complete_response(store, repo, &attempt, &answer)
                        .await?;
                }
                continue;
            }
            if started == 0 {
                tokio::time::sleep(Duration::from_millis(65)).await;
            }
        }
    })
    .await;
    let (final_sha, evidence_id) = match release {
        Ok(release) => release?,
        Err(_) => {
            let view = inspect(store, &repo.project_id, &run.id).await?;
            let status = serde_json::to_value(view.backlog_executions)?;
            let waits = serde_json::to_value(view.automatic_agent_waits)?;
            let children = progress.failure_evidence(store, &repo.project_id).await?;
            anyhow::bail!(
                "The published Audit factory did not reach release readiness, backlog {status}, pending {waits}, children {children}"
            );
        }
    };
    let view = inspect(store, &repo.project_id, &run.id).await?;
    ensure!(
        view.control
            .iter()
            .any(|stage| stage.node_id.ends_with("-release-approval")
                && stage.selected_ports == ["approved"]),
        "The native release gate did not retain approval for {final_sha}"
    );
    ensure!(
        view.outputs.iter().any(|output| output
            .artifact
            .node_instance_id
            .ends_with("-release-ready")
            && matches!(&output.artifact.value, ArtifactValue::ResultSummary { artifacts, .. }
                if artifacts.contains(&evidence_id))),
        "The final readiness summary omitted the evidence approved for {final_sha}"
    );
    progress.prove_history(store, repo, gh).await?;
    if script.adaptive {
        progress
            .prove_adaptive_reviews(store, &repo.project_id)
            .await?;
    }
    Ok(())
}

impl Progress {
    async fn prove_adaptive_reviews(&self, store: &Store, project: &str) -> Result<()> {
        for (task_key, specialty) in [("A", "maintainability"), ("B", "security")] {
            let child = self
                .children
                .values()
                .find(|child| child.key == task_key)
                .with_context(|| format!("Missing adaptive task {task_key}"))?;
            let artifacts = store
                .workflow_run_artifacts(project.into(), child.workflow_run_id.clone())
                .await?;
            let decision = artifacts
                .iter()
                .find_map(|artifact| {
                    if artifact
                        .node_instance_id
                        .ends_with("::assess-candidate::outcome")
                        && artifact.port == "outcome"
                        && let ArtifactValue::JsonObject(proof) = &artifact.value
                        && proof.get("approved").and_then(Value::as_bool) == Some(true)
                    {
                        Some(proof)
                    } else {
                        None
                    }
                })
                .with_context(|| format!("Missing reviewed adaptive candidate for {task_key}"))?;
            let calls = decision["calls"]
                .as_array()
                .context("Missing selected review receipts")?;
            let completed = calls
                .iter()
                .filter(|call| call["status"] == "completed")
                .filter_map(|call| call["role"].as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(completed, BTreeSet::from(["correctness", specialty]));
            for call in calls {
                let role = call["role"].as_str().context("Missing role alias")?;
                if completed.contains(role) {
                    assert!(call["artifactId"].is_string());
                    assert_eq!(call["approved"], true);
                } else {
                    assert_eq!(call["status"], "skipped");
                    assert!(call["attemptId"].is_null());
                }
            }
        }
        Ok(())
    }

    async fn failure_evidence(&self, store: &Store, project: &str) -> Result<String> {
        let mut failures = Vec::new();
        for child in self.children.values() {
            let view = inspect(store, project, &child.workflow_run_id).await?;
            for receipt in view.control.iter().filter(|receipt| {
                receipt
                    .selected_ports
                    .iter()
                    .any(|port| matches!(port.as_str(), "failed" | "blocked"))
            }) {
                failures.push(format!(
                    "{} {} {:?} {:?}",
                    child.key, receipt.node_id, receipt.selected_ports, receipt.reason
                ));
            }
            for output in view.outputs.iter().filter(|output| output.current) {
                if let ArtifactValue::TestPhaseEvidence(evidence) = &output.artifact.value
                    && evidence.result.verdict != TestVerdict::Verified
                {
                    failures.push(format!(
                        "{} {} {:?}",
                        child.key, output.artifact.node_instance_id, evidence.result.reason
                    ));
                }
            }
        }
        Ok(failures.join("\n"))
    }

    fn observe_children(&mut self, executions: &[BacklogExecution]) -> Result<()> {
        let executions = serde_json::to_value(executions)?;
        for stage in executions.as_array().into_iter().flatten() {
            ensure!(
                stage["state"] != "failed",
                "The published backlog {} failed: {}",
                stage["nodeId"],
                stage["reason"]
            );
            let children: Vec<Child> = serde_json::from_value(stage["tasks"].clone())?;
            if stage["nodeId"] == "deliver-backlog" && !children.is_empty() && !self.initial_launch
            {
                let keys: BTreeSet<_> = children.iter().map(|child| child.key.as_str()).collect();
                ensure!(
                    keys == BTreeSet::from(["A", "B"]),
                    "The two independent children should start while C waits: {keys:?}"
                );
                self.initial_launch = true;
            }
            self.children.extend(
                children
                    .into_iter()
                    .map(|child| (child.workflow_run_id.clone(), child)),
            );
        }
        Ok(())
    }

    async fn prepare_child(
        &mut self,
        store: &mut Store,
        database: &Path,
        temp: &Path,
        repo: &ProjectCheckout,
        gh: &LocalGithub,
    ) -> Result<bool> {
        let project = &repo.project_id;
        for child in self.children.values() {
            let current = store
                .workflow_run(project.clone(), child.workflow_run_id.clone())
                .await?
                .context("A reserved child workflow disappeared")?;
            if current.plan_revision.is_some_and(|plan| {
                self.prepared
                    .contains(&(child.workflow_run_id.clone(), plan))
            }) {
                continue;
            }
            let scope = store
                .workflow_approval_stage(
                    project.clone(),
                    child.workflow_run_id.clone(),
                    "approve-task-scope".into(),
                )
                .await?;
            let WorkflowApprovalStage::Ready {
                plan_id,
                task_artifact_id,
                ..
            } = scope
            else {
                continue;
            };
            if self
                .prepared
                .contains(&(child.workflow_run_id.clone(), plan_id.clone()))
            {
                continue;
            }
            if child.key == "C" {
                let prerequisite = self
                    .children
                    .values()
                    .find(|item| item.key == "B")
                    .context("C lost predecessor B")?;
                let before = inspect(store, project, &prerequisite.workflow_run_id).await?;
                ensure!(
                    before.outputs.iter().any(|output| output.current
                        && matches!(output.artifact.value, ArtifactValue::Merge(_))),
                    "C became eligible before B's checked merge"
                );
            }
            let repair = scenario::repair_attempt(store, project, &child.workflow_run_id).await?;
            if child.key == "B" && repair > 0 && !self.resumed_repair {
                *store = Store::open(database.into())?;
                self.resumed_repair = true;
                return Ok(true);
            }
            let submit = |node: &str, inputs, request| WorkflowHostSubmission {
                project_id: project.clone(),
                run_id: child.workflow_run_id.clone(),
                node_id: node.into(),
                inputs,
                request,
            };
            let approved = store
                .execute_workflow_host(submit(
                    "approve-task-scope",
                    NodeInputs::from([
                        ("plan".into(), plan_id.clone()),
                        ("task".into(), task_artifact_id.clone()),
                    ]),
                    WorkflowHostRequest::ApproveImplementation,
                ))
                .await?;
            store
                .execute_workflow_host_with_github(
                    submit(
                        "prepare-task-worktree",
                        NodeInputs::from([
                            ("decision".into(), approved.id),
                            ("task".into(), task_artifact_id),
                        ]),
                        WorkflowHostRequest::PrepareWorktree {
                            default_root: temp.join("managed"),
                        },
                    ),
                    gh,
                )
                .await?;
            if child.key == "C" {
                let inherited = store
                    .live_managed_worktree(project.clone(), child.task_id.clone())
                    .await?
                    .context("The dependent task lost its prepared worktree")?;
                ensure!(
                    std::fs::read_to_string(inherited.worktree.root.join("first.txt"))?.trim()
                        == scenario::REPAIRED_CONTENT,
                    "C's real worktree does not contain B's verified CI repair"
                );
            }
            self.prepared
                .insert((child.workflow_run_id.clone(), plan_id));
            return Ok(true);
        }
        Ok(false)
    }

    async fn approve_pull(
        &mut self,
        store: &Store,
        repo: &ProjectCheckout,
        gh: &LocalGithub,
    ) -> Result<bool> {
        let project = &repo.project_id;
        for child in self.children.values() {
            if !has_decision(store, &child.workflow_run_id, "%::approve-high-risk-merge").await? {
                continue;
            }
            let view = inspect(store, project, &child.workflow_run_id).await?;
            let Some(decision) = view
                .pending_decisions
                .iter()
                .find(|decision| decision.node_id.ends_with("::approve-high-risk-merge"))
            else {
                continue;
            };
            let id = decision
                .inputs
                .get("evidence")
                .context("The operator gate has no CI input")?;
            let evidence = store
                .resolve_workflow_artifact(
                    project.clone(),
                    child.workflow_run_id.clone(),
                    id.clone(),
                    ArtifactKind::GithubCi,
                )
                .await?
                .context("The exact checked revision is unavailable")?;
            let ArtifactValue::GithubCi(ci) = evidence.value else {
                unreachable!()
            };
            ensure!(
                child.key == "B"
                    && ci.pull.requires_human
                    && ci.snapshot.state == CiState::Passed
                    && ci.snapshot.head_sha == ci.pull.pull.head_sha
                    && ci.snapshot.base_sha == ci.pull.pull.base_sha
                    && !gh
                        .pull_request(
                            &crate::repository::github::RepositorySlug {
                                owner: "fixture".into(),
                                name: "backlog".into()
                            },
                            ci.pull.pull.number
                        )
                        .await?
                        .merged,
                "The human gate must approve B's exact passed CI while its pull request is still open"
            );
            ensure!(
                decision.unit_id == evidence.unit_id
                    && decision.plan_revision == view.run.plan_revision,
                "The high-risk decision crossed delivery scopes"
            );
            approve_gate(store, project, &child.workflow_run_id, decision).await?;
            self.approved_pulls.push(id.clone());
            return Ok(true);
        }
        Ok(false)
    }

    async fn prove_history(
        &self,
        store: &Store,
        repo: &ProjectCheckout,
        gh: &LocalGithub,
    ) -> Result<()> {
        ensure!(
            self.initial_launch && self.resumed_ci && self.resumed_merge && self.resumed_repair,
            "The fixture did not exercise all of its launch and restart boundaries"
        );
        ensure!(
            self.approved_pulls.len() == 1 && gh.merges() == 4,
            "The factory lost a scoped approval or performed an extra merge"
        );
        ensure!(
            self.children.len() == 4
                && self
                    .children
                    .values()
                    .any(|child| child.key.starts_with("integration-")),
            "The native audit must materialize and deliver one remediation task"
        );
        let b = self
            .children
            .values()
            .find(|child| child.key == "B")
            .context("The fixture lost child B")?;
        let view = inspect(store, &repo.project_id, &b.workflow_run_id).await?;
        let mut failure = false;
        let mut passed = false;
        let mut failed_ci = BTreeSet::new();
        let mut approved_ci = None;
        for output in view.outputs {
            match output.artifact.value {
                ArtifactValue::Checks(checks)
                    if output
                        .artifact
                        .node_instance_id
                        .ends_with("::build-approved-tree") =>
                {
                    let verification = store
                        .candidate_verification(
                            repo.project_id.clone(),
                            b.task_id.clone(),
                            checks.id,
                        )
                        .await?
                        .context("The build proof is unavailable after restart")?;
                    for gate in verification
                        .gates
                        .iter()
                        .filter(|gate| gate.gate.name == "Pipeline build")
                    {
                        failure |=
                            matches!(gate.outcome, Some(CandidateGateOutcome::Failed { .. }));
                        passed |= matches!(gate.outcome, Some(CandidateGateOutcome::Passed { .. }));
                    }
                }
                ArtifactValue::GithubCi(ci) => {
                    if ci.snapshot.state == CiState::Failed {
                        failed_ci.insert((ci.snapshot.pull_number, ci.snapshot.head_sha));
                    } else if self.approved_pulls.contains(&output.artifact.id) {
                        approved_ci = Some((ci.snapshot.pull_number, ci.snapshot.head_sha));
                    }
                }
                _ => {}
            }
        }
        ensure!(
            failure && passed,
            "The factory lost the failed build or passing repair build evidence"
        );
        ensure!(
            approved_ci.is_some_and(|(approved_pull, approved_head)| failed_ci
                .iter()
                .any(|(failed_pull, failed_head)| *failed_pull == approved_pull
                    && *failed_head != approved_head)),
            "The approved repair did not preserve B's pull request across the failed and passing revisions"
        );
        Ok(())
    }
}

async fn inspect(store: &Store, project: &str, run: &str) -> Result<WorkflowExecutionInspection> {
    store
        .inspect_workflow_execution(project.into(), run.into())
        .await?
        .context("The factory execution disappeared")
}

async fn has_decision(store: &Store, run: &str, node: &str) -> Result<bool> {
    let (run, node) = (run.to_owned(), node.to_owned());
    store.execute(move |db| Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_stage_decisions decision JOIN job_pipeline_runs run
           ON run.id = decision.pipeline_run_id AND coalesce(run.plan_revision, '') = decision.plan_key
         WHERE decision.pipeline_run_id = ?1 AND decision.node_instance_id LIKE ?2 AND decision.resolution IS NULL)",
        rusqlite::params![run, node], |row| row.get(0),
    )?)).await
}

async fn approve_gate(
    store: &Store,
    project: &str,
    run: &str,
    decision: &WorkflowPendingDecision,
) -> Result<()> {
    store
        .resolve_workflow_stage_decision(WorkflowDecisionSubmission {
            project_id: project.into(),
            run_id: run.into(),
            request_id: decision.request_id.clone(),
            decision: WorkflowStageDecision::Approval { approved: true },
        })
        .await?;
    Ok(())
}

async fn resume_github_poll(
    store: &mut Store,
    database: &Path,
    parent: &WorkflowRun,
    progress: &mut Progress,
) -> Result<bool> {
    let runs: BTreeSet<_> = progress.children.keys().cloned().collect();
    let project = parent.project_id.clone();
    let pending = store.execute(move |db| {
        let mut statement = db.prepare(
            "SELECT a.pipeline_run_id, a.node_instance_id, a.remote_request_id FROM workflow_github_actions a
             JOIN job_pipeline_runs w ON w.id = a.pipeline_run_id AND w.plan_revision = a.plan_key
             WHERE w.project_id = ?1 AND a.port IS NULL AND a.retry_after > unixepoch()
               AND (a.remote_request_id = ?2 OR a.reason = 'Fixture CI is still running')
             ORDER BY a.retry_after",
        )?;
        let rows = statement.query_map(rusqlite::params![project, MERGE_REQUEST], |row| Ok((
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?,
        )))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().find(|(run, _, _)| runs.contains(run)))
    }).await?;
    let Some((run, node, request)) = pending else {
        return Ok(false);
    };
    let restarted = if request.is_some() {
        &mut progress.resumed_merge
    } else {
        &mut progress.resumed_ci
    };
    if !*restarted {
        *store = Store::open(database.into())?;
        *restarted = true;
    }
    advance_github_poll(store, &run, &node, request.as_deref()).await?;
    Ok(true)
}

async fn prove_release(
    store: &Store,
    run: &WorkflowRun,
    root: &Path,
    view: &WorkflowExecutionInspection,
    pending: &WorkflowPendingDecision,
) -> Result<(String, String)> {
    let sha = git(root, &["rev-parse", DESTINATION])?;
    git(
        root,
        &["cat-file", "-e", &format!("{sha}:integration-guide.txt")],
    )?;
    let initial = view
        .outputs
        .iter()
        .find_map(|output| match &output.artifact.value {
            ArtifactValue::IntegratedRevision(revision)
                if output.artifact.node_instance_id == "deliver-backlog" =>
            {
                Some(&revision.sha)
            }
            _ => None,
        })
        .context("The original integrated proof is unavailable")?;
    ensure!(
        *initial != sha,
        "The factory approved the revision before remediation"
    );
    let evidence_id = pending
        .inputs
        .get("evidence")
        .context("The release decision lacked pinned evidence")?;
    let evidence = store
        .resolve_workflow_artifact(
            run.project_id.clone(),
            run.id.clone(),
            evidence_id.clone(),
            ArtifactKind::EvidenceSet,
        )
        .await?
        .context("The release evidence is unavailable")?;
    let ArtifactValue::EvidenceSet(items) = evidence.value else {
        unreachable!()
    };
    let values = serde_json::to_value(items)?;
    ensure!(
        values
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item.pointer("/value/kind")
                == Some(&Value::String("integrated_revision".into()))
                && item.pointer("/value/evidence/sha").and_then(Value::as_str) == Some(&sha)),
        "The release operator did not receive the remediated integration SHA"
    );
    ensure!(view.outputs.iter().any(|output| output.artifact.node_instance_id.ends_with("-build") &&
        matches!(&output.artifact.value, ArtifactValue::IntegratedChecksEvidence(proof) if proof.revision.sha == sha && proof.verdict == TestVerdict::Verified)),
        "The release decision did not use a passing build at the final SHA");
    ensure!(view.outputs.iter().any(|output| output.artifact.node_instance_id.ends_with("::regression") &&
        matches!(&output.artifact.value, ArtifactValue::IntegratedChecksEvidence(proof) if proof.revision.sha == sha && proof.verdict == TestVerdict::Verified)),
        "The release decision did not use passing integrated regression at the final SHA");
    Ok((sha, evidence_id.clone()))
}
