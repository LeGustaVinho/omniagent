use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use super::{ChecklistItem, ChecklistOrderAnchor};
use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, Delivery},
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, Job, JobPermission, JobRequest, JobRole, JobRule,
        JobWorkspace, PipelineAgentInput, PipelineDispatch, PipelineNodeKind, RecoveryAction,
        ReleaseBlocker,
    },
    providers::{Provider, SessionMode},
    repository::{self, BranchStrategy, ProjectCheckout, provision_task_worktree},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    tasks::{
        ChecklistExecutionMode as Mode, ChecklistProgressActor, ChecklistProgressAvailability,
        ChecklistProgressRequest, DescriptionOwner, TaskDetails, TaskRunScope,
    },
};

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git.exe")
        .current_dir(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn swaps_only_current_adjacent_checklist_positions_in_the_same_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("store.sqlite3"))?;
    let project = register(&store, &temp.path().join("repo"))
        .await?
        .project_id;
    let task = store
        .create_project_task(project.clone(), "Order".into())
        .await?
        .id;
    let other = store
        .create_project_task(project.clone(), "Other work".into())
        .await?
        .id;
    store
        .ensure_project_checklist(project.clone(), task.clone())
        .await?;
    let first = store
        .append_project_checklist_item(project.clone(), task.clone(), "First".into())
        .await?;
    let second = store
        .append_project_checklist_item(project.clone(), task.clone(), "Second".into())
        .await?;
    let third = store
        .append_project_checklist_item(project.clone(), task.clone(), "Third".into())
        .await?;
    let anchor = |item: &ChecklistItem| ChecklistOrderAnchor {
        id: item.id.clone(),
        position: item.position,
        revision: item.revision,
    };
    for (scope, before, after) in [(&task, &first, &third), (&other, &first, &second)] {
        assert!(
            store
                .swap_project_checklist_items(
                    project.clone(),
                    scope.clone(),
                    anchor(before),
                    anchor(after)
                )
                .await
                .is_err()
        );
    }
    store
        .swap_project_checklist_items(
            project.clone(),
            task.clone(),
            anchor(&first),
            anchor(&second),
        )
        .await?;
    let current = store
        .project_checklist(project.clone(), task.clone())
        .await?
        .context("checklist unavailable")?
        .items;
    assert_eq!(
        current,
        vec![
            ChecklistItem {
                position: first.position,
                ..second.clone()
            },
            ChecklistItem {
                position: second.position,
                ..first.clone()
            },
            third,
        ]
    );
    assert!(
        store
            .swap_project_checklist_items(
                project.clone(),
                task.clone(),
                anchor(&first),
                anchor(&second)
            )
            .await
            .is_err()
    );

    let revised = store
        .revise_project_checklist_item(
            project.clone(),
            task.clone(),
            first.id,
            first.revision,
            "First reviewed".into(),
            true,
        )
        .await?;
    assert!(
        store
            .swap_project_checklist_items(
                project.clone(),
                task.clone(),
                anchor(&current[0]),
                anchor(&current[1])
            )
            .await
            .is_err()
    );
    let saved = store
        .project_checklist(project, task)
        .await?
        .context("checklist unavailable")?
        .items;
    assert_eq!(saved, vec![current[0].clone(), revised, current[2].clone()]);
    Ok(())
}

