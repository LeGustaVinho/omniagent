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

#[tokio::test]
async fn comparison_preserves_output_order_and_success_after_another_candidate_times_out()
-> Result<()> {
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
            repo.checkout_id,
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
    let third = store.claim_next_job(environment).await?.unwrap();
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
            .model_comparison_outcomes(repo.project_id, group.id)
            .await?,
    )?;
    for (index, run) in [first_run, second_run, third_run].into_iter().enumerate() {
        assert_eq!(outcomes[index]["runId"], run);
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
    Ok(())
}
