use std::{path::Path, process::Command, sync::Arc, time::Duration};

use serde_json::json;

use crate::{
    jobs::JobScheduler,
    repository::ProjectCheckout,
    runner::DirectWindowsRunner,
    store::Store,
    tasks::{
        AgileEventData, AgileEventOrigin, DeliveryCompletionMilestone, DeliveryMilestonePolicy,
        TaskPlacement,
    },
    workflows::{
        ApprovalDecisionSource, ApprovalRuleSettings, ArtifactValue, UnmetApproval,
        WorkflowApprovalStage, WorkflowApprovalSubmission, WorkflowGraph, WorkflowHostRequest,
        WorkflowHostSubmission, WorkflowRevision, WorkflowRun, tests::approved_delivery_repository,
    },
};

use super::*;

fn task(key: &str, priority: i32, depends_on: &[&str]) -> Value {
    json!({
        "key": key,
        "title": format!("Implement {key}"),
        "description": format!("Follow the approved specification for {key}."),
        "priority": priority,
        "dependsOn": depends_on,
        "acceptanceCriteria": [format!("{key} is verifiable")],
        "sddRequirements": ["REQ-1"],
        "bddScenarios": ["BDD-1"],
    })
}

async fn launch(
    store: &Store,
    repo: &ProjectCheckout,
    id: &str,
    tasks: Vec<Value>,
    child: Option<WorkflowRevision>,
) -> Result<WorkflowRun> {
    let mut graph: WorkflowGraph = serde_json::from_value(json!({
        "name": "Approved Agile backlog",
        "nodes": [
            {"id": "start", "kind": "typed_start", "configuration": {"kind": "typed_start", "fields": []}},
            {"id": "plan", "kind": "set_fields", "configuration": {
                "kind": "set_fields", "mode": "build", "operations": [
                    {"op": "set", "path": "/epicTitle", "value": {"source": "literal", "value": "Pipeline delivery"}},
                    {"op": "set", "path": "/tasks", "value": {"source": "literal", "value": tasks}},
                ],
            }},
            {"id": "sdd", "kind": "set_fields", "configuration": {
                "kind": "set_fields", "mode": "build", "operations": [
                    {"op": "set", "path": "/requirements", "value": {"source": "literal", "value": [
                        {"id": "REQ-1", "description": "The approved requirement."},
                        {"id": "REQ-2", "description": "An unrelated requirement."}
                    ]}},
                ],
            }},
            {"id": "bdd", "kind": "set_fields", "configuration": {
                "kind": "set_fields", "mode": "build", "operations": [
                    {"op": "set", "path": "/scenarios", "value": {"source": "literal", "value": [
                        {"id": "BDD-1", "then": "The expected outcome is observable."}
                    ]}},
                ],
            }},
            {"id": "materialize", "kind": "materialize_backlog", "configuration": {"kind": "none"}},
        ],
        "wires": [
            {"from": {"node": "start", "port": "start"}, "to": {"node": "plan", "port": "ready"}},
            {"from": {"node": "start", "port": "start"}, "to": {"node": "sdd", "port": "ready"}},
            {"from": {"node": "start", "port": "start"}, "to": {"node": "bdd", "port": "ready"}},
            {"from": {"node": "plan", "port": "completed"}, "to": {"node": "materialize", "port": "ready"}},
            {"from": {"node": "plan", "port": "object"}, "to": {"node": "materialize", "port": "plan"}},
            {"from": {"node": "sdd", "port": "object"}, "to": {"node": "materialize", "port": "sdd"}},
            {"from": {"node": "bdd", "port": "object"}, "to": {"node": "materialize", "port": "bdd"}},
        ],
    }))?;
    let automatic = child.is_some();
    if let Some(child) = child {
        for (port, field, value) in [
            (
                "design",
                "technicalDesign",
                json!("Preserve the API boundary."),
            ),
            ("strategy", "requiredChecks", json!(["Pipeline mutation"])),
        ] {
            graph.nodes.push(serde_json::from_value(json!({
                "id": port, "kind": "set_fields", "configuration": {
                    "kind": "set_fields", "mode": "build", "operations": [
                        {"op": "set", "path": format!("/{field}"), "value": {"source": "literal", "value": value}}
                    ]
                }
            }))?);
            graph.wires.extend(serde_json::from_value::<Vec<_>>(json!([
                {"from": {"node": "start", "port": "start"}, "to": {"node": port, "port": "ready"}},
                {"from": {"node": port, "port": "object"}, "to": {"node": "materialize", "port": port}}
            ]))?);
        }
        graph.nodes.push(serde_json::from_value(json!({
            "id": "execute", "kind": "run_backlog", "configuration": {
                "kind": "run_backlog", "child": child, "maxParallel": 4,
                "failed": {"node": "outcome", "port": "matched"}
            }
        }))?);
        graph.wires.extend(serde_json::from_value::<Vec<_>>(json!([
            {"from": {"node": "materialize", "port": "completed"}, "to": {"node": "execute", "port": "ready"}},
            {"from": {"node": "materialize", "port": "backlog"}, "to": {"node": "execute", "port": "backlog"}}
        ]))?);
    }
    let version = store
        .save_workflow_revision(repo.project_id.clone(), id.into(), graph)
        .await?;
    store
        .launch_workflow_request(serde_json::from_value(json!({
            "projectId": repo.project_id, "workflow": version, "checkoutId": repo.checkout_id,
            "requestId": id, "inputs": {}, "automatic": automatic,
        }))?)
        .await
}

