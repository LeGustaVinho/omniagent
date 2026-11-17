use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::attempts::{CiRepairBudget, ci_repair_budget};

use crate::{
    repository::{
        IntegratedWorktreeBase, registered_checkout_head,
        verified_git::load_verified_delivery_commit, verified_merge::DeliveryMergeRequest,
    },
    tasks::delivery_merge_candidate,
    workflows::{
        ArtifactKind, ArtifactValue, BuiltinNode,
        artifacts::{insert_artifact, load_artifact},
        execution::record_host_completion,
        github::{CiEvidence, CiState, validate_published_pull_source},
        inspection::record_workflow_input_ids,
        launch::load_workflow_launch,
        runtime::{load_pinned_node, load_workflow_run},
        units::{load_delivery_unit, validate_selected_plan_revision},
    },
};

const MAX_CI_REPAIRS: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BacklogCiRepair {
    pub attempt: u16,
    pub previous_plan_revision: String,
    pub failed_ci_artifact_id: String,
    pub previous_source_sha: String,
    pub destination_sha: String,
}

struct ChildScope {
    parent: String,
    backlog: String,
    task: String,
    destination: String,
}

pub(crate) enum BacklogCiRetry {
    Replanned,
    Unavailable,
    BudgetExhausted { limit: u16, head_sha: String },
}