async fn register(store: &Store, root: &Path) -> Result<ProjectCheckout> {
    std::fs::create_dir(root)?;
    git(root, &["init", "--quiet"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    )?;
    repository::register(store, root.to_path_buf()).await
}

async fn pipeline(store: &Store, project: &str) -> Result<PipelineDispatch> {
    store
        .register_job_pipeline(project.into(), "pipeline".into())
        .await?;
    for (node, kind) in [
        ("execute", PipelineNodeKind::Execution),
        ("approve", PipelineNodeKind::ImplementationApproval),
    ] {
        store
            .register_job_pipeline_node(project.into(), "pipeline".into(), node.into(), kind)
            .await?;
    }
    store
        .set_job_pipeline_plan(project.into(), "pipeline".into(), None, "plan".into())
        .await?;
    Ok(PipelineDispatch {
        pipeline_run_id: "pipeline".into(),
        node_instance_id: "execute".into(),
        plan_revision: Some("plan".into()),
        release_id: None,
    })
}

fn scope(root: &Path, mode: SessionMode) -> Result<SessionScope> {
    Ok(SessionScope {
        provider: Provider::Codex,
        profile_id: "fixture".into(),
        account_binding: None,
        checkout: dunce::canonicalize(root)?,
        executable: std::env::current_exe()?,
        cli_version: "fixture".into(),
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "fixture-policy".into(),
        mode,
        model: None,
        effort: None,
    })
}

fn request(checkout: &str, task: &str, mode: SessionMode) -> JobRequest {
    JobRequest {
        checkout_id: Some(checkout.into()),
        task_id: Some(task.into()),
        mode,
        instructions: "Review the selected work".into(),
        dependencies: vec![],
        requires_bridge: false,
    }
}

fn input(item: Option<String>) -> PipelineAgentInput {
    PipelineAgentInput {
        target: Some(TaskRunScope {
            role: "reviewer".into(),
            checklist_item_id: item,
        }),
        ..Default::default()
    }
}

fn environment() -> AdmissionEnvironment {
    AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: false,
    }
}

