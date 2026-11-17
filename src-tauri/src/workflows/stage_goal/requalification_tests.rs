use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    dispatch::EventSource,
    jobs::{
        AdmissionEnvironment, GoalControlState, GoalCriterionEvidence, GoalStopAction,
        GoalTurnAction, GoalVerificationStatus, JobRequest, QuestionTimer, QueueState,
    },
    providers::{
        Provider, ProviderEvent, ProviderEventData, ProviderLocation, RequestId, SessionIntent,
        SessionMode,
        questions::{QuestionAction, QuestionForm, QuestionOrigin, QuestionReply, QuestionRoute},
    },
    repository::{
        self, AttemptWorktreeEvidence, ProjectCheckout,
        verification::{CandidateSelection, CandidateTarget},
    },
    runs::RunStatus,
    store::Store,
    workflows::{
        ArtifactValue, NodeInputs, WorkflowHostRequest, WorkflowHostSubmission,
        tests::{ApprovedDeliveryFixture, approve_reviewed_plan_fixture, submission},
    },
};

use super::{
    StageGoalEvidence,
    tests::{begin, finish, goal_delivery, goal_submission},
};

#[tokio::test]
async fn saved_question_cannot_resume_a_stale_writer_until_the_goal_is_requalified() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("workflow-goal-requalification.sqlite");
    let store = Store::open(db.clone())?;
    let (fixture, request, _) = goal_delivery(
        &store,
        temp.path(),
        StageGoalEvidence::OperatorReview,
        vec![],
    )
    .await?;
    let project = fixture.project.clone();
    let workflow = fixture.run.id.clone();
    let old_release = request.dispatch.release_id.clone().unwrap();
    let environment = AdmissionEnvironment {
        runner: request.scope.runner.clone(),
        bridge_available: true,
    };
    store.submit_workflow_agent(request).await?;
    let first = store
        .claim_next_job(environment.clone())
        .await?
        .context("first managed turn was not scheduled")?;
    assert!(repository::capture_worktree_start(&store, first.run.id.clone()).await?);
    begin(&store, &first).await?;
    std::fs::write(
        first.scope.checkout.join("first.txt"),
        "prior bounded progress\n",
    )?;
    repository::save_worktree_candidate(&store, first.run.id.clone()).await?;
    assert_eq!(
        finish(&store, &first, GoalTurnAction::Continue).await?,
        RunStatus::Succeeded
    );

    let asking = store
        .claim_next_job(environment.clone())
        .await?
        .context("question turn was not scheduled")?;
    assert_eq!(
        asking.previous_run_id.as_deref(),
        Some(first.run.id.as_str())
    );
    assert!(repository::capture_worktree_start(&store, asking.run.id.clone()).await?);
    begin(&store, &asking).await?;
    let delegated = store
        .submit_agent_job(
            asking.run.id.clone(),
            "prior-scope-child".into(),
            JobRequest {
                checkout_id: None,
                task_id: None,
                mode: SessionMode::ReadOnly,
                requires_bridge: false,
                instructions: "Collect evidence under the current scope".into(),
                dependencies: Vec::new(),
            },
        )
        .await?;
    let request_id = RequestId::Number(92);
    let schema =
        json!({"type":"object","properties":{"narrow":{"type":"boolean"}},"required":["narrow"]});
    let question = "Should the implementation use the narrower plan?";
    store
        .record_agent_observation(
            asking.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation::default(),
                data: ProviderEventData::Question {
                    request_id: request_id.clone(),
                    form: json!({"message":question,"schema":schema}),
                    normalized: Some(QuestionForm::schema(question.into(), &schema)?),
                    route: QuestionRoute::Native,
                },
            },
        )
        .await?;
    assert!(matches!(
        store
            .poll_question_timer(asking.run.id.clone(), i64::MAX)
            .await?,
        QuestionTimer::Stop
    ));
    repository::save_worktree_candidate(&store, asking.run.id.clone()).await?;
    assert_eq!(
        store
            .finish_observed_agent_attempt(
                asking.run.id.clone(),
                RunStatus::Interrupted,
                Some("Waiting on a narrower plan".into()),
                None,
                true
            )
            .await?,
        RunStatus::Interrupted
    );
    assert_eq!(
        store
            .saved_conversation_questions(asking.conversation_id.clone(), asking.run.id.clone())
            .await?
            .len(),
        1
    );
    assert!(store.claim_next_job(environment.clone()).await?.is_none());
    store
        .answer_saved_question(
            asking.conversation_id.clone(),
            QuestionOrigin {
                run_id: asking.run.id.clone(),
                request_id,
            },
            QuestionReply {
                action: QuestionAction::Accept,
                values: [("narrow".into(), json!(true))].into_iter().collect(),
            },
        )
        .await?;
    let previous = store
        .active_pipeline_goal(project.clone(), workflow.clone(), "implement".into())
        .await?
        .context("previous Goal missing")?;
    assert_eq!(
        store
            .request_goal_stop(project.clone(), previous.id.clone(), GoalStopAction::Pause)
            .await?,
        GoalControlState::Paused
    );
    store
        .set_job_pipeline_plan(
            project.clone(),
            workflow.clone(),
            Some(fixture.plan.id.clone()),
            fixture.alternate_plan.id.clone(),
        )
        .await?;
    let blocked = store
        .inspect_job(
            project.clone(),
            previous.job.id.clone(),
            environment.clone(),
        )
        .await?
        .context("stale job was discarded")?;
    let revoked = blocked
        .job
        .pipeline
        .and_then(|origin| origin.release)
        .context("the previous release reference was lost")?;
    assert_eq!(revoked.id, old_release);
    assert!(revoked.revoked);
    let refusal = store
        .resume_goal(project.clone(), previous.id.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("approval") || refusal.contains("release"),
        "{refusal}"
    );
    assert!(
        store
            .preview_workflow_goal_requalification(goal_submission(&fixture)?)
            .await
            .is_err()
    );
    assert!(store.claim_next_job(environment.clone()).await?.is_none());

    let replacement = approve_narrower_plan(&store, temp.path(), &fixture, &environment).await?;
    let release = goal_submission(&replacement)?.dispatch.release_id.unwrap();
    assert_ne!(release, old_release);
    assert!(
        store
            .submit_workflow_agent(goal_submission(&replacement)?)
            .await
            .is_err()
    );
    let preview = store
        .preview_workflow_goal_requalification(goal_submission(&replacement)?)
        .await?;
    let pending_child = store
        .inspect_job(project.clone(), delegated.id.clone(), environment.clone())
        .await?
        .context("preview discarded historical delegation")?;
    assert_eq!(pending_child.job.queue_state, QueueState::Queued);
    assert_eq!(preview.prior_revision_id, previous.id);
    assert_eq!(preview.prior_checkpoint_run_id, asking.run.id);
    assert_eq!(preview.prior_scope.release_id, old_release);
    assert_eq!(preview.replacement_scope.release_id, release);
    assert_ne!(
        preview.prior_scope.plan_revision,
        preview.replacement_scope.plan_revision
    );
    assert_eq!(preview.turns_already_used, 2);
    assert!(preview.preserves_worktree_checkpoint);
    let mut changed = goal_submission(&replacement)?;
    changed.scope.model = Some("model changed since review".into());
    assert!(
        store
            .submit_workflow_goal_requalification(changed, preview.review_token.clone())
            .await
            .is_err()
    );
    let resumed = store
        .submit_workflow_goal_requalification(
            goal_submission(&replacement)?,
            preview.review_token.clone(),
        )
        .await?;
    assert!(
        store
            .submit_workflow_goal_requalification(
                goal_submission(&replacement)?,
                preview.review_token
            )
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(db)?;
    store.recover_agent_attempts_after_restart().await?;
    let retired_child = store
        .inspect_job(project.clone(), delegated.id, environment.clone())
        .await?
        .context("delegation history lost after requalification")?;
    assert_eq!(retired_child.job.queue_state, QueueState::Cancelled);
    assert!(retired_child.job.latest_attempt.is_none());
    let (prior_turns, original_deadline, current_deadline): (u32, Option<i64>, Option<i64>) = store.execute({
        let predecessor = previous.id.clone();
        move |db| Ok(db.query_row(
            "SELECT transfer.prior_turns, predecessor.deadline_ms, current.deadline_ms
             FROM workflow_goal_requalifications transfer JOIN goal_revisions predecessor ON predecessor.id = transfer.predecessor_revision_id
             JOIN goal_revisions current ON current.id = transfer.revision_id WHERE predecessor.id = ?1",
            [predecessor], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }).await?;
    assert_eq!(prior_turns, 2);
    assert!(original_deadline.is_some());
    assert_eq!(current_deadline, original_deadline);
    let next = store
        .claim_next_job(environment.clone())
        .await?
        .context("requalified Goal was not admitted")?;
    assert!(matches!(next.intent, SessionIntent::Start));
    assert_ne!(next.conversation_id, asking.conversation_id);
    assert_eq!(
        next.previous_run_id.as_deref(),
        Some(asking.run.id.as_str())
    );
    let prior_origin = store
        .attempt_pipeline_origin(project.clone(), asking.run.id.clone())
        .await?
        .unwrap();
    assert!(
        prior_origin
            .release
            .as_ref()
            .is_some_and(|item| item.revoked && item.id == old_release)
    );
    let pinned = store
        .workflow_attempt(project.clone(), next.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(pinned.job_id, resumed.id);
    assert_eq!(pinned.pin.release_id.as_deref(), Some(release.as_str()));
    assert_eq!(
        pinned.pin.plan_revision.as_deref(),
        Some(replacement.plan.id.as_str())
    );
    let timeline = store
        .conversation_question_timeline(asking.conversation_id.clone(), asking.run.id.clone())
        .await?;
    assert!(timeline.iter().any(|entry| entry.reply.is_some()));

    assert!(repository::capture_worktree_start(&store, next.run.id.clone()).await?);
    begin(&store, &next).await?;
    std::fs::write(
        next.scope.checkout.join("first.txt"),
        "accepted narrower plan\n",
    )?;
    repository::save_worktree_candidate(&store, next.run.id.clone()).await?;
    assert_eq!(
        finish(&store, &next, GoalTurnAction::Review).await?,
        RunStatus::Succeeded
    );
    let unit = &replacement.units[0];
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), next.run.id.clone())
        .await?
    else {
        anyhow::bail!("terminal candidate disappeared");
    };
    let candidate = store
        .select_worktree_candidate(
            CandidateTarget {
                project_id: project.clone(),
                task_id: unit.task_id.clone(),
                unit_id: unit.unit.id.clone(),
                capture_revision: artifact.revision,
                run_id: next.run.id.clone(),
            },
            CandidateSelection::default(),
        )
        .await?;
    let publish = || {
        store.publish_workflow_candidate(
            project.clone(),
            workflow.clone(),
            "implement".into(),
            unit.task_id.clone(),
            candidate.id.clone(),
        )
    };
    assert!(publish().await.is_err());
    let verified = store
        .record_goal_criterion_evidence(
            project.clone(),
            next.run.id.clone(),
            1,
            GoalCriterionEvidence::OperatorReview {
                approved: true,
                note: Some("Verified the approved narrower scope".into()),
            },
        )
        .await?;
    assert_eq!(verified.status, GoalVerificationStatus::Completed);
    assert_ne!(verified.revision_id, previous.id);
    assert!(
        matches!(publish().await?.value, ArtifactValue::Candidate(item) if item.id == candidate.id)
    );
    assert!(store.claim_next_job(environment).await?.is_none());
    Ok(())
}

async fn approve_narrower_plan(
    store: &Store,
    path: &std::path::Path,
    previous: &ApprovedDeliveryFixture,
    environment: &AdmissionEnvironment,
) -> Result<ApprovedDeliveryFixture> {
    let project = &previous.project;
    let ArtifactValue::Approval(decision) = &previous.decision.value else {
        anyhow::bail!("previous approval missing");
    };
    let repo = ProjectCheckout {
        project_id: project.clone(),
        checkout_id: decision.source_checkout_id.clone(),
        repository_root: previous.checkout.repository_root.clone(),
        checkout_root: previous.checkout.repository_root.clone(),
    };
    let plan = previous.alternate_plan.clone();
    let mut reviewer = submission(
        &repo,
        &previous.run,
        "review-plan",
        Some(plan.id.clone()),
        SessionMode::ReadOnly,
    );
    reviewer.scope.executable = PathBuf::from(&repo.checkout_root).join("codex.exe");
    reviewer.scope.provider = Provider::Claude;
    reviewer.scope.profile_id = "replacement-reviewer".into();
    reviewer.artifacts = NodeInputs::from([("plan".into(), plan.id.clone())]);
    store.submit_workflow_agent(reviewer).await?;
    let review = store
        .claim_next_job(environment.clone())
        .await?
        .context("updated plan review was not scheduled")?;
    store.mark_agent_running(review.run.id.clone()).await?;
    store
        .finish_agent_attempt(
            review.run.id.clone(),
            RunStatus::Succeeded,
            Some("Reviewed the narrower plan".into()),
        )
        .await?;
    let review = store
        .publish_workflow_artifact(
            project.clone(),
            previous.run.id.clone(),
            "review-plan".into(),
            "review".into(),
            ArtifactValue::Review {
                run_id: review.run.id,
            },
        )
        .await?;
    let linked = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: project.clone(),
            run_id: previous.run.id.clone(),
            node_id: "work".into(),
            inputs: NodeInputs::from([
                ("plan".into(), plan.id.clone()),
                ("review".into(), review.id),
            ]),
            request: WorkflowHostRequest::LinkExistingTask {
                task_id: decision.task.task_id.clone(),
            },
        })
        .await?;
    approve_reviewed_plan_fixture(
        store,
        path,
        repo,
        previous.run.clone(),
        (linked, plan, previous.plan.clone()),
    )
    .await
}
