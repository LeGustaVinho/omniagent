use std::{
    ffi::OsStr,
    fs, io,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::SessionScope,
    jobs::{AdmissionBlocker, AdmissionEnvironment, JobRequest},
    process_io::CapturedProcess,
    providers::{Provider, SessionMode, runtime::start_managed_turn},
    repository::{self, ProjectCheckout, provision_task_worktree},
    runner::{LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};

fn git(directory: &Path, args: &[impl AsRef<OsStr>]) -> Result<String> {
    let output = Command::new("git.exe")
        .current_dir(directory)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git operation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

async fn repository(store: &Store, path: &Path) -> Result<ProjectCheckout> {
    fs::create_dir(path)?;
    git(path, &["init", "--quiet", "--template="])?;
    git(path, &["config", "core.autocrlf", "false"])?;
    fs::write(path.join(".gitattributes"), "*.sample filter=fixture\n")?;
    fs::write(path.join("file.sample"), "Pinned content\n")?;
    git(path, &["add", "--", ".gitattributes", "file.sample"])?;
    git(
        path,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=NUL",
            "commit",
            "--quiet",
            "-m",
            "Initial",
        ],
    )?;
    repository::register(store, path.into()).await
}

#[tokio::test]
async fn provisions_one_worktree_per_task_with_a_project_override_and_executable_git_hooks_disabled()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("store.sqlite"))?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let base = git(&source, &["rev-parse", "HEAD"])?;
    let main_branch = git(&source, &["branch", "--show-current"])?;
    let hooks = source.join(".git/hooks");
    fs::create_dir_all(&hooks)?;
    fs::write(
        hooks.join("reference-transaction"),
        b"#!/bin/sh\necho unexpected > hook-ran\n",
    )?;
    git(
        &source,
        &[
            "config",
            "core.hooksPath",
            hooks.to_str().context("non-Unicode hooks path")?,
        ],
    )?;
    for kind in ["smudge", "clean"] {
        let marker = temp.path().join(format!("{kind}-ran"));
        let marker = marker
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''");
        git(
            &source,
            &[
                "config",
                &format!("filter.fixture.{kind}"),
                &format!("echo unexpected > '{marker}'; cat"),
            ],
        )?;
    }
    git(&source, &["config", "filter.fixture.required", "true"])?;
    git(&source, &["branch", "fixture-hook-control"])?;
    git(&source, &["cat-file", "--filters", "HEAD:file.sample"])?;
    git(
        &source,
        &["hash-object", "--path=file.sample", "file.sample"],
    )?;
    assert!(source.join("hook-ran").is_file());
    fs::remove_file(source.join("hook-ran"))?;
    for marker in ["smudge-ran", "clean-ran"] {
        assert!(
            temp.path().join(marker).is_file(),
            "Git did not run the fixture's {marker} control"
        );
        fs::remove_file(temp.path().join(marker))?;
    }

    let task = store
        .create_project_task(project.project_id.clone(), "Implement".into())
        .await?;
    let default = temp.path().join("local-data/worktrees");
    let provision = || {
        provision_task_worktree(
            &store,
            default.clone(),
            project.project_id.clone(),
            task.id.clone(),
            project.checkout_id.clone(),
        )
    };
    let (first, concurrent) = tokio::join!(provision(), provision());
    let first = first?;
    assert_eq!(first, concurrent?);
    assert_ne!(first.checkout_id, project.checkout_id);
    assert_eq!(first.owner_task_id, task.id);
    assert_eq!(first.base_commit, base);
    assert!(first.root.starts_with(dunce::canonicalize(&default)?));
    assert_eq!(
        git(&first.root, &["branch", "--show-current"])?,
        first.branch
    );
    assert_eq!(git(&first.root, &["rev-parse", "HEAD"])?, base);
    assert_eq!(git(&source, &["branch", "--show-current"])?, main_branch);
    assert_eq!(
        fs::read_to_string(first.root.join("file.sample"))?,
        "Pinned content\n"
    );

    let override_root = temp.path().join("override");
    store
        .set_project_worktree_root(project.project_id.clone(), Some(override_root.clone()))
        .await?;
    git(&source, &["config", "core.autocrlf", "true"])?;
    let next_task = store
        .create_project_task(project.project_id.clone(), "Second".into())
        .await?;
    let second = provision_task_worktree(
        &store,
        default,
        project.project_id,
        next_task.id,
        project.checkout_id,
    )
    .await?;
    assert!(second.root.starts_with(dunce::canonicalize(override_root)?));
    assert_ne!(first.root, second.root);
    assert_ne!(first.branch, second.branch);
    assert_eq!(
        fs::read_to_string(second.root.join("file.sample"))?,
        "Pinned content\r\n"
    );
    for directory in [&source, &first.root, &second.root] {
        assert!(!directory.join("hook-ran").exists());
    }
    for marker in ["smudge-ran", "clean-ran"] {
        assert!(!temp.path().join(marker).exists());
    }
    Ok(())
}