#[tokio::test]
async fn materialization_rejects_cycles_and_preserves_the_approved_dag_across_restart() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("backlog.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let invalid = launch(
        &store,
        &repo,
        "invalid-backlog",
        vec![task("A", 10, &["B"]), task("B", 20, &["A"])],
        None,
    )
    .await?;
    assert_eq!(store.settle_ready_agile_stages().await?, 1);
    let inspection = store
        .inspect_workflow_execution(project.clone(), invalid.id)
        .await?
        .unwrap();
    let failed = inspection
        .control
        .iter()
        .find(|stage| stage.node_id == "materialize")
        .unwrap();
    assert_eq!(failed.selected_ports, ["failed"]);
    assert!(
        failed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("cycle"))
    );
    let hierarchy = store.project_work_hierarchy(project.clone()).await?;
    assert!(hierarchy.epics.is_empty() && hierarchy.work.is_empty());

    let run = launch(
        &store,
        &repo,
        "approved-backlog",
        vec![
            task("C", 30, &["A", "B"]),
            task("B", 10, &[]),
            task("A", 10, &[]),
        ],
        None,
    )
    .await?;
    assert!(
        store
            .project_work_hierarchy(project.clone())
            .await?
            .work
            .is_empty()
    );
    drop(store);
    let store = Store::open(database.clone())?;
    assert_eq!(store.settle_ready_agile_stages().await?, 1);
    let inspection = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    let output = inspection
        .outputs
        .into_iter()
        .find(|output| output.artifact.port == "backlog")
        .unwrap();
    let ArtifactValue::Backlog(receipt) = &output.artifact.value else {
        anyhow::bail!("The materialized backlog receipt was not published");
    };
    assert_eq!(receipt.sources, output.input_receipts[0].inputs);
    let keys: Vec<_> = receipt
        .tasks
        .iter()
        .map(|item| item.specification.key.as_str())
        .collect();
    assert_eq!(keys, ["A", "B", "C"]);
    let hierarchy = store.project_work_hierarchy(project.clone()).await?;
    assert_eq!(hierarchy.epics.len(), 1);
    assert_eq!(hierarchy.epics[0].id, receipt.epic_id);
    assert!(hierarchy.work.iter().all(|work| work.placement
        == TaskPlacement::Task {
            epic_id: Some(receipt.epic_id.clone())
        }));
    let final_task = &receipt.tasks[2];
    let details = store
        .project_task_details(project.clone(), final_task.task.task_id.clone())
        .await?
        .context("Agile task details were not saved")?;
    let mut expected_prerequisites: Vec<_> = receipt.tasks[..2]
        .iter()
        .map(|item| item.task.task_id.clone())
        .collect();
    expected_prerequisites.sort();
    assert_eq!(details.prerequisites, expected_prerequisites);
    assert_eq!(details.priority, 30);
    assert_eq!(details.acceptance_criteria, ["C is verifiable"]);
    assert_eq!(final_task.specification.sdd_requirements, ["REQ-1"]);
    assert_eq!(final_task.specification.bdd_scenarios, ["BDD-1"]);
    drop(store);

    let reopened = Store::open(database)?;
    assert_eq!(reopened.settle_ready_agile_stages().await?, 0);
    let work = reopened.project_work_hierarchy(project.clone()).await?;
    assert_eq!(work.epics.len(), 1);
    assert_eq!(work.work.len(), 3);
    let events = reopened
        .project_work_events(project.clone(), None, 100)
        .await?;
    assert!(events.iter().any(|event| event.task_id == final_task.task.task_id
        && matches!(event.data, AgileEventData::Created { .. })
        && matches!(&event.origin, AgileEventOrigin::Pipeline { context } if context.run_id == run.id)));
    Ok(())
}

