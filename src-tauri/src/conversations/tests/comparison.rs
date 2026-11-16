use super::*;

use crate::{
    jobs::{AdmissionEnvironment, FailureCause, FailureObservation, FailurePhase, QueueSettings},
    repository::ProjectCheckout,
};

async fn comparison_repository() -> Result<(
    tempfile::TempDir,
    Store,
    ProjectCheckout,
    SessionScope,
    String,
)> {
    let (directory, store, _, scope) = setup().await?;
    std::fs::write(&scope.executable, b"Fixture executable identity")?;
    let original = scope.checkout.join("review.txt");
    std::fs::write(&original, "Committed comparison input")?;
    std::fs::write(
        scope.checkout.join("AGENTS.md"),
        "Review only the pinned repository.",
    )?;
    let git = |args: &[&str]| -> Result<()> {
        ensure!(
            Command::new("git.exe")
                .current_dir(&scope.checkout)
                .args(args)
                .status()?
                .success(),
            "fixture Git command failed"
        );
        Ok(())
    };
    git(&["add", "--", "review.txt", "AGENTS.md"])?;
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgSign=false",
        "commit",
        "-qm",
        "Original review",
    ])?;
    let commit = crate::repository::base_commit(&scope.checkout)?.unwrap();
    std::fs::write(&original, "Unsaved source edit")?;
    let repo = crate::repository::register(&store, scope.checkout.clone()).await?;
    Ok((directory, store, repo, scope, commit))
}

