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
    jobs::{AdmissionEnvironment, JobScheduler, QueueSettings},
    providers::{
        Provider, SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
    workflows::comparison::ModelComparison,
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
#[ignore = "requires an installed authenticated Codex profile with two available models"]
async fn authenticated_codex_comparison_reads_pinned_bytes_for_both_models() -> Result<()> {
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
    let models = [
        std::env::var("OMNIAGENT_COMPARISON_SMOKE_FIRST_MODEL")
            .unwrap_or_else(|_| "gpt-6-luna".into()),
        std::env::var("OMNIAGENT_COMPARISON_SMOKE_SECOND_MODEL")
            .unwrap_or_else(|_| "gpt-6-sol".into()),
    ];
    ensure!(
        models[0] != models[1],
        "Choose two distinct models for the comparison smoke"
    );
    let profile = desktop_profile(configured, None)?;
    let scopes = models
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
    let profile_id = scopes[0].profile_id.clone();
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
        let mut saw_second_waiting = false;
        loop {
            let current = comparison(&store, &project.project_id, &started.id).await?;
            if let Some(candidate) = current.candidates.iter().find(|candidate| matches!(candidate.state.as_str(), "failed" | "interrupted" | "cancelled")) {
                let failure = match &candidate.run_id {
                    Some(run) => store.job_failure(project.project_id.clone(), run.clone()).await?,
                    None => None,
                };
                anyhow::bail!("Comparison candidate {} settled as {} (job {}, run {:?}, dispatch {:?}, failure {:?})",
                    candidate.selection.model_id, candidate.state, candidate.job_id, candidate.run_id, candidate.dispatch_id,
                    failure.map(|item| (item.observation, item.dispatch_rejection)));
            }
            let [first, second] = current.candidates.as_slice() else { anyhow::bail!("Expected two comparison candidates") };
            if first.run_id.is_some() && second.run_id.is_none() {
                ensure!(second.state == "queued" && second.dispatch_id.is_none(), "Waiting candidate unexpectedly advanced");
                saw_second_waiting = true;
            }
            ensure!(second.run_id.is_none() || first.state == "succeeded", "The second candidate started before the first succeeded");
            if first.state == "succeeded" && second.state == "succeeded" {
                ensure!(saw_second_waiting, "The second candidate was not observed queued while the first owned the only slot");
                return Ok::<_, anyhow::Error>(current);
            }
            let queue = store.inspect_project_queue(project.project_id.clone(), environment.clone()).await?;
            ensure!(queue.settings.capacity == 1 && queue.active <= 1, "The comparison exceeded queue capacity one");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;
    scheduler.shutdown().await?;
    let completed =
        completed.context("Timed out waiting for both live comparison candidates")??;
    drop(store);

    let store = Store::open(database)?;
    let reopened = comparison(&store, &project.project_id, &started.id).await?;
    assert_eq!(reopened.commit, commit);
    assert_eq!(reopened.tree, tree);
    assert_eq!(reopened.prompt, prompt);
    let mut run_ids = HashSet::new();
    let mut dispatch_ids = HashSet::new();
    let mut working_directories = HashSet::new();
    for (index, candidate) in reopened.candidates.iter().enumerate() {
        let model = &models[index];
        assert_eq!(candidate.state, "succeeded");
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
        assert!(run_ids.insert(run_id));
        assert!(dispatch_ids.insert(evidence.id));
        working_directories.insert(cwd.clone());
    }
    assert_eq!(working_directories.len(), 1);
    assert_eq!(std::fs::read_to_string(input)?, dirty);
    Ok(())
}