#[tokio::test]
async fn run_backlog_reserves_by_priority_and_holds_dependencies_without_a_verified_merge()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("run-backlog.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let root = Path::new(&repo.checkout_root);
    let git = |arguments: &[&str]| -> Result<String> {
        let result = Command::new("git.exe")
            .args(arguments)
            .current_dir(root)
            .output()?;
        ensure!(
            result.status.success(),
            "Git fixture failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(String::from_utf8(result.stdout)?.trim().into())
    };
    let launch_branch = git(&["branch", "--show-current"])?;
    let launch_head = git(&["rev-parse", "HEAD"])?;
    git(&["branch", "missing-prerequisite"])?;
    git(&["switch", "-c", "integration"])?;
    std::fs::write(root.join("integrated.txt"), "Already integrated\n")?;
    git(&["add", "integrated.txt"])?;
    git(&[
        "-c",
        "user.name=Workflow Test",
        "-c",
        "user.email=workflow@example.test",
        "commit",
        "-qm",
        "Advance destination",
    ])?;
    let integrated_head = git(&["rev-parse", "HEAD"])?;
    git(&["switch", &launch_branch])?;
    let child = store.save_workflow_revision(project.clone(), "task-child".into(), serde_json::from_value(json!({
        "name": "Pinned task workflow",
        "nodes": [
            {"id": "start", "kind": "typed_start", "configuration": {"kind": "typed_start", "fields": [
                {"name": "backlog", "valueType": "object", "required": true}
            ]}},
            {"id": "outcome", "kind": "condition", "configuration": {"kind": "condition", "predicate": {
                "op": "exists", "pointer": "/evidence/backlog/terminalFailure"
            }}},
            {"id": "approve", "kind": "approve_implementation", "configuration": {"kind": "none"}},
            {"id": "prepare", "kind": "prepare_worktree", "configuration": {"kind": "none"}}
        ],
        "wires": [
            {"from": {"node": "start", "port": "start"}, "to": {"node": "outcome", "port": "ready"}},
            {"from": {"node": "start", "port": "inputs"}, "to": {"node": "outcome", "port": "value"}},
            {"from": {"node": "start", "port": "start"}, "to": {"node": "approve", "port": "ready"}},
            {"from": {"node": "start", "port": "plan"}, "to": {"node": "approve", "port": "plan"}},
            {"from": {"node": "start", "port": "task"}, "to": {"node": "approve", "port": "task"}},
            {"from": {"node": "approve", "port": "approved"}, "to": {"node": "prepare", "port": "ready"}},
            {"from": {"node": "start", "port": "task"}, "to": {"node": "prepare", "port": "task"}},
            {"from": {"node": "approve", "port": "decision"}, "to": {"node": "prepare", "port": "decision"}}
        ]
    }))?).await?;
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("integration".into()),
            },
        )
        .await?;
    store.set_job_queue_capacity(1).await?;
    let run = launch(
        &store,
        &repo,
        "run-backlog",
        vec![
            task("A", 10, &[]),
            task("B", 20, &[]),
            task("C", 40, &["B"]),
            task("D", 3, &[]),
        ],
        Some(child.clone()),
    )
    .await?;
    assert_eq!(store.settle_ready_agile_stages().await?, 1);
    store.set_job_queue_paused(true).await?;
    store.settle_workflow_backlogs().await?;
    let children = |snapshot: crate::workflows::inspection::WorkflowExecutionInspection| -> Result<Vec<Value>> {
        let value = serde_json::to_value(snapshot.backlog_executions)?;
        Ok(value[0]["tasks"].as_array().context("Missing task executions")?.clone())
    };
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    assert!(children(snapshot)?.is_empty());
    drop(store);

    let store = Store::open(database.clone())?;
    store.set_job_queue_paused(false).await?;
    store.settle_workflow_backlogs().await?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    let first = children(snapshot)?;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["key"], "B");
    let first_child = first[0]["workflowRunId"].as_str().unwrap().to_owned();
    let first_task = first[0]["taskId"].as_str().unwrap().to_owned();
    let receipt = store
        .workflow_launch(project.clone(), first_child.clone())
        .await?
        .unwrap();
    assert_eq!(receipt.version, child);
    assert!(receipt.automatic);
    assert!(
        matches!(receipt.origin, crate::workflows::WorkflowLaunchOrigin::Backlog { parent_run_id, work_id, .. }
        if parent_run_id == run.id && work_id == first_task)
    );
    assert_eq!(
        receipt.inputs["backlog"]["specification"]["sddRequirements"],
        json!(["REQ-1"])
    );
    assert_eq!(
        receipt.inputs["backlog"]["specification"]["bddScenarios"],
        json!(["BDD-1"])
    );
    assert_eq!(
        receipt.inputs["backlog"]["specificationEvidence"]["sdd"]["requirements"],
        json!([{"id": "REQ-1", "description": "The approved requirement."}])
    );
    assert_eq!(
        receipt.inputs["backlog"]["specificationEvidence"]["bdd"]["scenarios"],
        json!([{"id": "BDD-1", "then": "The expected outcome is observable."}])
    );
    assert_eq!(
        receipt.inputs["backlog"]["specificationEvidence"]["design"]["technicalDesign"],
        "Preserve the API boundary."
    );
    assert_eq!(
        receipt.inputs["backlog"]["specificationEvidence"]["strategy"]["requiredChecks"],
        json!(["Pipeline mutation"])
    );
    let manual = store
        .record_manual_task_delivery(project.clone(), first_task.clone(), None)
        .await;
    assert!(
        manual
            .unwrap_err()
            .to_string()
            .contains("requires a verified merge")
    );
    let stage = store
        .workflow_approval_stage(project.clone(), first_child.clone(), "approve".into())
        .await?;
    let WorkflowApprovalStage::Ready {
        plan_id,
        task_artifact_id,
        preview,
        ..
    } = stage
    else {
        anyhow::bail!("The child task plan did not reach implementation approval: {stage:?}");
    };
    assert!(preview.allows_human);
    assert_eq!(preview.facts.task_id, first_task);
    assert_eq!(preview.facts.source_checkout_id, repo.checkout_id);
    assert_eq!(preview.facts.unit_count, 1);
    assert!(
        store
            .set_job_pipeline_plan(
                project.clone(),
                first_child.clone(),
                Some(plan_id.clone()),
                "fabricated-plan".into()
            )
            .await
            .is_err()
    );
    let units = store
        .workflow_plan_units(project.clone(), first_child.clone(), plan_id.clone())
        .await?;
    assert_eq!(units[0].purpose, "Implement B");
    assert_eq!(units[0].completion_criteria, ["B is verifiable"]);
    let decision = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: project.clone(),
            run_id: first_child.clone(),
            node_id: "approve".into(),
            inputs: NodeInputs::from([
                ("plan".into(), plan_id.clone()),
                ("task".into(), task_artifact_id.clone()),
            ]),
            request: WorkflowHostRequest::ApproveImplementation,
        })
        .await?;
    assert!(matches!(decision.value, ArtifactValue::Approval(_)));
    assert!(store.next_workflow_preparation().await?.is_some());
    store.set_job_queue_paused(true).await?;
    assert!(store.next_workflow_preparation().await?.is_none());
    drop(store);
    let store = Store::open(database.clone())?;
    store.set_job_queue_paused(false).await?;
    let scheduler = JobScheduler::start_with_workflow_storage(
        store.clone(),
        Arc::new(DirectWindowsRunner),
        None,
        temp.path().join("comparison"),
        temp.path().join("managed"),
    );
    let verified = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let output = store
                .workflow_run_artifacts(project.clone(), first_child.clone())
                .await?
                .into_iter()
                .find(|artifact| {
                    artifact.node_instance_id == "prepare" && artifact.port == "worktree"
                });
            if let Some(output) = output
                && store.next_workflow_preparation().await?.is_none()
            {
                return Ok::<_, anyhow::Error>(output);
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    })
    .await;
    scheduler.shutdown().await?;
    let prepared = verified.context("Automatic worktree preparation did not finish")??;
    assert!(matches!(prepared.value, ArtifactValue::Worktree { .. }));
    let worktree = store
        .live_managed_worktree(project.clone(), first_task)
        .await?
        .context("The child delivery lost its managed worktree")?
        .worktree;
    assert_ne!(worktree.base_commit, launch_head);
    assert_eq!(worktree.base_commit, integrated_head);
    assert!(worktree.root.join("integrated.txt").is_file());
    let scope = preview
        .facts
        .backlog_scope
        .clone()
        .context("Missing linked backlog approval scope")?;
    let submit = WorkflowApprovalSubmission {
        project_id: project.clone(),
        run_id: first_child.clone(),
        node_id: "approve".into(),
        inputs: NodeInputs::from([("plan".into(), plan_id), ("task".into(), task_artifact_id)]),
    };
    let scoped_rule = |scope| ApprovalRuleSettings::BacklogAutomatic {
        scope,
        source_checkout_id: repo.checkout_id.clone(),
        maximum_units: 1,
        if_unmet: UnmetApproval::Human,
    };
    let mut foreign = scope.clone();
    foreign.parent_run_id = "different-run".into();
    assert!(
        store
            .set_workflow_approval_rule(submit.clone(), None, scoped_rule(foreign))
            .await
            .is_err()
    );
    let saved_rule = store
        .set_workflow_approval_rule(submit, None, scoped_rule(scope))
        .await?;
    assert!(saved_rule.revision.is_some());
    store
        .set_delivery_milestone_policy(
            project.clone(),
            DeliveryMilestonePolicy {
                milestone: DeliveryCompletionMilestone::VerifiedCommit,
                destination_branch: Some("other".into()),
            },
        )
        .await?;
    store.set_job_queue_capacity(4).await?;
    store
        .execute(|db| {
            db.execute("UPDATE workflow_backlog_executions SET next_check = 0", [])?;
            Ok(())
        })
        .await?;
    store.settle_workflow_backlogs().await?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .unwrap();
    let next = children(snapshot)?;
    assert_eq!(
        next.iter()
            .map(|child| child["key"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["A", "B", "D"]
    );
    assert_eq!(next[1]["workflowRunId"], first_child);

    let project_id = project.clone();
    let second_child = next[0]["workflowRunId"].as_str().unwrap().to_owned();
    let second_task = next[0]["taskId"].as_str().unwrap().to_owned();
    let stage = store
        .workflow_approval_stage(project.clone(), second_child.clone(), "approve".into())
        .await?;
    assert!(matches!(
        stage,
        WorkflowApprovalStage::Ready {
            source: Some(ApprovalDecisionSource::AutomaticRule),
            ..
        }
    ));
    let refused = crate::repository::provision_task_worktree_at(
        &store,
        temp.path().join("managed"),
        project.clone(),
        second_task.clone(),
        repo.checkout_id.clone(),
        Some(crate::repository::IntegratedWorktreeBase {
            branch: "missing-prerequisite".into(),
            prerequisite_commits: vec![integrated_head],
            verified_tip: None,
        }),
    )
    .await
    .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("does not contain every verified prerequisite")
    );
    assert!(
        store
            .live_managed_worktree(project.clone(), second_task)
            .await?
            .is_none()
    );
    let parent_run = run.id.clone();
    store
        .execute(move |db| {
            for run in [&first_child, &second_child] {
                let policy = crate::tasks::load_workflow_milestone_policy(db, &project_id, run)?;
                assert_eq!(policy.milestone, DeliveryCompletionMilestone::VerifiedMerge);
                assert_eq!(policy.destination_branch.as_deref(), Some("integration"));
            }
            let origin = crate::tasks::pipeline_work_origin(db, &project_id, &second_child)?;
            let AgileEventOrigin::Pipeline { context } = origin else {
                unreachable!()
            };
            assert_eq!(context.automation_chain[0].run_id, parent_run);
            assert_eq!(context.automation_chain[1].run_id, second_child);
            Ok(())
        })
        .await?;
    drop(store);
    let reopened = Store::open(database)?;
    reopened.settle_workflow_backlogs().await?;
    let snapshot = reopened
        .inspect_workflow_execution(project.clone(), run.id)
        .await?
        .unwrap();
    assert_eq!(children(snapshot)?, next);
    Ok(())
}
