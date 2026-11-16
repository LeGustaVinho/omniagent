use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{MessageRole, SessionScope},
    dispatch::{Delivery, NativeHistory},
    jobs::{AdmissionEnvironment, DispatchRejection, JobScheduler, QueueSettings},
    providers::{
        Provider, SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
    workflows::comparison::{ComparisonSelection, ModelComparison},
};
use serde_json::Value;

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git.exe")
        .current_dir(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn comparison(store: &Store, project: &str, id: &str) -> Result<ModelComparison> {
    store
        .model_comparisons(project.to_owned())
        .await?
        .into_iter()
        .find(|comparison| comparison.id == id)
        .context("Missing comparison")
}

#[tokio::test]
#[ignore = "requires an installed authenticated Codex profile with three available models"]
async fn authenticated_codex_comparison_preserves_partial_results_and_synthesis() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source");
    std::fs::create_dir(&source)?;
    git(&source, &["init", "--quiet", "--template="])?;
    let relative_input = "compare-input.txt";
    let input = source.join(relative_input);
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let pinned = format!("PINNED-{nonce}\n");
    let dirty = format!("DIRTY-{}\n", uuid::Uuid::new_v4().simple());
    std::fs::write(&input, &pinned)?;
    git(&source, &["add", "--", relative_input])?;
    git(
        &source,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=NUL",
            "commit",
            "-qm",
            "Pinned comparison fixture",
        ],
    )?;
    let commit = git(&source, &["rev-parse", "HEAD"])?;
    let tree = git(&source, &["rev-parse", "HEAD^{tree}"])?;
    std::fs::write(&input, &dirty)?;

    let database = directory.path().join("comparison.sqlite");
    let store = Store::open(database.clone())?;
    let project = repository::register(&store, source).await?;
    let configured = store.configured_executable(Provider::Codex).await?;
    let first = std::env::var("OMNIAGENT_COMPARISON_SMOKE_FIRST_MODEL")
        .unwrap_or_else(|_| "gpt-6-luna".into());
    let last = std::env::var("OMNIAGENT_COMPARISON_SMOKE_SECOND_MODEL")
        .unwrap_or_else(|_| "gpt-6-sol".into());
    ensure!(
        first != last,
        "Choose two distinct models for the comparison smoke"
    );
    let profile = desktop_profile(configured, None)?;
    let rejected = profile
        .models
        .iter()
        .find(|entry| {
            let candidate = &entry.option.selection.model_id;
            entry.option.selectable && candidate != &first && candidate != &last
        })
        .context(
            "The installed profile does not offer a third distinct model for the isolated failure",
        )?;
    let models = [first, rejected.option.selection.model_id.clone(), last];
    let mut scopes = models
        .iter()
        .map(|model| {
            let option = profile
                .models
                .iter()
                .find(|entry| entry.option.selectable && entry.option.selection.model_id == *model)
                .with_context(|| {
                    format!("The installed authenticated profile does not offer {model}")
                })?;
            Ok(SessionScope {
                provider: Provider::Codex,
                profile_id: option.option.selection.profile_id.clone(),
                account_binding: profile.account_binding.clone(),
                executable: profile.executable.clone(),
                checkout: PathBuf::from(&project.checkout_root),
                cli_version: profile.version.clone(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: DESKTOP_PERMISSION_REVISION.into(),
                mode: SessionMode::ReadOnly,
                model: Some(model.clone()),
                effort: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(scopes[0].profile_id, scopes[1].profile_id);
    assert_eq!(scopes[0].profile_id, scopes[2].profile_id);
    let profile_id = scopes[0].profile_id.clone();
    let synthesis_scope = scopes[0].clone();
    // Simulate a stale saved build identity for just one job in the disposable store.
    // The real runtime checks the installed version and rejects this scope before dispatch.
    scopes[1].cli_version = "0.0.0".into();
    let prompt = "Read compare-input.txt in the current working directory. In your final response, return the exact line you read from that file. Do not edit any file.";
    store
        .set_job_queue_settings(QueueSettings {
            paused: false,
            capacity: 1,
        })
        .await?;
    let started = store
        .start_model_comparison(
            project.project_id.clone(),
            project.checkout_id.clone(),
            commit.clone(),
            prompt.into(),
            scopes,
            directory.path().join("comparisons"),
        )
        .await?;
    assert_eq!(started.commit, commit);
    assert_eq!(started.tree, tree);
    assert_eq!(started.candidates.len(), models.len());
    assert!(
        started
            .candidates
            .iter()
            .all(|candidate| candidate.state == "queued"
                && candidate.run_id.is_none()
                && candidate.dispatch_id.is_none())
    );

    let environment = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: false,
    };
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let completed = tokio::time::timeout(Duration::from_secs(180), async {
        let mut saw_queued_candidates = false;
        loop {
            let current = comparison(&store, &project.project_id, &started.id).await?;
            if let Some((_, candidate)) = current.candidates.iter().enumerate().find(|(position, candidate)| {
                matches!(candidate.state.as_str(), "interrupted" | "cancelled") || (*position != 1 && candidate.state == "failed")
            }) {
                let failure = match &candidate.run_id {
                    Some(run) => store.job_failure(project.project_id.clone(), run.clone()).await?,
                    None => None,
                };
                anyhow::bail!("Comparison candidate {} settled as {} (job {}, run {:?}, dispatch {:?}, failure {:?})",
                    candidate.selection.model_id, candidate.state, candidate.job_id, candidate.run_id, candidate.dispatch_id,
                    failure.map(|item| (item.observation, item.dispatch_rejection)));
            }
            let [first, rejected, last] = current.candidates.as_slice() else { anyhow::bail!("Expected three comparison candidates") };
            if first.run_id.is_some() && rejected.run_id.is_none() && last.run_id.is_none() {
                ensure!(rejected.state == "queued" && rejected.dispatch_id.is_none() && last.state == "queued" && last.dispatch_id.is_none(),
                    "Waiting candidates unexpectedly advanced");
                saw_queued_candidates = true;
            }
            ensure!(rejected.run_id.is_none() || first.state == "succeeded", "The second candidate started before the first succeeded");
            ensure!(last.run_id.is_none() || rejected.state == "failed", "The final candidate started before the rejected candidate settled");
            ensure!(rejected.state != "succeeded", "The stale CLI build unexpectedly passed revalidation");
            if last.state == "succeeded" {
                ensure!(saw_queued_candidates, "The last two candidates were not observed queued while the first held the only slot");
                return Ok::<_, anyhow::Error>(current);
            }
            let queue = store.inspect_project_queue(project.project_id.clone(), environment.clone()).await?;
            ensure!(queue.settings.capacity == 1 && queue.active <= 1, "The comparison exceeded queue capacity one");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;
    scheduler.shutdown().await?;
    let completed = completed.context("Timed out waiting for the live comparison group")??;
    drop(store);

    let store = Store::open(database.clone())?;
    let reopened = comparison(&store, &project.project_id, &started.id).await?;
    assert_eq!(reopened.commit, commit);
    assert_eq!(reopened.tree, tree);
    assert_eq!(reopened.prompt, prompt);
    let mut run_ids = HashSet::new();
    let mut dispatch_ids = HashSet::new();
    let mut working_directories = HashSet::new();
    let mut successful_inputs = Vec::new();
    let outcomes = serde_json::to_value(
        store
            .model_comparison_outcomes(project.project_id.clone(), started.id.clone())
            .await?,
    )?;
    assert_eq!(
        outcomes
            .as_array()
            .context("Expected terminal comparison outcomes")?
            .len(),
        models.len()
    );
    for (index, candidate) in reopened.candidates.iter().enumerate() {
        let model = &models[index];
        assert_eq!(
            candidate.state,
            if index == 1 { "failed" } else { "succeeded" }
        );
        assert_eq!(candidate.job_id, started.candidates[index].job_id);
        assert_eq!(candidate.selection.provider, Provider::Codex);
        assert_eq!(candidate.selection.profile_id, profile_id);
        assert_eq!(&candidate.selection.model_id, model);
        assert_eq!(candidate.run_id, completed.candidates[index].run_id);
        assert_eq!(
            candidate.dispatch_id,
            completed.candidates[index].dispatch_id
        );
        let run_id = candidate
            .run_id
            .clone()
            .context("Missing candidate run ID")?;
        let chat = candidate
            .conversation_id
            .clone()
            .context("Missing candidate conversation link")?;
        let job = store
            .job(project.project_id.clone(), candidate.job_id.clone())
            .await?
            .context("Missing candidate job")?;
        let attempt = job.latest_attempt.context("Missing linked job attempt")?;
        assert_eq!(attempt.ordinal, 1);
        assert_eq!(attempt.run_id, run_id);
        assert_eq!(attempt.conversation_id.as_deref(), Some(chat.as_str()));
        assert_eq!(outcomes[index]["runId"], run_id);
        assert!(run_ids.insert(run_id.clone()));
        if index == 1 {
            assert!(candidate.dispatch_id.is_none());
            assert!(outcomes[index]["response"].is_null());
            assert!(store.agent_dispatch(chat, run_id.clone()).await?.is_none());
            let failure = store
                .job_failure(project.project_id.clone(), run_id)
                .await?
                .context("Missing isolated candidate failure evidence")?;
            assert_eq!(
                failure.dispatch_rejection,
                Some(DispatchRejection::ProfileUnavailable)
            );
            continue;
        }
        let dispatch_id = candidate
            .dispatch_id
            .as_deref()
            .context("Missing candidate dispatch link")?;
        let evidence = store
            .agent_dispatch(chat.clone(), run_id.clone())
            .await?
            .context("Missing candidate dispatch evidence")?;
        assert_eq!(evidence.id, dispatch_id);
        assert_eq!(evidence.delivery, Delivery::Submitted);
        assert!(matches!(
            evidence.snapshot.native_history,
            NativeHistory::Fresh
        ));
        assert_eq!(evidence.snapshot.scope.mode, SessionMode::ReadOnly);
        assert_eq!(evidence.snapshot.scope.model.as_ref(), Some(model));
        assert_eq!(evidence.snapshot.base_commit.as_ref(), Some(&commit));
        let capability_scope = &evidence
            .snapshot
            .capabilities
            .as_ref()
            .context("Missing provider-reported comparison profile")?
            .scope;
        assert_eq!(capability_scope.reported_model.as_ref(), Some(model));
        assert!(
            matches!(evidence.snapshot.submitted_messages.as_slice(), [message] if message.text == prompt)
        );
        let [frame] = evidence.frames.as_slice() else {
            anyhow::bail!("Expected one exact Codex dispatch frame")
        };
        let frame: Value = serde_json::from_slice(frame)?;
        assert_eq!(frame["method"], "turn/start");
        assert_eq!(frame["params"]["input"][0]["text"], prompt);
        let launch = evidence
            .snapshot
            .scoped_launch
            .as_ref()
            .context("Missing scoped comparison launch evidence")?;
        let context = launch
            .inputs
            .iter()
            .find(|input| input.name == "Comparison context")
            .context("Missing comparison context pin")?;
        let context: Value = serde_json::from_str(&context.contents)?;
        assert_eq!(context["groupId"], started.id);
        assert_eq!(context["commit"], commit);
        assert_eq!(context["tree"], tree);
        let cwd = &evidence
            .snapshot
            .instructions
            .as_ref()
            .context("Missing launch working directory")?
            .working_directory;
        assert_ne!(cwd, Path::new(&project.checkout_root));
        assert_eq!(std::fs::read_to_string(cwd.join(relative_input))?, pinned);
        let response = store
            .conversation_history(chat)
            .await?
            .messages
            .into_iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .last()
            .context("Missing original assistant response")?;
        assert!(
            response.text.contains(pinned.trim()),
            "{model} response did not include the committed bytes"
        );
        assert!(
            !response.text.contains(dirty.trim()),
            "{model} response included dirty source bytes"
        );
        assert_eq!(outcomes[index]["response"], response.text);
        successful_inputs.push((run_id, evidence.id.clone(), response.revision_id));
        assert!(dispatch_ids.insert(evidence.id));
        working_directories.insert(cwd.clone());
    }
    assert_eq!(working_directories.len(), 1);
    assert_eq!(std::fs::read_to_string(&input)?, dirty);

    let synthesis = store
        .start_model_comparison_synthesis(
            project.project_id.clone(),
            ComparisonSelection {
                group_id: started.id.clone(),
                run_ids: successful_inputs
                    .iter()
                    .rev()
                    .map(|(run_id, _, _)| run_id.clone())
                    .collect(),
            },
            synthesis_scope,
        )
        .await?;
    assert_eq!(synthesis.state, "queued");
    assert!(synthesis.run_id.is_none());
    assert_eq!(
        synthesis
            .inputs
            .iter()
            .map(|source| (
                &source.run_id,
                &source.dispatch_id,
                &source.response_revision_id
            ))
            .collect::<Vec<_>>(),
        successful_inputs
            .iter()
            .map(|(run, dispatch, revision)| (run, dispatch, revision))
            .collect::<Vec<_>>()
    );
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let completed_synthesis = tokio::time::timeout(Duration::from_secs(110), async {
        loop {
            let result = store
                .model_comparison_syntheses(project.project_id.clone(), started.id.clone())
                .await?
                .into_iter()
                .find(|row| row.id == synthesis.id)
                .context("Missing linked synthesis")?;
            match result.state.as_str() {
                "succeeded" => return Ok::<_, anyhow::Error>(result),
                "failed" | "interrupted" | "cancelled" => anyhow::bail!(
                    "Synthesis ended as {} (run {:?})",
                    result.state,
                    result.run_id
                ),
                _ => tokio::time::sleep(Duration::from_millis(150)).await,
            }
        }
    })
    .await;
    scheduler.shutdown().await?;
    let completed_synthesis =
        completed_synthesis.context("Timed out waiting for the live synthesis")??;
    drop(store);

    let store = Store::open(database)?;
    let saved = store
        .model_comparison_syntheses(project.project_id.clone(), started.id.clone())
        .await?
        .into_iter()
        .find(|row| row.id == synthesis.id)
        .context("Missing synthesis after reopen")?;
    assert_eq!(saved.inputs, synthesis.inputs);
    assert_eq!(saved.run_id, completed_synthesis.run_id);
    assert_eq!(saved.dispatch_id, completed_synthesis.dispatch_id);
    assert!(
        saved
            .response
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    );
    let run = saved.run_id.context("Missing synthesis run link")?;
    let chat = saved
        .conversation_id
        .context("Missing synthesis conversation link")?;
    let dispatch = store
        .agent_dispatch(chat, run.clone())
        .await?
        .context("Missing synthesis dispatch")?;
    assert!(!run_ids.contains(&run));
    assert!(!dispatch_ids.contains(&dispatch.id));
    assert_eq!(Some(&dispatch.id), saved.dispatch_id.as_ref());
    assert_eq!(dispatch.delivery, Delivery::Submitted);
    assert_eq!(dispatch.snapshot.scope.mode, SessionMode::ReadOnly);
    assert_eq!(dispatch.snapshot.base_commit.as_ref(), Some(&commit));
    let cwd = &dispatch
        .snapshot
        .instructions
        .as_ref()
        .context("Missing synthesis execution directory")?
        .working_directory;
    assert!(working_directories.contains(cwd));
    let [frame] = dispatch.frames.as_slice() else {
        anyhow::bail!("Expected one exact synthesis dispatch frame")
    };
    let frame: Value = serde_json::from_slice(frame)?;
    assert_eq!(frame["method"], "turn/start");
    let text = frame["params"]["input"][0]["text"]
        .as_str()
        .context("Missing synthesis input")?;
    let (_, context) = text
        .split_once("\n\n")
        .context("Missing selected response context")?;
    let context: Value = serde_json::from_str(context)?;
    assert_eq!(context["groupId"], started.id);
    assert_eq!(context["commit"], commit);
    let selected = context["responses"]
        .as_array()
        .context("Missing responses in synthesis input")?;
    assert_eq!(selected.len(), successful_inputs.len());
    for (envelope, (run_id, _, revision)) in selected.iter().zip(&successful_inputs) {
        assert_eq!(envelope["run"]["runId"], run_id.as_str());
        assert_eq!(envelope["run"]["outputRevisionId"], revision.as_str());
        assert!(
            envelope["response"].as_str().is_some_and(
                |answer| answer.contains(pinned.trim()) && !answer.contains(dirty.trim())
            )
        );
    }
    let finished = comparison(&store, &project.project_id, &started.id).await?;
    assert_eq!(
        finished
            .candidates
            .iter()
            .map(|item| (&item.run_id, &item.state))
            .collect::<Vec<_>>(),
        completed
            .candidates
            .iter()
            .map(|item| (&item.run_id, &item.state))
            .collect::<Vec<_>>()
    );
    assert_eq!(std::fs::read_to_string(input)?, dirty);
    Ok(())
}
