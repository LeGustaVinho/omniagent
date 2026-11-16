use super::*;
use crate::tasks::ChecklistExecutionMode::{RunByItem, TrackProgress};

#[tokio::test]
async fn agile_checklist_actions_use_the_active_release_and_pin_both_execution_modes() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("agile.sqlite"))?;
    let fixture = approved_delivery_fixture(&store, temp.path()).await?;
    let unit = &fixture.units[0];
    let project = &fixture.project;
    let task = &unit.task_id;
    let target = store
        .project_checklist_dispatch(project.clone(), task.clone())
        .await?
        .context("approved delivery has no checklist release")?;
    let ArtifactValue::Release(approved) = &fixture.release.value else {
        anyhow::bail!("fixture release is missing")
    };
    assert_eq!(target.release_id, approved.release.id);
    assert_eq!(target.checkout_id, unit.checkout_id);
    assert_eq!(target.plan_revision, unit.plan_revision);
    assert_eq!(target.unit_id, unit.unit.id);
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    let scope = implementation_submission(&fixture, &worktree, unit)?.scope;
    let environment = AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    };
    assert!(
        store
            .queue_project_checklist_work(
                project.clone(),
                task.clone(),
                "old-release".into(),
                target.unit_id.clone(),
                None,
                scope.clone()
            )
            .await
            .is_err()
    );

    store
        .ensure_project_checklist(project.clone(), task.clone())
        .await?;
    let first = store
        .append_project_checklist_item(
            project.clone(),
            task.clone(),
            "Check response mapping".into(),
        )
        .await?;
    let second = store
        .append_project_checklist_item(project.clone(), task.clone(), "Review all branches".into())
        .await?;
    let current = store
        .project_work_fields(project.clone(), task.clone())
        .await?;
    store
        .set_project_task_assignment(
            project.clone(),
            task.clone(),
            "implementer".into(),
            Some(crate::model_catalog::ModelSelection {
                provider: Provider::Codex,
                profile_id: scope.profile_id.clone(),
                model_id: "planned".into(),
            }),
        )
        .await?;
    let details = store
        .project_task_details(project.clone(), task.clone())
        .await?
        .context("missing work")?;
    let mut next = current.clone();
    next.title = "Check the integration".into();
    next.acceptance_criteria = vec!["Retain existing API behavior".into()];
    next.priority = 5;
    assert_eq!(
        store
            .update_project_work_fields(
                project.clone(),
                task.clone(),
                current.clone(),
                next.clone()
            )
            .await?,
        next
    );
    assert!(
        store
            .update_project_work_fields(project.clone(), task.clone(), current.clone(), current)
            .await
            .is_err()
    );
    let persisted = store
        .project_task_details(project.clone(), task.clone())
        .await?
        .context("missing work")?;
    assert_eq!(persisted.planned_assignments, details.planned_assignments);
    assert_eq!(persisted.workflow_state, details.workflow_state);

    for (mode, item, planned_items) in [
        (TrackProgress, None, vec![first.clone(), second.clone()]),
        (RunByItem, Some(second.id.clone()), vec![second.clone()]),
    ] {
        store
            .set_task_checklist_mode(project.clone(), task.clone(), Some(mode))
            .await?;
        let settings = store
            .task_checklist_mode_settings(project.clone(), task.clone())
            .await?;
        assert_eq!(settings.override_mode, Some(mode));
        assert_eq!(settings.effective_mode, mode);
        let job = store
            .queue_project_checklist_work(
                project.clone(),
                task.clone(),
                target.release_id.clone(),
                target.unit_id.clone(),
                item.clone(),
                scope.clone(),
            )
            .await?;
        assert_eq!(job.checkout_id, target.checkout_id);
        assert_eq!(
            job.pipeline
                .as_ref()
                .and_then(|origin| origin.release.as_ref())
                .map(|release| release.id.as_str()),
            Some(target.release_id.as_str())
        );
        let attempt = claim_fixture_job(&store, job, environment.clone()).await?;
        let pinned = store
            .workflow_attempt(project.clone(), attempt.run.id.clone())
            .await?
            .context("checklist attempt lost its typed workflow binding")?;
        assert_eq!(pinned.pin.unit_id.as_ref(), Some(&target.unit_id));
        let context = store
            .task_dispatch_context(attempt.run.id.clone())
            .await?
            .context("checklist attempt lost its work context")?;
        assert_eq!(context.checklist_item_id, item);
        assert_eq!(context.checklist_mode, Some(mode));
        assert_eq!(context.checklist, planned_items);
        assert_eq!(
            context
                .direct_work
                .context("missing direct work")?
                .acceptance_criteria,
            next.acceptance_criteria
        );
        store.mark_agent_running(attempt.run.id.clone()).await?;
        ensure!(repository::capture_worktree_start(&store, attempt.run.id.clone()).await?);
        repository::save_worktree_candidate(&store, attempt.run.id.clone()).await?;
        store
            .finish_agent_attempt(attempt.run.id, RunStatus::Succeeded, None)
            .await?;
    }
    Ok(())
}
