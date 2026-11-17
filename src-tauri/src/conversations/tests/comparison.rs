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
    assert!(comparison.source_document.is_none());

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
async fn saved_markdown_comparison_pins_the_archived_revision_and_submits_it_once_to_every_model()
-> Result<()> {
    use crate::{
        dispatch::DispatchSourceRevision, repository::documents::RepositoryDocumentOpen,
        workflows::comparison::ComparisonMarkdownSource,
    };

    let (_directory, store, repo, scope, _) = comparison_repository().await?;
    let storage = tempfile::tempdir()?;
    let git = |arguments: &[&str]| -> Result<()> {
        let run = Command::new("git.exe")
            .current_dir(&scope.checkout)
            .args(arguments)
            .output()?;
        ensure!(
            run.status.success(),
            "Fixture Git command failed: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        Ok(())
    };
    let path = "review.md";
    let note = scope.checkout.join(path);
    let tracked = "The base Git tree's version";
    let archived = "\u{feff}# Archived revisão\r\nThe exact saved note.\r\n";
    let current = "# Current file is different\n";
    let commit_note = |message: &str| -> Result<()> {
        git(&["add", "--", path])?;
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-qm",
            message,
        ])
    };
    std::fs::write(&note, tracked)?;
    commit_note("Baseline for the archived note")?;
    std::fs::write(&note, archived)?;
    let RepositoryDocumentOpen::Document { document: saved } = store
        .open_repository_document(repo.checkout_id.clone(), path.into())
        .await?
    else {
        anyhow::bail!("Note revision was not archived");
    };
    let base = saved
        .revision
        .base_commit
        .clone()
        .context("Archived base is missing")?;
    std::fs::write(&note, current)?;
    commit_note("Advance HEAD after retaining the note")?;
    assert_ne!(
        crate::repository::base_commit(&scope.checkout)?.as_deref(),
        Some(base.as_str())
    );
    let linked_path = storage.path().join("sibling worktree");
    git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        linked_path.to_str().context("Linked path")?,
    ])?;
    let linked = crate::repository::register(&store, linked_path).await?;
    assert_eq!(linked.project_id, repo.project_id);
    assert_ne!(linked.checkout_id, repo.checkout_id);

    let source = || ComparisonMarkdownSource {
        revision_id: saved.revision.id.clone(),
        relative_path: path.into(),
        sha256: saved.revision.sha256.clone(),
    };
    let prompt = "Compare the archived note's reasoning.";
    let mut second = scope.clone();
    second.model = Some("other-model".into());
    let profiles = || vec![(scope.clone(), None), (second.clone(), None)];
    let comparison_folder = storage.path().join("comparison checkouts");
    for (project, checkout, request) in [
        (
            "unrelated-project".into(),
            repo.checkout_id.clone(),
            source(),
        ),
        (repo.project_id.clone(), linked.checkout_id, source()),
        (
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            ComparisonMarkdownSource {
                relative_path: "other.md".into(),
                ..source()
            },
        ),
        (
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            ComparisonMarkdownSource {
                sha256: "f".repeat(64),
                ..source()
            },
        ),
    ] {
        assert!(
            store
                .start_model_comparison_from_markdown(
                    project,
                    checkout,
                    request,
                    prompt.into(),
                    profiles(),
                    comparison_folder.clone()
                )
                .await
                .is_err()
        );
    }
    assert!(
        store
            .model_comparisons(repo.project_id.clone())
            .await?
            .is_empty()
    );
    let replace_archived_body = |bytes: &'static [u8]| {
        let revision = saved.revision.clone();
        store.execute(move |db| {
            db.execute("UPDATE repository_markdown_revisions SET body = ?3 WHERE checkout_id = ?1 AND id = ?2",
                params![revision.checkout_id, revision.id, crate::protection::protect(bytes)?])?;
            Ok(())
        })
    };
    replace_archived_body(b"a different stored body").await?;
    let rejected = store
        .start_model_comparison_from_markdown(
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            source(),
            prompt.into(),
            profiles(),
            comparison_folder.clone(),
        )
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("does not match its checksum"));
    assert!(
        store
            .model_comparisons(repo.project_id.clone())
            .await?
            .is_empty()
    );
    replace_archived_body(archived.as_bytes()).await?;
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let group = store
        .start_model_comparison_from_markdown(
            repo.project_id.clone(),
            repo.checkout_id.clone(),
            source(),
            prompt.into(),
            profiles(),
            comparison_folder,
        )
        .await?;
    assert_eq!(group.commit, base);
    assert_eq!(group.prompt, prompt);
    assert_eq!(group.source_document.as_ref(), Some(&saved.revision));
    let output = serde_json::to_value(&group)?;
    assert_eq!(output["sourceDocument"]["relativePath"], path);
    assert_eq!(output["sourceDocument"]["sha256"], saved.revision.sha256);

    let environment = AdmissionEnvironment {
        runner: scope.runner.clone(),
        bridge_available: false,
    };
    let mut transmitted = Vec::new();
    let mut runs = Vec::new();
    for candidate in &group.candidates {
        let plan = store
            .claim_next_job(environment.clone())
            .await?
            .context("Candidate was not queued")?;
        let owner = plan.conversation_id.clone();
        let run = plan.run.id.clone();
        let pin = store
            .model_comparison_pin_for_run(run.clone())
            .await?
            .context("Missing comparison context")?;
        assert_eq!(pin.source_document.as_ref(), Some(&saved.revision));
        assert_eq!(pin.commit, base);
        assert_eq!(
            std::fs::read_to_string(pin.checkout()?.root.join(path))?,
            tracked
        );
        assert_eq!(plan.input[0].text, prompt);
        assert_eq!(
            plan.scope.model.as_deref(),
            Some(candidate.selection.model_id.as_str())
        );
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
        let saved_dispatch = store
            .agent_dispatch(owner.clone(), run.clone())
            .await?
            .context("Candidate was not submitted")?;
        assert_eq!(saved_dispatch.delivery, Delivery::Submitted);
        assert_eq!(
            saved_dispatch.snapshot.base_commit.as_deref(),
            Some(base.as_str())
        );
        assert_eq!(saved_dispatch.snapshot.submitted_messages[0].text, prompt);
        let document = saved_dispatch
            .snapshot
            .repository_markdown
            .context("Missing dispatch Markdown")?;
        assert_eq!(document.revision, saved.revision);
        assert_eq!(document.markdown, archived);
        let evidence: Value = serde_json::from_slice(&saved_dispatch.frames[0])?;
        let text = evidence["params"]["input"][0]["text"]
            .as_str()
            .context("Missing wire prompt")?;
        let (request, archive) = text
            .split_once("\n\nRepository Markdown source\n")
            .context("Missing archive in wire prompt")?;
        assert_eq!(request, prompt);
        assert_eq!(
            serde_json::from_str::<crate::repository::RepositoryMarkdown>(archive)?,
            document
        );
        transmitted.push(text.to_owned());
        let lineage = store
            .dispatch_lineage(owner, run.clone())
            .await?
            .context("Dispatch lineage was not saved")?;
        assert!(
            lineage
                .sources
                .contains(&DispatchSourceRevision::RepositoryMarkdown {
                    revision: saved.revision.clone()
                })
        );
        runs.push(run);
    }
    assert_eq!(transmitted.len(), 2);
    assert_eq!(transmitted[0], transmitted[1]);
    assert_eq!(std::fs::read_to_string(note)?, current);
    let database = scope.checkout.join("store.sqlite");
    drop(store);

    let reopened = Store::open(database)?;
    let groups = reopened.model_comparisons(repo.project_id.clone()).await?;
    assert_eq!(groups[0].source_document, group.source_document);
    let outcomes = reopened
        .model_comparison_outcomes(repo.project_id.clone(), group.id.clone())
        .await?;
    assert!(
        serde_json::to_value(outcomes)?
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["response"] == "Fixture answer")
    );
    let synthesis = reopened
        .start_model_comparison_synthesis(
            repo.project_id.clone(),
            crate::workflows::comparison::ComparisonSelection {
                group_id: group.id.clone(),
                run_ids: runs,
            },
            scope,
        )
        .await?;
    let plan = reopened
        .claim_next_job(environment)
        .await?
        .context("Synthesis was not queued")?;
    let claimed = reopened
        .model_comparison_syntheses(repo.project_id.clone(), group.id.clone())
        .await?;
    assert_eq!(claimed[0].job_id, synthesis.job_id);
    assert_eq!(claimed[0].run_id.as_deref(), Some(plan.run.id.as_str()));
    assert_eq!(
        reopened
            .dispatch_repository_markdown(plan.run.id)
            .await?
            .context("Synthesis lost the archive")?
            .markdown,
        archived
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

#[tokio::test]
async fn pinned_workflow_comparison_uses_candidate_slots_and_recovers_partial_results() -> Result<()>
{
    use crate::workflows::{
        ArtifactValue, BuiltinNode, Endpoint, ManualWorkflowLaunch, NodeConfiguration,
        NodeInstance, Wire, WorkflowGraph, WorkflowLaunchSource,
        comparison::{WorkflowComparisonConfiguration, stage::WorkflowComparisonOutcome},
    };

    let (_directory, store, repo, scope, commit) = comparison_repository().await?;
    let comparison_folder = tempfile::tempdir()?;
    let storage = comparison_folder.path().to_path_buf();
    let markdown = scope.checkout.join("brief.md");
    std::fs::write(&markdown, "Use this saved Markdown revision.")?;
    let source = store
        .capture_repository_markdown(repo.checkout_id.clone(), markdown.clone())
        .await?;
    std::fs::write(markdown, "A later edit must stay outside the request.")?;
    let mut second = scope.clone();
    second.model = Some("second-codex-model".into());
    let profiles = vec![scope.clone(), second];
    let selections = profiles
        .iter()
        .map(|profile| crate::model_catalog::ModelSelection {
            provider: profile.provider,
            profile_id: profile.profile_id.clone(),
            model_id: profile.model.clone().unwrap(),
        })
        .collect::<Vec<_>>();
    let node = |id: &str, kind, configuration| NodeInstance {
        id: id.into(),
        kind,
        configuration,
    };
    let wire = |from: &str, output: &str, to: &str, input: &str| Wire {
        from: Endpoint {
            node: from.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    };
    let workflow = store
        .save_workflow_revision(
            repo.project_id.clone(),
            "compare-stage".into(),
            WorkflowGraph {
                name: "Compare the saved request".into(),
                nodes: vec![
                    node("start", BuiltinNode::Start, NodeConfiguration::None),
                    node(
                        "text",
                        BuiltinNode::BuildText,
                        NodeConfiguration::BuildText(serde_json::from_value(
                            serde_json::json!({"template":"Compare the recorded source."}),
                        )?),
                    ),
                    node(
                        "compare",
                        BuiltinNode::CompareModels,
                        NodeConfiguration::CompareModels(WorkflowComparisonConfiguration {
                            selections: selections.clone(),
                        }),
                    ),
                ],
                wires: vec![
                    wire("start", "start", "text", "ready"),
                    wire("text", "completed", "compare", "ready"),
                    wire("text", "text", "compare", "request"),
                    wire("start", "document", "compare", "context"),
                ],
            },
        )
        .await?;
    let pipeline = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: repo.project_id.clone(),
            workflow,
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: source.revision.id,
            },
            request_id: "compare-stage-launch".into(),
            work_id: None,
        })
        .await?;
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let pending = store
        .next_workflow_comparison()
        .await?
        .context("Missing comparison activation")?;
    let first = pending
        .prepare_with_fixture_profiles(&store, storage.clone(), profiles.clone())
        .await?;
    let replay = pending
        .prepare_with_fixture_profiles(&store, storage, profiles)
        .await?;
    pending.clone().commit(&store, first).await?;
    pending.commit(&store, replay).await?;
    assert!(store.next_workflow_comparison().await?.is_none());
    let mut groups = store.model_comparisons(repo.project_id.clone()).await?;
    assert_eq!(groups.len(), 1);
    let group = groups.pop().unwrap();
    assert_eq!(group.commit, commit);
    assert!(group.prompt.contains("Use this saved Markdown revision."));
    assert!(!group.prompt.contains("A later edit"));
    let environment = AdmissionEnvironment {
        bridge_available: false,
        runner: scope.runner.clone(),
    };
    let candidate = store
        .claim_next_job(environment.clone())
        .await?
        .context("Missing first candidate")?;
    let first_run = candidate.run.id.clone();
    let runner = Arc::new(FixtureRunner(WireLog {
        store: store.clone(),
        runtime: tokio::runtime::Handle::current(),
        conversation: candidate.conversation_id.clone(),
        frames: Arc::default(),
    }));
    assert!(store.claim_next_job(environment.clone()).await?.is_none());
    assert_eq!(
        start_managed_turn(store.clone(), runner, candidate, None)
            .wait()
            .await?,
        RunStatus::Succeeded
    );
    store.refresh_workflow_comparisons().await?;
    let snapshot = store
        .inspect_workflow_execution(repo.project_id.clone(), pipeline.id.clone())
        .await?
        .unwrap();
    assert_eq!(snapshot.comparison_stages[0].state, "running");
    assert!(
        snapshot
            .outputs
            .iter()
            .all(|record| record.artifact.node_instance_id != "compare")
    );
    let candidate = store
        .claim_next_job(environment)
        .await?
        .context("Missing second candidate")?;
    store
        .finish_agent_attempt(candidate.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store.refresh_workflow_comparisons().await?;

    let database = scope.checkout.join("store.sqlite");
    drop(store);
    let reopened = Store::open(database)?;
    reopened.refresh_workflow_comparisons().await?;
    let snapshot = reopened
        .inspect_workflow_execution(repo.project_id.clone(), pipeline.id.clone())
        .await?
        .unwrap();
    assert_eq!(reopened.model_comparisons(repo.project_id).await?.len(), 1);
    assert_eq!(
        snapshot.comparison_stages[0].group_id.as_deref(),
        Some(group.id.as_str())
    );
    assert_eq!(snapshot.comparison_stages[0].state, "settled");
    assert_eq!(snapshot.jobs.len(), 2);
    assert!(snapshot.jobs.iter().all(|candidate| {
        candidate.node_id == "compare"
            && candidate
                .job
                .pipeline
                .as_ref()
                .is_some_and(|origin| origin.pipeline_run_id == pipeline.id)
    }));
    let output = snapshot
        .outputs
        .iter()
        .find(|record| record.artifact.node_instance_id == "compare")
        .context("Missing comparison evidence")?;
    let ArtifactValue::ComparisonResults(results) = &output.artifact.value else {
        anyhow::bail!("The comparison has the wrong output kind");
    };
    assert_eq!(results.state, WorkflowComparisonOutcome::Partial);
    assert_eq!(results.candidates[0].selection, selections[0]);
    assert_eq!(results.candidates[1].selection, selections[1]);
    assert_eq!(results.successful_selection().run_ids, vec![first_run]);
    assert!(!output.input_receipts.is_empty());
    Ok(())
}