#[tokio::test]
async fn comparison_runs_use_one_unregistered_checkout_at_the_pinned_git_commit() -> Result<()> {
    let (_directory, store, repo, scope, commit) = comparison_repository().await?;
    let comparison_folder = tempfile::tempdir()?;
    let prompt = "Review the tracked comparison input.";
    let mut second = scope.clone();
    second.model = Some("second-codex-model".into());
    let mut writer = second.clone();
    writer.mode = SessionMode::ApprovedWrite;
    assert!(
        store
            .start_model_comparison(
                repo.project_id.clone(),
                repo.checkout_id.clone(),
                commit.clone(),
                prompt.into(),
                vec![scope.clone(), writer],
                comparison_folder.path().to_path_buf()
            )
            .await
            .is_err()
    );
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let comparison = store
        .start_model_comparison(
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            commit.clone(),
            prompt.into(),
            vec![scope.clone(), second],
            comparison_folder.path().to_path_buf(),
        )
        .await?;
    assert!(
        comparison
            .candidates
            .iter()
            .all(|candidate| candidate.state == "queued" && candidate.run_id.is_none())
    );

    let environment = AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    };
    let mut run_ids = Vec::new();
    let mut paths = Vec::new();
    for (index, candidate) in comparison.candidates.iter().enumerate() {
        let plan = store
            .claim_next_job(environment.clone())
            .await?
            .context("candidate not scheduled")?;
        let run_id = plan.run.id.clone();
        let group = store
            .model_comparisons(repo.project_id.clone())
            .await?
            .remove(0);
        assert_eq!(group.candidates[index].state, "queued");
        assert_eq!(
            group.candidates[index].run_id.as_deref(),
            Some(run_id.as_str())
        );
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        assert!(
            matches!(plan.intent, SessionIntent::Start) && plan.scope.mode == SessionMode::ReadOnly
        );
        let pin = store
            .model_comparison_pin_for_run(run_id.clone())
            .await?
            .context("comparison pin missing")?;
        assert_eq!(pin.group_id, comparison.id);
        assert_eq!(pin.commit, commit);
        let root = pin.checkout()?.root;
        assert_eq!(
            std::fs::read_to_string(root.join("review.txt"))?,
            "Committed comparison input"
        );
        assert_ne!(root, scope.checkout);
        assert!(
            crate::repository::register(&store, root.clone())
                .await
                .is_err()
        );
        paths.push(root.clone());
        let owner = plan.conversation_id.clone();
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: owner.clone(),
            frames: Arc::default(),
        }));
        assert_eq!(
            start_managed_turn(store.clone(), runner, plan, None)
                .wait()
                .await?,
            RunStatus::Succeeded
        );
        let saved = store
            .agent_dispatch(owner, run_id.clone())
            .await?
            .context("comparison dispatch missing")?;
        assert_eq!(saved.delivery, Delivery::Submitted);
        assert_eq!(saved.snapshot.scope.checkout, scope.checkout);
        assert_eq!(
            saved
                .snapshot
                .instructions
                .as_ref()
                .map(|inventory| &inventory.working_directory),
            Some(&root)
        );
        assert_eq!(
            saved.snapshot.scope.model.as_deref(),
            Some(candidate.selection.model_id.as_str())
        );
        assert_eq!(saved.snapshot.submitted_messages[0].text, prompt);
        let launch = saved.snapshot.scoped_launch.as_ref().unwrap();
        assert!(
            launch
                .inputs
                .iter()
                .any(|entry| entry.name == "Comparison context"
                    && entry.contents.contains(&comparison.id))
        );
        run_ids.push(run_id);
    }
    assert_eq!(paths[0], paths[1]);
    assert_ne!(run_ids[0], run_ids[1]);
    assert_eq!(
        std::fs::read_to_string(scope.checkout.join("review.txt"))?,
        "Unsaved source edit"
    );
    let database = scope.checkout.join("store.sqlite");
    drop(store);
    let reopened = Store::open(database)?;
    let recovered = reopened
        .model_comparisons(repo.project_id.clone())
        .await?
        .remove(0);
    assert_eq!(recovered.commit, commit);
    assert_eq!(
        recovered
            .candidates
            .iter()
            .map(|candidate| candidate.run_id.clone().unwrap())
            .collect::<Vec<_>>(),
        run_ids
    );
    assert!(
        recovered
            .candidates
            .iter()
            .all(|candidate| candidate.state == "succeeded" && candidate.dispatch_id.is_some())
    );
    let environments = reopened
        .model_comparison_environments(repo.project_id, recovered.id)
        .await?;
    let environments = serde_json::to_value(environments)?;
    assert_eq!(
        environments[0]["dispatchId"].as_str(),
        recovered.candidates[0].dispatch_id.as_deref()
    );
    assert_eq!(
        environments[1]["dispatchId"].as_str(),
        recovered.candidates[1].dispatch_id.as_deref()
    );
    assert!(
        environments[0]["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|instruction| instruction["path"] == "AGENTS.md"
                && instruction["observedRead"] == false)
    );
    assert!(
        !environments[0]["observedTools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

// Behavioral cases adapted from MCO tests/test_invocation_runtime.py at
// https://github.com/mco-org/mco/blob/7ce642193a127f3fac9c68fb10c5d0198d4e837e/tests/test_invocation_runtime.py#L348-L395
// MCO's MIT notice is retained in tests/fixtures/LICENSE-MCO.
#[tokio::test]
async fn comparison_preserves_output_order_and_success_after_another_candidate_times_out()
-> Result<()> {
    use crate::dispatch::{HistoryDeletionMode, HistoryDeletionTarget};
    use crate::workflows::comparison::ComparisonSelection;
    let (_directory, store, repo, scope, commit) = comparison_repository().await?;
    let storage = tempfile::tempdir()?;
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 2,
        })
        .await?;
    let profiles = ["first", "second", "third"]
        .into_iter()
        .map(|model| SessionScope {
            model: Some(model.into()),
            ..scope.clone()
        })
        .collect();
    let group = store
        .start_model_comparison(
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            commit,
            "Review the pinned input".into(),
            profiles,
            storage.path().to_path_buf(),
        )
        .await?;
    let environment = AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    };
    let first = store.claim_next_job(environment.clone()).await?.unwrap();
    let second = store.claim_next_job(environment.clone()).await?.unwrap();
    assert!(store.claim_next_job(environment.clone()).await?.is_none());
    let first_run = first.run.id;
    let second_run = second.run.id.clone();
    let execute = |plan: AttemptPlan| {
        let runner = Arc::new(FixtureRunner(WireLog {
            store: store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: plan.conversation_id.clone(),
            frames: Arc::default(),
        }));
        start_managed_turn(store.clone(), runner, plan, None)
    };
    assert_eq!(execute(second).wait().await?, RunStatus::Succeeded);
    let in_progress = store
        .model_comparisons(repo.project_id.clone())
        .await?
        .remove(0);
    assert_eq!(
        in_progress
            .candidates
            .iter()
            .map(|candidate| candidate.state.as_str())
            .collect::<Vec<_>>(),
        ["queued", "succeeded", "queued"]
    );
    store
        .finish_observed_agent_attempt(
            first_run.clone(),
            RunStatus::Failed,
            None,
            Some(FailureObservation {
                phase: FailurePhase::Startup,
                cause: Some(FailureCause::StartupTimeout),
                process: None,
            }),
            true,
        )
        .await?;
    let third = store.claim_next_job(environment.clone()).await?.unwrap();
    let third_run = third.run.id.clone();
    assert_eq!(execute(third).wait().await?, RunStatus::Succeeded);
    let result = store
        .model_comparisons(repo.project_id.clone())
        .await?
        .remove(0);
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.state.as_str())
            .collect::<Vec<_>>(),
        ["failed", "succeeded", "succeeded"]
    );
    let outcomes = serde_json::to_value(
        store
            .model_comparison_outcomes(repo.project_id.clone(), group.id.clone())
            .await?,
    )?;
    for (index, run) in [&first_run, &second_run, &third_run]
        .into_iter()
        .enumerate()
    {
        assert_eq!(outcomes[index]["runId"], run.as_str());
        assert_eq!(outcomes[index]["jobId"], result.candidates[index].job_id);
    }
    assert_eq!(outcomes[0]["failure"]["cause"], "startup_timeout");
    assert!(outcomes[0]["response"].is_null());
    for outcome in [&outcomes[1], &outcomes[2]] {
        assert_eq!(outcome["response"], "Fixture answer");
        assert_eq!(outcome["inputTokens"], 5);
        assert_eq!(outcome["outputTokens"], 3);
        assert!(outcome["durationSeconds"].is_i64());
    }

    let selection = |run_ids| ComparisonSelection {
        group_id: group.id.clone(),
        run_ids,
    };
    for invalid in [
        vec![second_run.clone(), second_run.clone()],
        vec![first_run.clone()],
        vec!["unrelated-run".into()],
    ] {
        assert!(
            store
                .start_model_comparison_synthesis(
                    repo.project_id.clone(),
                    selection(invalid),
                    scope.clone()
                )
                .await
                .is_err()
        );
    }
    let chosen = selection(vec![third_run.clone(), second_run.clone()]);
    let synthesis = store
        .start_model_comparison_synthesis(repo.project_id.clone(), chosen.clone(), scope.clone())
        .await?;
    assert_eq!(
        synthesis
            .inputs
            .iter()
            .map(|source| source.run_id.clone())
            .collect::<Vec<_>>(),
        [second_run.clone(), third_run.clone()]
    );
    assert!(synthesis.run_id.is_none());
    let attempt = store
        .claim_next_job(environment.clone())
        .await?
        .context("Separate synthesis was not admitted")?;
    let synthesis_run = attempt.run.id.clone();
    let pin = store
        .model_comparison_pin_for_run(synthesis_run.clone())
        .await?
        .context("Missing synthesis checkout pin")?;
    assert_eq!(pin.group_id, group.id);
    assert_ne!(pin.checkout()?.root, scope.checkout);
    pin.verify_attempt(&attempt)?;
    let context: serde_json::Value =
        serde_json::from_str(attempt.input[0].text.split_once("\n\n").unwrap().1)?;
    assert_eq!(context["responses"][0]["run"]["runId"], second_run);
    assert_eq!(context["responses"][1]["run"]["runId"], third_run);
    assert_eq!(context["responses"].as_array().unwrap().len(), 2);
    assert!(![&first_run, &second_run, &third_run].contains(&&synthesis_run));
    assert_eq!(execute(attempt).wait().await?, RunStatus::Succeeded);
    let reopened = Store::open(scope.checkout.join("store.sqlite"))?;
    let results = reopened
        .model_comparison_syntheses(repo.project_id.clone(), group.id.clone())
        .await?;
    let saved = &results[0];
    assert_eq!(saved.inputs, synthesis.inputs);
    assert_eq!(saved.run_id.as_deref(), Some(synthesis_run.as_str()));
    assert_eq!(saved.response.as_deref(), Some("Fixture answer"));
    assert!(
        reopened
            .model_comparison_syntheses("other project".into(), group.id.clone())
            .await?
            .is_empty()
    );

    let (review, review_job) =
        queue_plan_review_using_comparison(&store, &repo, &scope, chosen, &environment).await?;
    assert_eq!(
        reopened
            .model_comparison_job_inputs(repo.project_id.clone(), review_job.clone())
            .await?,
        saved.inputs
    );
    assert!(
        reopened
            .model_comparison_job_inputs("other project".into(), review_job)
            .await
            .is_err()
    );
    assert!(
        review.input[0].text.contains(&second_run) && review.input[0].text.contains(&third_run)
    );
    assert!(!review.input[0].text.contains(&first_run));
    finish_comparison_review(&store, &review).await?;

    let target = HistoryDeletionTarget::Message {
        conversation_id: result.candidates[1].conversation_id.clone().unwrap(),
        message_revision_id: saved.inputs[0].response_revision_id.clone(),
    };
    let preview = store
        .preview_conversation_history_deletion(
            target.clone(),
            HistoryDeletionMode::WithDerivedCopies,
        )
        .await?;
    assert!(
        preview
            .dispatch_ids
            .contains(saved.dispatch_id.as_ref().unwrap())
    );
    store
        .confirm_conversation_history_deletion(
            target,
            HistoryDeletionMode::WithDerivedCopies,
            preview.revision,
        )
        .await?;
    let after = store
        .model_comparison_syntheses(repo.project_id.clone(), group.id.clone())
        .await?;
    assert_eq!(after[0].inputs, synthesis.inputs);
    assert!(after[0].response.is_none());
    assert!(
        store
            .start_model_comparison_synthesis(repo.project_id, selection(vec![second_run]), scope)
            .await
            .is_err()
    );
    Ok(())
}