#[tokio::test]
async fn resumes_a_pinned_provision_across_restart_and_refuses_a_replaced_managed_branch()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Restart".into())
        .await?;
    let root = temp
        .path()
        .join("worktrees")
        .join(&project.project_id)
        .join(&task.id);
    fs::create_dir_all(root.parent().context("missing fixture directory")?)?;
    let root = dunce::canonicalize(root.parent().unwrap())?.join(&task.id);
    let branch = format!("codex/task-{}", task.id);
    let base = git(&source, &["rev-parse", "HEAD"])?;
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO worktree_provisions VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![project.project_id, task.id, root.to_str(), branch, base],
    )?;
    git(
        &source,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("-b"),
            OsStr::new(&branch),
            root.as_os_str(),
            OsStr::new(&base),
        ],
    )?;
    assert!(!root.join("file.sample").exists());
    drop(store);
    let reopened = Store::open(database)?;
    let attach = || {
        provision_task_worktree(
            &reopened,
            temp.path().join("different-default"),
            project.project_id.clone(),
            task.id.clone(),
            project.checkout_id.clone(),
        )
    };
    let managed = attach().await?;
    assert_eq!(managed.root, root);
    assert_eq!(managed.base_commit, base);
    assert_eq!(
        fs::read_to_string(root.join("file.sample"))?,
        "Pinned content\n"
    );
    git(
        &root,
        &["-c", "core.hooksPath=NUL", "switch", "--quiet", "--detach"],
    )?;
    assert!(attach().await.is_err());
    Ok(())
}

struct RejectAfterCapture(AtomicBool);

impl Runner for RejectAfterCapture {
    fn identity(&self) -> &str {
        "worktree-start-fixture"
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request
            .arguments
            .iter()
            .any(|argument| *argument == OsStr::new("app-server"))
        {
            self.0.store(true, Ordering::Release);
        }
        Err(io::Error::other("fixture does not execute a provider"))
    }
}

#[tokio::test]
async fn scheduled_writer_records_worktree_identity_and_git_state_before_launch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Writer".into())
        .await?;
    let managed = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    fs::write(managed.root.join("staged.txt"), b"Staged content\n")?;
    git(&managed.root, &["add", "--", "staged.txt"])?;
    fs::write(managed.root.join("file.sample"), b"Edited content\n")?;
    fs::write(
        managed.root.join("untracked-private-marker.txt"),
        b"Existing file\n",
    )?;
    git(
        &source,
        &[
            "config",
            "filter.fixture.clean",
            "echo unexpected > clean-ran; cat",
        ],
    )?;
    git(&source, &["config", "filter.fixture.required", "true"])?;

    let conversation = store
        .create_conversation(project.checkout_id, "Parent".into())
        .await?;
    let runner = Arc::new(RejectAfterCapture(AtomicBool::new(false)));
    let parent = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "writer".into(),
                account_binding: None,
                checkout: dunce::canonicalize(&source)?,
                executable: std::env::current_exe()?,
                cli_version: "0.169.0-alpha.11.1".into(),
                runner: runner.identity().into(),
                permission_revision: "writer-fixture".into(),
                mode: SessionMode::ApprovedWrite,
                model: None,
                effort: None,
            },
            "Prepare a writer job".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let job = store
        .submit_agent_job(
            parent.run.id,
            "worktree-start".into(),
            JobRequest {
                checkout_id: Some(managed.checkout_id.clone()),
                task_id: Some(task.id),
                mode: SessionMode::ApprovedWrite,
                instructions: "Edit the fixture".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
        )
        .await?;
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let inspection = store
        .inspect_job(job.project_id.clone(), job.id.clone(), env.clone())
        .await?
        .context("job not persisted")?;
    let [AdmissionBlocker::Approval { approval_id }] = inspection.blockers.as_slice() else {
        anyhow::bail!("expected pending job approval")
    };
    store
        .decide_job_approval(job.project_id.clone(), approval_id.clone(), true)
        .await?;
    let attempt = store
        .claim_next_job(env)
        .await?
        .context("approved writer not claimed")?;
    let run_id = attempt.run.id.clone();
    let result = start_managed_turn(store.clone(), runner.clone(), attempt, None)
        .wait()
        .await?;
    assert_eq!(result, RunStatus::Failed);
    assert!(
        runner.0.load(Ordering::Acquire),
        "writer never reached the attempted CLI launch"
    );
    let start = store
        .worktree_start(job.project_id, run_id.clone())
        .await?
        .context("starting Git state missing")?;
    assert_eq!(start.repository_root, dunce::canonicalize(source)?);
    assert_eq!(start.worktree, managed);
    assert_eq!(start.starting.head, managed.base_commit);
    for file in ["staged.txt", "file.sample", "untracked-private-marker.txt"] {
        assert!(
            start
                .starting
                .status
                .iter()
                .any(|entry| entry.ends_with(file))
        );
    }
    assert_ne!(
        start.starting.staged_diff_digest,
        start.starting.unstaged_diff_digest
    );
    assert!(!managed.root.join("clean-ran").exists());
    let state: Vec<u8> = rusqlite::Connection::open(database)?.query_row(
        "SELECT state FROM job_worktree_starts WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    assert!(
        !state
            .windows(b"untracked-private-marker".len())
            .any(|window| window == b"untracked-private-marker")
    );
    Ok(())
}