/// Called after Wait for CI stores a failed result and before refreshing control receipts.
pub(crate) fn retry_backlog_ci_failure_in_tx(
    db: &Connection,
    project: &str,
    child_run: &str,
    failed_ci_artifact_id: &str,
) -> Result<BacklogCiRetry> {
    let child: Option<ChildScope> = db.query_row(
        "SELECT t.pipeline_run_id, t.backlog_artifact_id, t.task_id, t.destination_branch
         FROM workflow_backlog_task_runs t
         JOIN workflow_backlog_executions e ON e.pipeline_run_id = t.pipeline_run_id
           AND e.node_instance_id = t.node_instance_id AND e.backlog_artifact_id = t.backlog_artifact_id
         JOIN job_pipeline_runs p ON p.id = e.pipeline_run_id AND coalesce(p.plan_revision, '') = e.plan_key
         WHERE p.project_id = ?1 AND t.child_run_id = ?2 AND e.state = 'pending'",
        params![project, child_run],
        |row| Ok(ChildScope { parent: row.get(0)?, backlog: row.get(1)?, task: row.get(2)?, destination: row.get(3)? }),
    ).optional()?;
    let Some(child) = child else {
        return Ok(BacklogCiRetry::Unavailable);
    };
    let run = load_workflow_run(db, project, child_run)?
        .context("The backlog child workflow disappeared")?;
    let current = run
        .plan_revision
        .clone()
        .context("The backlog child has no selected plan")?;
    let plan_artifact = load_artifact(db, project, child_run, &current, ArtifactKind::Plan)?
        .context("The selected backlog plan is unavailable")?;
    let ArtifactValue::BacklogPlan(mut plan) = plan_artifact.value else {
        return Ok(BacklogCiRetry::Unavailable);
    };
    ensure!(
        plan.parent_run_id == child.parent
            && plan.backlog_artifact_id == child.backlog
            && plan.task.task.project_id == project
            && plan.task.task.task_id == child.task
            && plan.destination_branch == child.destination,
        "The active backlog plan no longer belongs to the child task and destination"
    );
    if plan
        .repair
        .as_ref()
        .is_some_and(|repair| repair.failed_ci_artifact_id == failed_ci_artifact_id)
    {
        return Ok(BacklogCiRetry::Replanned);
    }
    let start = &plan_artifact.node_instance_id;
    ensure!(
        load_pinned_node(db, project, child_run, start)?
            .is_some_and(|node| node.instance.kind == BuiltinNode::TypedStart),
        "The backlog plan lost its native start node"
    );

    let proof_artifact = load_artifact(
        db,
        project,
        child_run,
        failed_ci_artifact_id,
        ArtifactKind::GithubCi,
    )?
    .context("The failed GitHub CI evidence is unavailable")?;
    let ArtifactValue::GithubCi(ci) = proof_artifact.value else {
        return Ok(BacklogCiRetry::Unavailable);
    };
    let Some(unit_id) = proof_artifact.unit_id else {
        return Ok(BacklogCiRetry::Unavailable);
    };
    let terminal = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_github_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3
           AND plan_key = ?4 AND port = 'failed' AND output_id = ?5)",
        params![
            child_run,
            proof_artifact.node_instance_id,
            unit_id,
            current,
            failed_ci_artifact_id
        ],
        |row| row.get::<_, bool>(0),
    )?;
    if !terminal || proof_artifact.port != "ci" || !failed_check_for_revision(&ci) {
        return Ok(BacklogCiRetry::Unavailable);
    }
    ensure!(
        load_pinned_node(db, project, child_run, &proof_artifact.node_instance_id)?
            .is_some_and(|node| node.instance.kind == BuiltinNode::WaitForCi),
        "The retry must use the native failed CI result"
    );
    let pull_artifact = load_artifact(
        db,
        project,
        child_run,
        &ci.pull_artifact_id,
        ArtifactKind::GithubPullRequest,
    )?
    .context("The failed CI result lost its pull request evidence")?;
    ensure!(
        pull_artifact.unit_id.as_deref() == Some(&unit_id)
            && pull_artifact.value == ArtifactValue::GithubPullRequest(ci.pull.clone()),
        "The failed CI result differs from its recorded pull request"
    );

    let commit =
        load_verified_delivery_commit(db, project, &child.task, &ci.pull.commit_evidence_id)?
            .context("The failed GitHub head has no verified delivery commit")?;
    let unit = load_delivery_unit(db, project, &child.task, &unit_id)?
        .context("The failed CI evidence is outside the active delivery")?;
    let candidate = delivery_merge_candidate(db, project, &child.task, child_run)?
        .context("The retry needs the active final verified delivery chain")?;
    let pull = &ci.pull.pull;
    ensure!(
        unit.workflow_run_id == child_run
            && unit.plan_revision == current
            && unit.checkout_id == ci.pull.checkout_id
            && unit.commit_evidence_id.as_deref() == Some(&commit.id)
            && ci.pull.task_id == child.task
            && candidate.commit_evidence_id == commit.id
            && candidate.source_sha == commit.sha
            && commit.checkout_id == ci.pull.checkout_id
            && commit.branch == pull.head_branch
            && pull.open
            && !pull.merged
            && !pull.draft
            && pull.base_branch == child.destination,
        "The failed CI result does not match the approved delivery head, checkout and destination"
    );
    require_published_head(db, project, child_run, &current, &ci, &child)?;
    let request = DeliveryMergeRequest {
        project_id: project.into(),
        task_id: child.task.clone(),
        workflow_run_id: child_run.into(),
        unit_id: unit_id.clone(),
        commit_evidence_id: commit.id.clone(),
        expected_source_branch: commit.branch.clone(),
        expected_source_sha: commit.sha.clone(),
        destination_branch: child.destination.clone(),
        expected_destination_sha: pull.base_sha.clone(),
        external_sha: None,
        drift_acceptance_id: None,
        prepare_drift: false,
        perform: false,
    };
    validate_published_pull_source(db, project, child_run, &current, &ci.pull, &request)?;

    match ci_repair_budget(db, &run)? {
        CiRepairBudget::Exhausted(limit) => {
            return Ok(BacklogCiRetry::BudgetExhausted {
                limit,
                head_sha: pull.head_sha.clone(),
            });
        }
        CiRepairBudget::NoCurrentAttempt => return Ok(BacklogCiRetry::Unavailable),
        CiRepairBudget::Unmarked
            if plan
                .repair
                .as_ref()
                .is_some_and(|repair| repair.attempt >= MAX_CI_REPAIRS) =>
        {
            return Ok(BacklogCiRetry::Unavailable);
        }
        CiRepairBudget::Unmarked | CiRepairBudget::Available => {}
    }

    let checkout = &ci.pull.checkout_id;
    ensure!(
        registered_checkout_head(db, project, checkout)? == commit.sha,
        "The managed worktree moved after the failed GitHub revision"
    );
    IntegratedWorktreeBase {
        branch: pull.head_branch.clone(),
        prerequisite_commits: vec![pull.base_sha.clone()],
        verified_tip: Some(pull.head_sha.clone()),
    }
    .registered_tip(db, project, checkout)
    .context("The failed branch does not contain the exact GitHub destination revision")?;

    let task_host_id: String = db
        .query_row(
            "SELECT artifact_id FROM workflow_host_completions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3",
            params![child_run, start, current],
            |row| row.get(0),
        )
        .context("The selected backlog plan is missing its task binding")?;
    let task = load_artifact(db, project, child_run, &task_host_id, ArtifactKind::Task)?
        .context("The backlog task binding is unavailable")?;
    ensure!(
        task.value == ArtifactValue::Task(plan.task.task.clone()),
        "The retry would change the approved task"
    );
    let launch = load_workflow_launch(db, project, child_run)?
        .context("The backlog launch is unavailable")?;
    ensure!(
        launch.checkout_id == plan.checkout_id && launch.automatic,
        "The backlog launch scope changed"
    );
    let mut inputs = launch.inputs;
    let backlog = inputs
        .get_mut("backlog")
        .and_then(Value::as_object_mut)
        .context("The backlog launch lost its structured inputs")?;
    ensure!(
        backlog
            .get("specificationEvidence")
            .is_some_and(Value::is_object),
        "The backlog launch lost its approved specification evidence"
    );
    let repair = BacklogCiRepair {
        attempt: plan
            .repair
            .as_ref()
            .map_or(1, |previous| previous.attempt + 1),
        previous_plan_revision: current.clone(),
        failed_ci_artifact_id: failed_ci_artifact_id.into(),
        previous_source_sha: commit.sha,
        destination_sha: pull.base_sha.clone(),
    };
    backlog.insert("repair".into(), json!({"pins": repair, "ci": ci.snapshot}));
    plan.repair = Some(repair);
    let provenance = BTreeMap::from([
        ("previousPlan".into(), current.clone()),
        ("failedCi".into(), failed_ci_artifact_id.into()),
    ]);

    db.execute_batch("SAVEPOINT backlog_ci_retry")?;
    let transition = (|| -> Result<()> {
        let next = insert_artifact(
            db,
            project,
            child_run.into(),
            start.clone(),
            "plan".into(),
            ArtifactValue::BacklogPlan(plan),
        )?;
        validate_selected_plan_revision(db, project, child_run, &next.id)?;
        let revised = insert_artifact(
            db,
            project,
            child_run.into(),
            start.clone(),
            "inputs".into(),
            ArtifactValue::JsonObject(inputs),
        )?;
        record_workflow_input_ids(db, &next, &next.id, &provenance)?;
        record_workflow_input_ids(db, &revised, &next.id, &provenance)?;
        record_host_completion(db, &task, &next.id, &next.id)?;
        let changed = db.execute(
            "UPDATE job_pipeline_runs SET plan_revision = ?4
             WHERE id = ?1 AND project_id = ?2 AND plan_revision = ?3",
            params![child_run, project, current, next.id],
        )?;
        ensure!(
            changed == 1,
            "The backlog child plan changed before the GitHub repair could be selected"
        );
        db.execute("UPDATE job_pipeline_releases SET revoked = 1 WHERE pipeline_run_id = ?1 AND plan_revision = ?2",
            params![child_run, current])?;
        Ok(())
    })();
    if transition.is_err() {
        db.execute_batch("ROLLBACK TO backlog_ci_retry")?;
    }
    db.execute_batch("RELEASE backlog_ci_retry")?;
    transition.map(|()| BacklogCiRetry::Replanned)
}

