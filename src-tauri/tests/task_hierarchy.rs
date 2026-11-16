use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    dispatch::EventSource,
    jobs::{AdmissionEnvironment, JobRequest, PipelineDispatch, PipelineNodeKind, RecoveryAction},
    providers::{Provider, ProviderEvent, ProviderEventData, SessionMode},
    repository::{self, provision_task_worktree},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    tasks::{
        PlannedAssignment, ProjectWork, TaskDetails, TaskPlacement, TaskRunRole, TaskRunScope,
        TaskWorkflowState,
    },
};

fn git(checkout: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(checkout)
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
async fn legacy_task_ids_jobs_and_worktrees_survive_the_hierarchy_migration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    std::fs::create_dir(&source)?;
    git(&source, &["init", "--quiet"])?;
    git(
        &source,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "Base",
        ],
    )?;
    let database = temp.path().join("omniagent.sqlite3");
    let store = Store::open(database.clone())?;
    let project = repository::register(&store, source.clone()).await?;
    let project_id = project.project_id.clone();
    let legacy_task = store
        .create_project_task(project_id.clone(), "Existing task".into())
        .await?;
    let worktree_root = temp.path().join("worktrees");
    let worktree = provision_task_worktree(
        &store,
        worktree_root.clone(),
        project_id.clone(),
        legacy_task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    let conversation = store
        .create_conversation(project.checkout_id.clone(), "Legacy fixture".into())
        .await?;
    let parent = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "fixture".into(),
                account_binding: None,
                checkout: dunce::canonicalize(source)?,
                executable: std::env::current_exe()?,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "fixture-read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            "Legacy job origin".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let legacy_job = store
        .submit_agent_job(
            parent.run.id.clone(),
            "legacy-request".into(),
            JobRequest {
                checkout_id: Some(worktree.checkout_id.clone()),
                task_id: Some(legacy_task.id.clone()),
                mode: SessionMode::ReadOnly,
                instructions: "Inspect the managed checkout".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
        )
        .await?;
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Succeeded, None)
        .await?;
    drop(store);

    let legacy = rusqlite::Connection::open(&database)?;
    legacy.execute_batch(
        "DROP TABLE task_branch_strategies;
         DROP TABLE project_branch_strategies;
         ALTER TABLE pending_worktree_changes DROP COLUMN discard_starting_head;
         DROP TABLE checklist_progress_events;
         DROP TABLE job_checklist_modes;
         DROP TABLE task_checklist_modes;
         DROP TABLE project_checklist_modes;
         DROP TABLE job_role_contexts;
         DROP TABLE project_context_review_events;
         DROP TABLE project_context_revisions;
         DROP TABLE project_context_records;
         DROP TABLE job_repository_markdown;
         DROP TABLE repository_markdown_revisions;
         DROP TABLE project_description_revisions;
         DROP VIEW task_runs;
         DROP TABLE job_task_run_scopes;
         DROP TABLE project_checklist_item_revisions;
         DROP TABLE project_checklist_items;
         DROP TABLE project_task_prerequisites;
         DROP TABLE project_task_details;
         DROP TABLE project_checklists;
         DROP TABLE project_task_memberships;
         DROP TABLE project_epics;
         PRAGMA user_version = 51;",
    )?;
    drop(legacy);

    let store = Store::open(database)?;
    let project_view = store.project_work_hierarchy(project_id.clone()).await?;
    assert!(project_view.epics.is_empty());
    assert_eq!(
        project_view.work,
        [ProjectWork {
            task: legacy_task.clone(),
            placement: TaskPlacement::Task { epic_id: None },
            checklist_id: None,
        }]
    );
    assert_eq!(
        store.job(project_id.clone(), legacy_job.id.clone()).await?,
        Some(legacy_job)
    );
    assert_eq!(
        provision_task_worktree(
            &store,
            worktree_root.clone(),
            project_id.clone(),
            legacy_task.id.clone(),
            project.checkout_id.clone()
        )
        .await?,
        worktree,
    );

    let epic = store
        .create_project_epic(project_id.clone(), "Milestone".into())
        .await?;
    store
        .set_project_task_epic(
            project_id.clone(),
            legacy_task.id.clone(),
            Some(epic.id.clone()),
        )
        .await?;
    let child = store
        .create_project_subtask(project_id.clone(), legacy_task.id.clone(), "Child".into())
        .await?;
    let root_checklist = store
        .ensure_project_checklist(project_id.clone(), legacy_task.id.clone())
        .await?;
    let child_checklist = store
        .ensure_project_checklist(project_id.clone(), child.id.clone())
        .await?;
    assert_ne!(root_checklist, child_checklist);
    assert_eq!(
        store
            .ensure_project_checklist(project_id.clone(), child.id.clone())
            .await?,
        child_checklist
    );

    let other = temp.path().join("other");
    std::fs::create_dir(&other)?;
    git(&other, &["init", "--quiet"])?;
    let other_project = repository::register(&store, other).await?.project_id;
    let foreign_epic = store
        .create_project_epic(other_project.clone(), "Foreign epic".into())
        .await?;
    assert!(
        store
            .set_project_task_epic(
                project_id.clone(),
                legacy_task.id.clone(),
                Some(foreign_epic.id)
            )
            .await
            .is_err()
    );
    assert!(
        store
            .set_project_task_epic(project_id.clone(), child.id.clone(), Some(epic.id.clone()))
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(
                other_project.clone(),
                legacy_task.id.clone(),
                "Cross-project child".into()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(project_id.clone(), child.id.clone(), "Grandchild".into())
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_subtask(project_id.clone(), epic.id.clone(), "Wrong parent".into())
            .await
            .is_err()
    );
    assert!(
        store
            .ensure_project_checklist(project_id.clone(), epic.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .ensure_project_checklist(other_project.clone(), child.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .project_work_hierarchy(other_project)
            .await?
            .work
            .is_empty()
    );

    let hierarchy = store.project_work_hierarchy(project_id.clone()).await?;
    assert_eq!(hierarchy.epics.as_slice(), [&epic].map(Clone::clone));
    assert_eq!(
        hierarchy.work,
        [
            ProjectWork {
                task: child.clone(),
                placement: TaskPlacement::Subtask {
                    parent_task_id: legacy_task.id.clone()
                },
                checklist_id: Some(child_checklist),
            },
            ProjectWork {
                task: legacy_task.clone(),
                placement: TaskPlacement::Task {
                    epic_id: Some(epic.id)
                },
                checklist_id: Some(root_checklist),
            },
        ]
    );
    assert_eq!(
        store
            .project_task(project_id.clone(), child.id.clone())
            .await?,
        Some(child.clone())
    );
    let child_worktree = provision_task_worktree(
        &store,
        worktree_root,
        project_id.clone(),
        child.id,
        project.checkout_id,
    )
    .await
    .context("sub-task should be a worktree scope")?;
    assert_ne!(child_worktree.checkout_id, worktree.checkout_id);

    store
        .set_project_task_epic(project_id.clone(), legacy_task.id, None)
        .await?;
    let ungrouped = store.project_work_hierarchy(project_id).await?;
    assert_eq!(ungrouped.work[0].placement, hierarchy.work[0].placement);
    assert_eq!(
        ungrouped.work[1].placement,
        TaskPlacement::Task { epic_id: None }
    );
    Ok(())
}

#[tokio::test]
async fn checklist_attempt_retains_its_role_and_actual_profile_after_reassignment_and_restart()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let checkout = temp.path().join("source");
    std::fs::create_dir(&checkout)?;
    git(&checkout, &["init", "--quiet"])?;
    let database = temp.path().join("omniagent.sqlite3");
    let store = Store::open(database.clone())?;
    let repository = repository::register(&store, checkout.clone()).await?;
    let project = repository.project_id;
    let parent = store
        .create_project_task(project.clone(), "Parent".into())
        .await?;
    let child = store
        .create_project_subtask(project.clone(), parent.id.clone(), "Child".into())
        .await?;
    assert_eq!(
        store
            .project_task_details(project.clone(), child.id.clone())
            .await?,
        Some(TaskDetails::default())
    );
    let mut details = TaskDetails {
        priority: 3,
        workflow_state: TaskWorkflowState::Blocked,
        prerequisites: vec![parent.id.clone()],
        acceptance_criteria: vec!["Check each changed endpoint".into()],
        blockers: vec!["Waiting for fixtures".into()],
        planned_assignments: vec![PlannedAssignment {
            role: "reviewer".into(),
            provider: Provider::Codex,
            profile_id: "initial-profile".into(),
            model: Some("requested-model".into()),
        }],
    };
    store
        .set_project_task_details(project.clone(), child.id.clone(), details.clone())
        .await?;
    assert!(
        store
            .set_project_task_details(
                project.clone(),
                parent.id.clone(),
                TaskDetails {
                    prerequisites: vec![child.id.clone()],
                    priority: 12,
                    ..TaskDetails::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .project_task_details(project.clone(), parent.id.clone())
            .await?,
        Some(TaskDetails::default())
    );
    assert!(
        store
            .set_project_task_details(
                project.clone(),
                child.id.clone(),
                TaskDetails {
                    prerequisites: vec!["missing-task".into()],
                    ..TaskDetails::default()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .project_task_details(project.clone(), child.id.clone())
            .await?,
        Some(details.clone())
    );

    store
        .ensure_project_checklist(project.clone(), child.id.clone())
        .await?;
    let item = store
        .append_project_checklist_item(
            project.clone(),
            child.id.clone(),
            "Review the API changes".into(),
        )
        .await?;
    let next_item = store
        .append_project_checklist_item(
            project.clone(),
            child.id.clone(),
            "Review the UI changes".into(),
        )
        .await?;
    assert_eq!(item.revision, 1);
    assert!(!item.checked);
    assert_eq!(next_item.position, item.position + 1);

    let pipeline = "review-pipeline".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "review".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    let binding = PipelineDispatch {
        pipeline_run_id: pipeline,
        node_instance_id: "review".into(),
        plan_revision: None,
        release_id: None,
    };
    let profile = SessionScope {
        provider: Provider::Codex,
        profile_id: "initial-profile".into(),
        account_binding: None,
        checkout: dunce::canonicalize(checkout)?,
        executable: std::env::current_exe()?,
        cli_version: "fixture".into(),
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "read-only-fixture".into(),
        mode: SessionMode::ReadOnly,
        model: Some("requested-model".into()),
        effort: None,
    };
    let request = JobRequest {
        checkout_id: Some(repository.checkout_id),
        task_id: Some(child.id.clone()),
        mode: SessionMode::ReadOnly,
        instructions: "Review the selected checklist item".into(),
        dependencies: Vec::new(),
        requires_bridge: false,
    };
    let target = TaskRunScope {
        role: "reviewer".into(),
        checklist_item_id: Some(item.id.clone()),
    };
    assert!(
        store
            .submit_pipeline_task_agent_job(
                binding.clone(),
                profile.clone(),
                JobRequest {
                    task_id: Some(parent.id.clone()),
                    ..request.clone()
                },
                target.clone()
            )
            .await
            .is_err()
    );
    let env = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: false,
    };
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    let job = store
        .submit_pipeline_task_agent_job(binding, profile, request, target)
        .await?;
    let attempt = store
        .claim_next_job(env.clone())
        .await?
        .context("missing scheduled job")?;
    let run_id = attempt.run.id;
    let starting = store
        .project_task_runs(project.clone(), child.id.clone())
        .await?;
    assert_eq!(starting.len(), 1);
    assert_eq!(starting[0].job_id, job.id);
    assert_eq!(starting[0].run_id, run_id);
    assert_eq!(starting[0].turn_id, attempt.run.turn_id);
    assert_eq!(starting[0].status, RunStatus::Starting);
    assert_eq!(starting[0].started_at, None);
    assert_eq!(starting[0].reported_model, None);
    store.mark_agent_running(run_id.clone()).await?;
    store
        .record_agent_observation(
            run_id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::ModelObserved {
                    model: Some("observed-model".into()),
                    effort: None,
                },
            },
        )
        .await?;
    store
        .finish_agent_attempt(run_id.clone(), RunStatus::Failed, None)
        .await?;

    details.planned_assignments[0].provider = Provider::Claude;
    details.planned_assignments[0].profile_id = "reassigned-profile".into();
    details.planned_assignments[0].model = None;
    store
        .set_project_task_details(project.clone(), child.id.clone(), details.clone())
        .await?;
    drop(store);
    let store = Store::open(database)?;
    assert_eq!(
        store
            .project_task_details(project.clone(), child.id.clone())
            .await?,
        Some(details)
    );
    let saved = store
        .project_task_runs(project.clone(), child.id.clone())
        .await?;
    assert_eq!(saved.len(), 1);
    let first = &saved[0];
    assert_eq!(first.role, TaskRunRole::Workflow("reviewer".into()));
    assert_eq!(first.checklist_item_id.as_ref(), Some(&item.id));
    assert_eq!(first.provider, Provider::Codex);
    assert_eq!(first.profile_id, "initial-profile");
    assert_eq!(first.requested_model.as_deref(), Some("requested-model"));
    assert_eq!(first.reported_model.as_deref(), Some("observed-model"));
    assert_eq!(first.status, RunStatus::Failed);
    let start = first.started_at.context("missing start time")?;
    let end = first.finished_at.context("missing terminal time")?;
    assert!(first.created_at <= start && start <= end);
    assert!(
        store
            .project_task_runs(project.clone(), parent.id)
            .await?
            .is_empty()
    );
    assert!(
        store
            .project_task_runs("other-project".into(), child.id.clone())
            .await?
            .is_empty()
    );

    store
        .queue_job_recovery(project.clone(), job.id, run_id, RecoveryAction::Retry)
        .await?;
    let retry = store
        .claim_next_job(env)
        .await?
        .context("missing retried job")?;
    let history = store.project_task_runs(project, child.id).await?;
    assert_eq!(history.len(), 2);
    assert_eq!(&history[0], first);
    assert_eq!(history[1].ordinal, 2);
    assert_eq!(history[1].run_id, retry.run.id);
    assert_eq!(history[1].role, first.role);
    assert_eq!(history[1].checklist_item_id, first.checklist_item_id);
    assert_eq!(history[1].reported_model, None);
    Ok(())
}
