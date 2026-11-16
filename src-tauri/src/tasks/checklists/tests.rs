use std::{path::Path, process::Command};

use anyhow::{Context, Result, ensure};

use crate::{
    conversations::SessionScope,
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, Job, JobPermission, JobRequest, JobRole, JobRule,
        JobWorkspace, PipelineAgentInput, PipelineDispatch, PipelineNodeKind, RecoveryAction,
        ReleaseBlocker,
    },
    providers::{Provider, SessionMode},
    repository::{self, ProjectCheckout, provision_task_worktree},
    runner::{DirectWindowsRunner, Runner},
    runs::RunStatus,
    store::Store,
    tasks::{ChecklistExecutionMode as Mode, DescriptionOwner, TaskDetails, TaskRunScope},
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
async fn checklist_write_waits_for_the_current_worktree_release() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("repo");
    let store = Store::open(temp.path().join("store.sqlite3"))?;
    let source = register(&store, &root).await?;
    let project = source.project_id;
    let task = store
        .create_project_task(project.clone(), "Implement API change".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        source.checkout_id,
    )
    .await?;
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
