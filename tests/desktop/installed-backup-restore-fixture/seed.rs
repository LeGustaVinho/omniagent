use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::restore;

const TITLE: &str = "Older task for installed backup restore";
const PROMPT: &str = "Recover this exact protected implementation prompt from the checkpoint.";
const CHANGE: &str = "Historic managed diff from before the backup.\n";
const NOTE: &str = "# Older delivery brief\n\n[[Review target#Decision|Historical decision]]\n";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeedReceipt {
    checkout: ProjectCheckout,
    task_id: String,
    task_title: String,
    conversation_id: String,
    run_id: String,
    prompt_sha256: String,
    prompt_marker: &'static str,
    capture_revision: String,
    diff_sha256: String,
    diff_marker: &'static str,
    worktree: PathBuf,
    source_revision: String,
    source_markdown: &'static str,
    question_timeout: u32,
}

#[tokio::test]
#[ignore = "explicit installed backup/restore sandbox only"]
async fn seed_installed_backup_restore() -> Result<()> {
    let case = dunce::canonicalize(PathBuf::from(
        std::env::var_os("OMNIAGENT_RESTORE_TEST_CASE")
            .context("Missing the installed restore case")?,
    ))?;
    let label = case
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("omniagent-phase09-nsis-"))
        .filter(|label| label.len() == 10 && label.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("Expected the case directory of the NSIS fixture")?;
    let identifier = format!("io.github.legustavinho.omniagent.nsis-test-{label}");
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("Missing Windows profile")?);
    let profile = local.join(&identifier);
    let case_config: serde_json::Value =
        serde_json::from_slice(&fs::read(case.join("case.json"))?)?;
    ensure!(
        case_config["identifier"] == identifier,
        "Wrong NSIS case profile"
    );
    ensure!(
        dunce::canonicalize(&profile)? == dunce::canonicalize(&local)?.join(&identifier),
        "The fixture profile is redirected"
    );
    ensure!(
        profile.join("installation.dpapi").is_file(),
        "Launch the disposable installed app before seeding its profile"
    );
    ensure!(
        profile.join("omniagent.sqlite3").is_file(),
        "The installed profile did not create its database"
    );
    let installed = restore::open_installation(&profile)?;
    ensure!(
        installed.restore.active.is_none()
            && installed.restore.pending.is_none()
            && installed.restore.rejection.is_none(),
        "The case has already attempted a restore"
    );
    let fixture_root = case.join("backup-restore-repository");
    fs::create_dir(&fixture_root)
        .context("The fixture must start with a fresh repository directory")?;
    let store = Store::open(installed.layout.database_path())?;
    let checkout = approved_delivery_repository(&store, &fixture_root).await?;
    let project = checkout.project_id.clone();
    let repo = Path::new(&checkout.checkout_root);
    fs::write(repo.join("brief.md"), NOTE)?;
    fs::write(
        repo.join("Review target.md"),
        "# Decision\n\nBaseline decision.\n",
    )?;
    git(repo, &["add", "--", "brief.md", "Review target.md"])?;
    git(
        repo,
        &[
            "-c",
            "user.name=Restore Fixture",
            "-c",
            "user.email=restore@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Record historical note links",
        ],
    )?;

    let task = store
        .create_project_task(project.clone(), TITLE.into())
        .await?;
    let fixture = approve_delivery_fixture_for_work(
        &store,
        &fixture_root,
        checkout.clone(),
        Some(task.id.clone()),
    )
    .await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("Expected one delivery unit")
    };
    let worktree = store
        .live_managed_worktree(project.clone(), task.id.clone())
        .await?
        .context("Missing fixture managed worktree")?
        .worktree;
    let markdown = store
        .workflow_run_artifacts(project.clone(), fixture.run.id.clone())
        .await?
        .into_iter()
        .find_map(|artifact| match artifact.value {
            ArtifactValue::Markdown(revision) if artifact.node_instance_id == "start" => {
                Some(revision)
            }
            _ => None,
        })
        .context("The workflow did not pin its historical Markdown")?;
    let historical = store
        .repository_markdown(checkout.checkout_id.clone(), markdown.id.clone())
        .await?
        .context("Missing protected Markdown")?;
    assert_eq!(historical.markdown, NOTE);

    let job = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let attempt = claim_fixture_job(&store, job, fixture_admission()).await?;
    let run = attempt.run.id.clone();
    store.mark_agent_running(run.clone()).await?;
    ensure!(repository::capture_worktree_start(&store, run.clone()).await?);
    let context = store
        .task_dispatch_context(run.clone())
        .await?
        .context("Missing exact task scope")?;
    let mut prompt = PROMPT.to_owned();
    context.append_to_prompt(&mut prompt)?;
    historical.append_to_prompt(&mut prompt)?;
    let frame = serde_json::to_vec(&json!({"prompt": prompt}))?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": attempt.context_id, "scope": attempt.scope,
        "submittedMessages": attempt.input, "baseCommit": worktree.base_commit,
        "nativeHistory": {"mode": "fresh"}, "taskContext": context
    }))?;
    let dispatch = store
        .prepare_dispatch(run.clone(), snapshot, vec![frame.clone()])
        .await?;
    store.settle_dispatch(dispatch, Delivery::Submitted).await?;
    fs::write(worktree.root.join("first.txt"), CHANGE)?;
    repository::save_worktree_candidate(&store, run.clone()).await?;
    store
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), run.clone())
        .await?
    else {
        anyhow::bail!("Missing managed diff evidence")
    };
    let changes = artifact
        .candidate
        .changes
        .context("The writer did not record its diff")?;
    let patch = STANDARD.decode(changes.unstaged_patch_base64)?;
    assert!(String::from_utf8_lossy(&patch).contains(CHANGE.trim_end()));

    let question_timeout = 47;
    store
        .set_project_question_timeout(project, question_timeout)
        .await?;
    store
        .select_active_checkout(checkout.checkout_id.clone())
        .await?;
    store
        .rebuild_repository_note_index(checkout.checkout_id.clone())
        .await?;
    let receipt = SeedReceipt {
        checkout,
        task_id: task.id,
        task_title: TITLE.into(),
        conversation_id: attempt.conversation_id,
        run_id: run,
        prompt_sha256: digest(&frame),
        prompt_marker: PROMPT,
        capture_revision: artifact.revision,
        diff_sha256: digest(&patch),
        diff_marker: CHANGE.trim_end(),
        worktree: worktree.root,
        source_revision: markdown.id,
        source_markdown: NOTE,
        question_timeout,
    };
    fs::write(
        case.join("backup-restore-seed.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