#[tokio::test]
async fn checklist_modes_select_direct_work_and_preserve_item_attempts_across_retries() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite3");
    let root = temp.path().join("repo");
    let store = Store::open(database.clone())?;
    let checkout = register(&store, &root).await?;
    let project = checkout.project_id;
    let parent = store
        .create_project_task(project.clone(), "Delivery".into())
        .await?;
    let child = store
        .create_project_subtask(project.clone(), parent.id.clone(), "API review".into())
        .await?;
    store
        .set_project_task_details(
            project.clone(),
            child.id.clone(),
            TaskDetails {
                acceptance_criteria: vec!["Check the HTTP contract".into()],
                ..Default::default()
            },
        )
        .await?;
    store
        .save_project_description(
            project.clone(),
            DescriptionOwner::Work(parent.id.clone()),
            None,
            Some("Retain the current API behavior.".into()),
        )
        .await?;

    let checklist_id = store
        .ensure_project_checklist(project.clone(), child.id.clone())
        .await?;
    let first = store
        .append_project_checklist_item(
            project.clone(),
            child.id.clone(),
            "Inspect the routes".into(),
        )
        .await?;
    let second = store
        .append_project_checklist_item(project.clone(), child.id.clone(), "Check the schema".into())
        .await?;
    let revised = store
        .revise_project_checklist_item(
            project.clone(),
            child.id.clone(),
            first.id.clone(),
            1,
            "Inspect the public routes".into(),
            true,
        )
        .await?;
    assert_eq!(revised.id, first.id);
    assert_eq!(revised.revision, 2);
    assert_eq!(revised.position, first.position);
    assert!(
        store
            .revise_project_checklist_item(
                project.clone(),
                child.id.clone(),
                first.id.clone(),
                1,
                "Stale edit".into(),
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .project_checklist_item_revision(project.clone(), child.id.clone(), first.id.clone(), 1)
            .await?,
        Some(first)
    );

    assert_eq!(
        store.project_checklist_mode(project.clone()).await?,
        Mode::TrackProgress
    );
    store
        .set_project_checklist_mode(project.clone(), Mode::RunByItem)
        .await?;
    assert_eq!(
        store
            .task_checklist_mode(project.clone(), child.id.clone())
            .await?,
        Mode::RunByItem
    );
    store
        .set_task_checklist_mode(
            project.clone(),
            parent.id.clone(),
            Some(Mode::TrackProgress),
        )
        .await?;
    store
        .set_task_checklist_mode(project.clone(), child.id.clone(), Some(Mode::RunByItem))
        .await?;
    assert_eq!(
        store
            .task_checklist_mode(project.clone(), child.id.clone())
            .await?,
        Mode::RunByItem
    );
    assert!(
        store
            .set_task_checklist_mode(project.clone(), "other-task".into(), Some(Mode::RunByItem))
            .await
            .is_err()
    );
    store
        .set_task_checklist_mode(project.clone(), child.id.clone(), None)
        .await?;
    let loaded = store
        .project_checklist(project.clone(), child.id.clone())
        .await?
        .unwrap();
    assert_eq!(loaded.id, checklist_id);
    assert_eq!(loaded.mode, Mode::TrackProgress);
    assert_eq!(loaded.items, [revised.clone(), second.clone()]);

    let binding = pipeline(&store, &project).await?;
    let profile = scope(&root, SessionMode::ReadOnly)?;
    let request = request(&checkout.checkout_id, &child.id, profile.mode);
    assert!(
        store
            .submit_pipeline_checklist_job(
                binding.clone(),
                profile.clone(),
                request.clone(),
                input(Some(revised.id.clone()))
            )
            .await
            .is_err()
    );
    let whole = store
        .submit_pipeline_checklist_job(
            binding.clone(),
            profile.clone(),
            request.clone(),
            input(None),
        )
        .await?;
    store
        .set_task_checklist_mode(project.clone(), parent.id.clone(), None)
        .await?;
    assert_eq!(
        store
            .task_checklist_mode(project.clone(), child.id.clone())
            .await?,
        Mode::RunByItem
    );
    assert!(
        store
            .submit_pipeline_checklist_job(
                binding.clone(),
                profile.clone(),
                request.clone(),
                input(None)
            )
            .await
            .is_err()
    );
    let attempt = store
        .claim_next_job(environment())
        .await?
        .context("missing Track progress attempt")?;
    let context = store
        .task_dispatch_context(attempt.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(context.checklist_mode, Some(Mode::TrackProgress));
    assert_eq!(context.checklist_item_id, None);
    assert_eq!(context.checklist, [revised.clone(), second.clone()]);
    let work = context.direct_work.as_ref().unwrap();
    assert_eq!(work.title, "API review");
    assert_eq!(work.parent_title.as_deref(), Some("Delivery"));
    assert_eq!(work.acceptance_criteria, ["Check the HTTP contract"]);
    let mut prompt = String::new();
    context.append_to_prompt(&mut prompt)?;
    assert!(prompt.contains("selected task's direct work"));
    assert!(prompt.contains("Check the schema"));
    assert!(prompt.contains("Inspect the public routes"));
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(attempt.run.id, RunStatus::Succeeded, None)
        .await?;

    store
        .ensure_project_checklist(project.clone(), parent.id.clone())
        .await?;
    let parent_item = store
        .append_project_checklist_item(project.clone(), parent.id, "Parent only".into())
        .await?;
    assert!(
        store
            .submit_pipeline_checklist_job(
                binding.clone(),
                profile.clone(),
                request.clone(),
                input(Some(parent_item.id))
            )
            .await
            .is_err()
    );
    let single = store
        .submit_pipeline_checklist_job(binding, profile, request, input(Some(revised.id.clone())))
        .await?;
    store
        .set_project_checklist_mode(project.clone(), Mode::TrackProgress)
        .await?;
    let attempt = store
        .claim_next_job(environment())
        .await?
        .context("missing Run by item attempt")?;
    let context = store
        .task_dispatch_context(attempt.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(context.checklist_mode, Some(Mode::RunByItem));
    assert_eq!(context.checklist, [revised.clone()]);
    let mut prompt = String::new();
    context.append_to_prompt(&mut prompt)?;
    assert!(prompt.contains("selected checklist item"));
    assert!(prompt.contains("Retain the current API behavior."));
    assert!(!prompt.contains(&second.body));
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            project.clone(),
            single.id.clone(),
            attempt.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;

    drop(store);
    let store = Store::open(database)?;
    let retry = store
        .claim_next_job(environment())
        .await?
        .context("missing checklist retry")?;
    let history = store
        .project_checklist_item_runs(project.clone(), child.id.clone(), revised.id)
        .await?;
    assert_eq!(
        history
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<Vec<_>>(),
        [attempt.run.id.as_str(), retry.run.id.as_str()]
    );
    assert!(
        history
            .iter()
            .all(|run| run.job_id == single.id && run.checklist_mode == Some(Mode::RunByItem))
    );
    assert!(
        store
            .project_task_runs(project, child.id)
            .await?
            .iter()
            .any(|run| run.job_id == whole.id
                && run.checklist_mode == Some(Mode::TrackProgress)
                && run.checklist_item_id.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn checklist_write_uses_its_scoped_branch_and_waits_for_the_current_worktree_release()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    let store = Store::open(temp.path().join("store.sqlite3"))?;
    let source = register(&store, &root).await?;
    let project = source.project_id;
    let parent = store
        .create_project_task(project.clone(), "Implement API change".into())
        .await?;
    let task = store
        .create_project_subtask(project.clone(), parent.id.clone(), "Update routing".into())
        .await?;
    store
        .set_project_branch_strategy(project.clone(), BranchStrategy::SharedBranchPerTask)
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        source.checkout_id.clone(),
    )
    .await?;
    assert_eq!(tree.owner_task_id, parent.id);
    store
        .ensure_project_checklist(project.clone(), task.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(
            project.clone(),
            task.id.clone(),
            "Update the API route".into(),
        )
        .await?;
    store
        .set_project_checklist_mode(project.clone(), Mode::RunByItem)
        .await?;
    store
        .set_project_job_rules(
            project.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;

    let binding = pipeline(&store, &project).await?;
    let wrong_branch = store
        .submit_pipeline_checklist_job(
            binding.clone(),
            scope(&root, SessionMode::ReadOnly)?,
            request(&source.checkout_id, &task.id, SessionMode::ReadOnly),
            input(Some(item.id.clone())),
        )
        .await
        .unwrap_err();
    assert!(
        wrong_branch
            .to_string()
            .contains("work scope's managed worktree")
    );
    let profile = scope(&tree.root, SessionMode::ApprovedWrite)?;
    let request = request(&tree.checkout_id, &task.id, profile.mode);
    let submit = |binding: PipelineDispatch| {
        store.submit_pipeline_checklist_job(
            binding,
            profile.clone(),
            request.clone(),
            input(Some(item.id.clone())),
        )
    };
    let inspect = |job: &Job| store.inspect_job(project.clone(), job.id.clone(), environment());
    let missing = submit(binding.clone()).await?;
    assert!(inspect(&missing).await?.unwrap().blockers.contains(
        &AdmissionBlocker::PipelineRelease {
            reason: ReleaseBlocker::Missing
        }
    ));
    assert!(store.claim_next_job(environment()).await?.is_none());

    let release = store
        .record_job_pipeline_release(
            project.clone(),
            binding.pipeline_run_id.clone(),
            "approve".into(),
            "plan".into(),
            tree.checkout_id.clone(),
            task.id.clone(),
        )
        .await?;
    let valid_binding = PipelineDispatch {
        release_id: Some(release.id),
        ..binding
    };
    let valid = submit(valid_binding.clone()).await?;
    assert_eq!(valid.task_id.as_deref(), Some(task.id.as_str()));
    assert_eq!(
        valid.worktree_owner_task_id.as_deref(),
        Some(parent.id.as_str())
    );
    let admitted = store
        .claim_next_job(environment())
        .await?
        .context("matching checklist release did not admit the write")?;
    assert_eq!(admitted.run.checkout_id, tree.checkout_id);
    assert_eq!(
        inspect(&valid)
            .await?
            .unwrap()
            .job
            .latest_attempt
            .unwrap()
            .run_id,
        admitted.run.id
    );
    assert_eq!(
        store
            .task_dispatch_context(admitted.run.id)
            .await?
            .unwrap()
            .checklist,
        [item.clone()]
    );

    store
        .set_job_pipeline_plan(
            project.clone(),
            valid_binding.pipeline_run_id.clone(),
            Some("plan".into()),
            "plan-2".into(),
        )
        .await?;
    let stale = submit(valid_binding).await?;
    assert!(inspect(&stale).await?.unwrap().blockers.contains(
        &AdmissionBlocker::PipelineRelease {
            reason: ReleaseBlocker::StalePlan
        }
    ));
    Ok(())
}

#[tokio::test]
async fn checklist_progress_authenticates_and_preserves_scoped_receipts_in_both_modes() -> Result<()>
{
    use crate::{
        conversations::AttemptPlan,
        dispatch::EventSource,
        mcp_bridge::{RunBridge, exchange_test_checklist_progress},
        providers::{
            Capability, ProviderEvent, ProviderEventData,
            capabilities::{
                Availability, CapabilityAssessment, CapabilityDescriptor, CapabilityScope,
                CapabilitySubject, DeclaredSource, EvidenceBoundary, ObservedResult,
                ResolvedCapability,
            },
        },
        tasks::TaskDispatchContext,
    };

    fn snapshot(
        attempt: &AttemptPlan,
        context: TaskDispatchContext,
        assessment: &CapabilityAssessment,
    ) -> Result<ContextSnapshot> {
        Ok(serde_json::from_value(json!({
            "version": 1, "contextId": attempt.context_id, "scope": attempt.scope,
            "baseCommit": null, "submittedMessages": attempt.input, "nativeHistory": {"mode": "fresh"},
            "taskContext": context, "capabilities": assessment,
        }))?)
    }

    for mode in [Mode::TrackProgress, Mode::RunByItem] {
        let temp = tempfile::tempdir()?;
        let database = temp.path().join("store.sqlite3");
        let root = temp.path().join("repo");
        let store = Store::open(database.clone())?;
        let checkout = register(&store, &root).await?;
        let project = checkout.project_id;
        let task = store
            .create_project_task(project.clone(), "Review API".into())
            .await?;
        store
            .ensure_project_checklist(project.clone(), task.id.clone())
            .await?;
        let item = store
            .append_project_checklist_item(project.clone(), task.id.clone(), "Inspect query".into())
            .await?;
        let sibling = store
            .append_project_checklist_item(
                project.clone(),
                task.id.clone(),
                "Inspect response".into(),
            )
            .await?;
        store
            .set_project_checklist_mode(project.clone(), mode)
            .await?;

        let profile = scope(&root, SessionMode::ReadOnly)?;
        let binding = pipeline(&store, &project).await?;
        let job = store
            .submit_pipeline_checklist_job(
                binding,
                profile.clone(),
                request(&checkout.checkout_id, &task.id, profile.mode),
                input((mode == Mode::RunByItem).then(|| item.id.clone())),
            )
            .await?;
        let attempt = store
            .claim_next_job(environment())
            .await?
            .context("missing checklist run")?;
        let run = attempt.run.id.clone();
        assert_eq!(
            store
                .run_checklist_progress(project.clone(), run.clone())
                .await?
                .unwrap()
                .availability,
            ChecklistProgressAvailability::PendingDispatch,
        );
        store.mark_agent_running(run.clone()).await?;
        let bridge = RunBridge::start(&store, run.clone()).await?;
        let report =
            |request: &str, id: &str, revision: i64, checked: bool| ChecklistProgressRequest {
                request_id: request.into(),
                item_id: id.into(),
                expected_revision: revision,
                checked,
            };
        let update = |request| exchange_test_checklist_progress(&bridge, request, true);
        assert_eq!(
            update(report("early", &item.id, 1, true)).await?["status"],
            "checklist_progress_rejected"
        );
        let pending = store
            .run_checklist_progress(project.clone(), run.clone())
            .await?
            .unwrap();
        assert_eq!(
            pending.availability,
            ChecklistProgressAvailability::PendingDispatch
        );
        assert_eq!(
            pending.selected_item_id,
            (mode == Mode::RunByItem).then(|| item.id.clone())
        );

        let mut assessment = CapabilityAssessment {
            version: 2,
            scope: CapabilityScope::from_dispatch(
                &profile,
                checkout.checkout_id,
                None,
                None,
                serde_json::from_value(json!({"detected":[],"unresolved":[]}))?,
                None,
                None,
            ),
            entries: vec![ResolvedCapability {
                descriptor: CapabilityDescriptor {
                    subject: CapabilitySubject::Protocol {
                        capability: Capability::RunScopedMcp,
                    },
                    source: DeclaredSource::Adapter,
                    availability: Availability::Unavailable,
                    blocker: None,
                    observed: None,
                    boundary: EvidenceBoundary::ExactCheckout,
                },
                evidence: None,
            }],
        };
        let context = store.task_dispatch_context(run.clone()).await?.unwrap();
        let dispatch = store
            .prepare_dispatch(
                run.clone(),
                snapshot(&attempt, context, &assessment)?,
                vec![b"fixture".to_vec()],
            )
            .await?;
        assert_eq!(
            update(report("prepared", &item.id, 1, true)).await?["status"],
            "checklist_progress_rejected"
        );
        store
            .settle_dispatch(dispatch.clone(), Delivery::Submitted)
            .await?;
        let advisory = store
            .run_checklist_progress(project.clone(), run.clone())
            .await?
            .unwrap();
        assert_eq!(
            advisory.availability,
            ChecklistProgressAvailability::BridgeUnavailable
        );
        assert_eq!(
            advisory.items.len(),
            if mode == Mode::RunByItem { 1 } else { 2 }
        );
        assert!(
            store
                .run_checklist_progress("another-project".into(), run.clone())
                .await?
                .is_none()
        );

        assessment.entries[0].descriptor.availability = Availability::Available;
        assessment.entries[0].descriptor.observed = Some(ObservedResult::Succeeded);
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Application,
                ProviderEvent {
                    location: Default::default(),
                    data: ProviderEventData::CapabilityAssessmentUpdated {
                        assessment: Box::new(assessment.clone()),
                    },
                },
            )
            .await?;
        assert_eq!(
            store
                .run_checklist_progress(project.clone(), run.clone())
                .await?
                .unwrap()
                .availability,
            ChecklistProgressAvailability::Ready,
        );

        let invalid =
            exchange_test_checklist_progress(&bridge, report("bad-key", &item.id, 1, true), false)
                .await?;
        assert_eq!(invalid["status"], "unavailable");
        let later_item = store
            .append_project_checklist_item(
                project.clone(),
                task.id.clone(),
                "Added after dispatch".into(),
            )
            .await?;
        assert_eq!(
            update(report("new-item", &later_item.id, 1, true)).await?["status"],
            "checklist_progress_rejected"
        );
        if mode == Mode::RunByItem {
            assert_eq!(
                update(report("sibling", &sibling.id, 1, true)).await?["status"],
                "checklist_progress_rejected"
            );
        } else {
            assert_eq!(
                update(report("sibling", &sibling.id, 1, true)).await?["status"],
                "checklist_progress_recorded"
            );
        }

        let accepted = update(report("first", &item.id, 1, true)).await?;
        assert_eq!(
            accepted,
            json!({"status":"checklist_progress_recorded", "payload":{"itemId":item.id,"revision":2,"checked":true}})
        );
        assert_eq!(update(report("first", &item.id, 1, true)).await?, accepted);
        assert_eq!(
            update(report("first", &item.id, 1, false)).await?["status"],
            "checklist_progress_request_conflict"
        );
        let reopened = store
            .revise_project_checklist_item(
                project.clone(),
                task.id.clone(),
                item.id.clone(),
                2,
                "Inspect the updated query".into(),
                false,
            )
            .await?;
        assert_eq!(reopened.revision, 3);
        assert!(!reopened.checked);
        let stale = update(report("after-operator", &item.id, 2, false)).await?;
        assert_eq!(
            stale,
            json!({"status":"checklist_progress_revision_conflict", "payload":{"current_revision":3}})
        );
        assert_eq!(update(report("first", &item.id, 1, true)).await?, accepted);
        let manually_checked = store
            .set_project_checklist_item_checked(
                project.clone(),
                task.id.clone(),
                item.id.clone(),
                3,
                true,
            )
            .await?;
        assert_eq!(manually_checked.revision, 4);
        assert!(manually_checked.checked);
        drop(bridge);
        drop(store);
        let store = Store::open(database)?;
        store.recover_agent_attempts_after_restart().await?;
        let recovered = store.job(project.clone(), job.id.clone()).await?.unwrap();
        let stopped = recovered
            .latest_attempt
            .context("missing interrupted attempt")?;
        assert_eq!(stopped.run_id, run);
        assert_eq!(stopped.status, RunStatus::Interrupted);
        assert!(RunBridge::start(&store, run.clone()).await.is_err());
        let history = store
            .checklist_progress_history(project.clone(), task.id.clone(), item.id.clone())
            .await?;
        assert_eq!(history.len(), 3);
        assert_eq!(
            history[0].actor,
            ChecklistProgressActor::Agent {
                run_id: run.clone(),
                dispatch_id: dispatch,
                request_id: "first".into()
            }
        );
        assert_eq!(history[0].before_revision, 1);
        assert_eq!(history[0].after.revision, 2);
        assert!(
            history[1..]
                .iter()
                .all(|event| event.actor == ChecklistProgressActor::Operator)
        );
        assert!(!history[1].after.checked);
        let current = store
            .project_checklist_item_revision(project.clone(), task.id.clone(), item.id.clone(), 4)
            .await?
            .unwrap();
        assert!(current.checked);
        assert_eq!(current.body, "Inspect the updated query");
        let saved = store
            .run_checklist_progress(project.clone(), run.clone())
            .await?
            .unwrap();
        assert_eq!(saved.availability, ChecklistProgressAvailability::Inactive);
        assert_eq!(saved.items[0].revision, 1);

        store
            .queue_job_recovery(project.clone(), job.id, run.clone(), RecoveryAction::Retry)
            .await?;
        let retry = store
            .claim_next_job(environment())
            .await?
            .context("missing restarted checklist attempt")?;
        let retry_run = retry.run.id.clone();
        assert_ne!(retry_run, run);
        assert_eq!(retry.previous_run_id.as_deref(), Some(run.as_str()));
        store.mark_agent_running(retry_run.clone()).await?;
        let bridge = RunBridge::start(&store, retry_run.clone()).await?;
        let context = store
            .task_dispatch_context(retry_run.clone())
            .await?
            .unwrap();
        assert_eq!(context.checklist_mode, Some(mode));
        assert_eq!(context.checklist.first(), Some(&current));
        let retry_dispatch = store
            .prepare_dispatch(
                retry_run.clone(),
                snapshot(&retry, context, &assessment)?,
                vec![b"retry fixture".to_vec()],
            )
            .await?;
        store
            .settle_dispatch(retry_dispatch.clone(), Delivery::Submitted)
            .await?;
        assert_eq!(
            exchange_test_checklist_progress(&bridge, report("first", &item.id, 4, false), true)
                .await?,
            json!({"status":"checklist_progress_recorded", "payload":{"itemId":item.id,"revision":5,"checked":false}})
        );
        store
            .finish_agent_attempt(retry_run.clone(), RunStatus::Succeeded, None)
            .await?;
        assert_eq!(
            exchange_test_checklist_progress(&bridge, report("late", &item.id, 5, true), true)
                .await?["status"],
            "unavailable"
        );
        let revised = store
            .checklist_progress_history(project, task.id, item.id)
            .await?;
        assert_eq!(revised.len(), 4);
        assert_eq!(&revised[..3], history);
        assert_eq!(
            revised[3].actor,
            ChecklistProgressActor::Agent {
                run_id: retry_run,
                dispatch_id: retry_dispatch,
                request_id: "first".into(),
            }
        );
        assert_eq!(revised[3].before_revision, 4);
        assert_eq!(revised[3].after.revision, 5);
        assert!(!revised[3].after.checked);
    }
    Ok(())
}