async fn queue_plan_review_using_comparison(
    store: &Store,
    repo: &ProjectCheckout,
    scope: &SessionScope,
    comparison: crate::workflows::comparison::ComparisonSelection,
    environment: &AdmissionEnvironment,
) -> Result<(AttemptPlan, String)> {
    use crate::jobs::{JobRequest, PipelineDispatch};
    use crate::providers::planning::PlanSource;
    use crate::workflows::{
        ArtifactValue, DeliveryRoles, NodeInputs, WorkflowAgentSubmission, WorkflowHostRequest,
        WorkflowHostSubmission,
    };

    let project = &repo.project_id;
    let run = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let markdown = store
        .capture_repository_markdown(repo.checkout_id.clone(), scope.checkout.join("AGENTS.md"))
        .await?;
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let start = store
        .execute_workflow_host(host(
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start {
                document: markdown.revision,
            },
        ))
        .await?;
    let context = store
        .execute_workflow_host(host(
            "context",
            NodeInputs::from([("document".into(), start.id)]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    let submission =
        |node: &str, mode: SessionMode, plan_revision, artifacts| WorkflowAgentSubmission {
            project_id: project.clone(),
            dispatch: PipelineDispatch {
                pipeline_run_id: run.id.clone(),
                node_instance_id: node.into(),
                plan_revision,
                release_id: None,
            },
            scope: SessionScope {
                mode,
                profile_id: node.into(),
                ..scope.clone()
            },
            request: JobRequest {
                checkout_id: Some(repo.checkout_id.clone()),
                task_id: None,
                mode,
                instructions: "Review the scope".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
            input: Default::default(),
            artifacts,
            unit_id: None,
        };
    store
        .submit_workflow_agent(submission(
            "plan",
            SessionMode::Plan,
            None,
            NodeInputs::from([("context".into(), context.id)]),
        ))
        .await?;
    let planner = store
        .claim_next_job(environment.clone())
        .await?
        .context("Missing planner")?;
    store.mark_agent_running(planner.run.id.clone()).await?;
    store
        .record_agent_observation(
            planner.run.id.clone(),
            EventSource::Provider,
            ProviderEvent {
                data: ProviderEventData::PlanProposed {
                    text: "Review the defined change".into(),
                    source: PlanSource::CodexPlanItem,
                },
                location: ProviderLocation::default(),
            },
        )
        .await?;
    store
        .finish_agent_attempt(
            planner.run.id.clone(),
            RunStatus::Succeeded,
            Some("Plan ready".into()),
        )
        .await?;
    let event_sequence = store
        .agent_observations(planner.conversation_id, planner.run.id.clone(), 0)
        .await?
        .into_iter()
        .find(|event| matches!(event.event.data, ProviderEventData::PlanProposed { .. }))
        .context("Missing plan proposal")?
        .seq;
    let plan = store
        .publish_workflow_artifact(
            project.clone(),
            run.id.clone(),
            "plan".into(),
            "plan".into(),
            ArtifactValue::Plan {
                run_id: planner.run.id,
                event_sequence,
            },
        )
        .await?;
    store
        .set_job_pipeline_plan(project.clone(), run.id.clone(), None, plan.id.clone())
        .await?;
    let review = store
        .submit_workflow_agent_with_comparison(
            submission(
                "review-plan",
                SessionMode::ReadOnly,
                Some(plan.id.clone()),
                NodeInputs::from([("plan".into(), plan.id)]),
            ),
            Some(comparison),
        )
        .await?;
    let admitted = store
        .claim_next_job(environment.clone())
        .await?
        .context("Missing selected plan review")?;
    let linked = store
        .workflow_attempt(project.clone(), admitted.run.id.clone())
        .await?
        .context("Missing workflow linkage")?;
    assert_eq!(linked.job_id, review.id);
    Ok((admitted, review.id))
}

async fn finish_comparison_review(store: &Store, review: &AttemptPlan) -> Result<()> {
    let run_id = review.run.id.clone();
    store.mark_agent_running(run_id.clone()).await?;
    let dispatch = prepare_fixture_dispatch(store, review).await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    store
        .finish_agent_attempt(
            run_id,
            RunStatus::Succeeded,
            Some("Reviewed selected comparison sources".into()),
        )
        .await
}