fn failed_check_for_revision(ci: &CiEvidence) -> bool {
    let pull = &ci.pull.pull;
    let snapshot = &ci.snapshot;
    snapshot.state == CiState::Failed
        && snapshot.pull_number == pull.number
        && snapshot.head_sha == pull.head_sha
        && snapshot.base_sha == pull.base_sha
        // Wait for CI verified which commit was evaluated. GitHub may populate
        // its synthetic test merge after the initial PR artifact was written.
        && github_oid(&snapshot.tested_sha)
        && snapshot.tested_tree.as_deref().is_some_and(github_oid)
        && snapshot.required_checks.iter().any(|required| {
            snapshot.checks.iter().any(|check| {
                check.state == CiState::Failed
                    && check.id != 0
                    && check.matches_context(&required.context)
                    && (check.source == "commit_status"
                        || (check.source == "check_run"
                            && required.app_id.is_none_or(|app| check.app_id == Some(app))))
            })
        })
}

fn github_oid(sha: &str) -> bool {
    sha.len() == 40
        && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        && sha.bytes().any(|byte| byte != b'0')
}

fn require_published_head(
    db: &Connection,
    project: &str,
    run: &str,
    plan: &str,
    ci: &CiEvidence,
    child: &ChildScope,
) -> Result<()> {
    let pull = &ci.pull.pull;
    let repository = pull.repository_id.to_string();
    let qualified = db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM workflow_github_branches b JOIN workflow_github_project_settings s ON s.project_id = b.project_id
           JOIN workflow_github_delivery_pins p ON p.pipeline_run_id = ?2 AND p.plan_key = ?3 AND p.policy_revision = s.revision
           JOIN managed_task_worktrees w ON w.project_id = b.project_id AND w.task_id = b.task_id
           WHERE b.project_id = ?1 AND s.revision = ?4 AND b.repository_id = ?5 AND b.branch = ?6
             AND b.task_id = ?7 AND b.target_head = ?8 AND b.published = 1
             AND w.checkout_id = ?9 AND w.branch = b.branch AND p.destination_branch = ?10
             AND NOT EXISTS (SELECT 1 FROM job_writer_reservations WHERE checkout_id = ?9)
             AND NOT EXISTS (SELECT 1 FROM app_worktree_writers WHERE checkout_id = ?9)
             AND NOT EXISTS (SELECT 1 FROM pending_worktree_changes WHERE checkout_id = ?9)
         )",
        params![project, run, plan, ci.pull.policy_revision, repository, pull.head_branch,
            child.task, pull.head_sha, ci.pull.checkout_id, child.destination],
        |row| row.get::<_, bool>(0),
    )?;
    ensure!(
        qualified,
        "The GitHub policy, branch publication or worktree ownership changed before retry"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::github::{
        CheckEvidence, CiSnapshot, PullEvidence, PullRequest, RequiredCheck,
    };

    #[test]
    fn retry_requires_measured_failure_on_the_exact_revision_and_check() {
        let head = "a".repeat(40);
        let base = "b".repeat(40);
        let mut evidence = CiEvidence {
            pull_artifact_id: "pull".into(),
            pull: PullEvidence {
                policy_revision: "policy".into(),
                task_id: "task".into(),
                checkout_id: "checkout".into(),
                commit_evidence_id: "commit".into(),
                git_artifact_id: "git".into(),
                requires_human: false,
                changed_paths: vec!["first.txt".into()],
                source_syncs: vec![],
                pull: PullRequest {
                    number: 1,
                    id: "PR".into(),
                    repository_id: 11,
                    head_branch: "task/one".into(),
                    head_sha: head.clone(),
                    base_branch: "main".into(),
                    base_sha: base.clone(),
                    merge_commit_sha: None,
                    merged: false,
                    open: true,
                    draft: false,
                    url: "https://github.com/owner/repo/pull/1".into(),
                },
            },
            snapshot: CiSnapshot {
                pull_number: 1,
                head_sha: head,
                base_sha: base,
                tested_sha: "c".repeat(40),
                tested_tree: Some("d".repeat(40)),
                state: CiState::Failed,
                checks: vec![CheckEvidence {
                    context: "test".into(),
                    app_id: Some(42),
                    state: CiState::Failed,
                    source: "check_run".into(),
                    id: 5,
                }],
                required_checks: vec![RequiredCheck {
                    context: "test".into(),
                    app_id: Some(42),
                }],
                reason: Some("Required check failed".into()),
            },
        };
        assert!(failed_check_for_revision(&evidence));
        evidence.snapshot.checks[0].app_id = Some(43);
        assert!(!failed_check_for_revision(&evidence));
        evidence.snapshot.checks[0].app_id = Some(42);
        evidence.snapshot.tested_tree = None;
        assert!(!failed_check_for_revision(&evidence));
    }
}
