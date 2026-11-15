use std::{
    ffi::OsStr,
    fs,
    io::{self, BufRead, Write},
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use omniagent::{
    conversations::SessionScope,
    jobs::{AdmissionBlocker, AdmissionEnvironment, JobRequest},
    process_io::CapturedProcess,
    providers::{Provider, SessionMode, runtime::start_managed_turn},
    repository::{self, AttemptWorktreeEvidence, ProjectCheckout, provision_task_worktree},
    runner::{DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};
use serde_json::{Value, json};

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

struct CandidateWriter;

impl Runner for CandidateWriter {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments.contains(&OsStr::new("app-server")) {
            return DirectWindowsRunner.launch(LaunchRequest {
                arguments: &[
                    "--ignored",
                    "--exact",
                    "candidate_fixture_worker",
                    "--nocapture",
                ]
                .map(OsStr::new),
                ..request
            });
        }
        Err(io::Error::other(
            "the fixture only implements the app server",
        ))
    }
}

#[test]
#[ignore = "native child for the managed worktree candidate test"]
fn candidate_fixture_worker() -> Result<()> {
    let root = std::env::current_dir()?;
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        let result = match frame["method"].as_str() {
            Some("initialize") => json!({"userAgent":"fixture"}),
            Some("initialized") => continue,
            Some("thread/start") => json!({
                "thread":{"id":"fixture"}, "cwd":root, "model":"fixture",
                "sandbox":{"type":"workspaceWrite"}
            }),
            Some("config/read") => json!({"config":{"sandbox_mode":"workspace-write"}}),
            Some("model/list" | "skills/list") => json!({"data":[]}),
            Some("turn/start") => {
                let binary = root.join("candidate.bin");
                if !binary.exists() {
                    fs::write(root.join("committed.txt"), "Committed by the CLI\n")?;
                    git(&root, &["add", "--", "committed.txt"])?;
                    git(
                        &root,
                        &[
                            "-c",
                            "user.name=Fixture",
                            "-c",
                            "user.email=fixture@example.invalid",
                            "-c",
                            "commit.gpgsign=false",
                            "-c",
                            "core.hooksPath=NUL",
                            "commit",
                            "--quiet",
                            "-m",
                            "Fixture candidate",
                        ],
                    )?;
                    fs::write(&binary, [0, 255, 68, 13, 10])?;
                } else {
                    fs::write(&binary, b"Later file revision\n")?;
                }
                fs::write(root.join("staged.txt"), "Staged by the CLI\n")?;
                git(&root, &["add", "--", "staged.txt"])?;
                fs::write(root.join("staged.txt"), "Unstaged by the CLI\n")?;
                fs::write(root.join("ignored.txt"), "Ignored fixture data\n")?;
                let terminal = json!({"method":"turn/completed","params":{
                    "threadId":"fixture", "turn":{"id":"turn","status":"completed"}
                }});
                let accepted = json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}});
                writeln!(output, "{accepted}\n{terminal}")?;
                output.flush()?;
                // Keep the native process alive until the runtime stops its job object.
                continue;
            }
            other => anyhow::bail!("unexpected fixture request {other:?}"),
        };
        let reply = json!({"id":frame["id"],"result":result});
        writeln!(output, "\n{reply}")?;
        output.flush()?;
    }
    Ok(())
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
        .worktree_start(job.project_id.clone(), run_id.clone())
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
    assert!(matches!(
        store
            .attempt_worktree_evidence(job.project_id, run_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Unavailable { .. })
    ));
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

async fn candidate_parent(
    store: &Store,
    checkout: &ProjectCheckout,
    runner: &CandidateWriter,
) -> Result<String> {
    let conversation = store
        .create_conversation(checkout.checkout_id.clone(), "Parent".into())
        .await?;
    let parent = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "writer".into(),
                account_binding: None,
                checkout: checkout.checkout_root.clone().into(),
                executable: std::env::current_exe()?,
                cli_version: "0.169.0-alpha.11.1".into(),
                runner: runner.identity().into(),
                permission_revision: "writer-fixture".into(),
                mode: SessionMode::ApprovedWrite,
                model: None,
                effort: None,
            },
            "Prepare fixture jobs".into(),
        )
        .await?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    Ok(parent.run.id)
}

#[tokio::test]
async fn same_agent_and_branch_keep_worktree_evidence_scoped_to_project_and_attempt() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let other = repository(&store, &temp.path().join("other")).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Evidence".into())
        .await?;
    // Task IDs are project-scoped. Reuse the ID to give both worktrees the
    // same generated branch name without changing the production provisioner.
    rusqlite::Connection::open(&database)?.execute(
        "INSERT INTO project_tasks (project_id, id, title) VALUES (?1, ?2, 'Sibling evidence')",
        [&other.project_id, &task.id],
    )?;
    let managed = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    let sibling = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        other.project_id.clone(),
        task.id.clone(),
        other.checkout_id.clone(),
    )
    .await?;
    assert_eq!(managed.branch, sibling.branch);
    assert_ne!(managed.checkout_id, sibling.checkout_id);
    assert_ne!(managed.root, sibling.root);
    for project_root in [&project.repository_root, &other.repository_root] {
        let info = Path::new(project_root).join(".git/info");
        fs::create_dir_all(&info)?;
        fs::write(info.join("exclude"), "ignored.txt\n")?;
    }
    fs::write(
        sibling.root.join("sibling-only.txt"),
        "Sibling worktree data\n",
    )?;
    fs::write(
        managed.root.join("previous.txt"),
        "Existing untracked data\n",
    )?;
    fs::write(
        managed.root.join("file.sample"),
        "Preexisting unstaged line\n",
    )?;

    let runner = Arc::new(CandidateWriter);
    let parent = candidate_parent(&store, &project, &runner).await?;
    let other_parent = candidate_parent(&store, &other, &runner).await?;
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let mut ids = Vec::new();
    let mut artifacts = Vec::new();
    for (target, origin, key) in [
        (&managed, &parent, "earlier"),
        (&sibling, &other_parent, "earlier"),
        (&managed, &parent, "later"),
    ] {
        let job = store
            .submit_agent_job(
                origin.clone(),
                key.into(),
                JobRequest {
                    checkout_id: Some(target.checkout_id.clone()),
                    task_id: Some(task.id.clone()),
                    mode: SessionMode::ApprovedWrite,
                    instructions: "Edit the fixture".into(),
                    dependencies: Vec::new(),
                    requires_bridge: false,
                },
            )
            .await?;
        let inspection = store
            .inspect_job(target.project_id.clone(), job.id, env.clone())
            .await?
            .context("job not persisted")?;
        let [AdmissionBlocker::Approval { approval_id }] = inspection.blockers.as_slice() else {
            anyhow::bail!(
                "expected pending job approval, got {:?}",
                inspection.blockers
            );
        };
        store
            .decide_job_approval(target.project_id.clone(), approval_id.clone(), true)
            .await?;
        let attempt = store
            .claim_next_job(env.clone())
            .await?
            .context("writer not claimed")?;
        let run_id = attempt.run.id.clone();
        let status = start_managed_turn(store.clone(), runner.clone(), attempt, None)
            .wait()
            .await?;
        let evidence = store
            .attempt_worktree_evidence(target.project_id.clone(), run_id.clone())
            .await?
            .context("attempt was not found")?;
        let AttemptWorktreeEvidence::Saved { artifact } = evidence else {
            anyhow::bail!("candidate was not saved for {key} ({status:?}): {evidence:?}");
        };
        assert_eq!(status, RunStatus::Succeeded);
        assert_eq!(&artifact.start.worktree, target);
        ids.push(run_id);
        artifacts.push(artifact);
    }
    let first = &artifacts[0];
    let before = first
        .start
        .starting
        .changes
        .as_ref()
        .context("starting changes missing")?;
    assert_eq!(first.start.starting.head, managed.base_commit);
    assert!(
        String::from_utf8(STANDARD.decode(&before.unstaged_patch_base64)?)?
            .contains("+Preexisting unstaged line")
    );
    assert_eq!(before.untracked[0].path, "previous.txt");
    assert!(before.untracked[0].content_base64.is_none());
    let changes = first
        .candidate
        .changes
        .as_ref()
        .context("candidate changes missing")?;
    assert!(
        String::from_utf8(STANDARD.decode(&changes.staged_patch_base64)?)?
            .contains("+Staged by the CLI")
    );
    assert!(
        String::from_utf8(STANDARD.decode(&changes.unstaged_patch_base64)?)?
            .contains("+Unstaged by the CLI")
    );
    assert!(
        String::from_utf8(STANDARD.decode(&first.committed_patch_base64)?)?
            .contains("+Committed by the CLI")
    );
    assert_eq!(
        changes
            .untracked
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["candidate.bin", "previous.txt"]
    );
    assert_eq!(
        STANDARD.decode(changes.untracked[0].content_base64.as_deref().unwrap())?,
        [0, 255, 68, 13, 10]
    );
    let second_project = &artifacts[1];
    assert!(
        second_project
            .candidate
            .changes
            .as_ref()
            .context("sibling changes missing")?
            .untracked
            .iter()
            .any(|file| file.path == "sibling-only.txt")
    );
    assert!(
        !changes
            .untracked
            .iter()
            .any(|file| file.path == "sibling-only.txt")
    );
    let later = &artifacts[2];
    assert_ne!(first.revision, later.revision);
    let later_files = &later
        .candidate
        .changes
        .as_ref()
        .context("later changes missing")?
        .untracked;
    let later_binary = later_files
        .iter()
        .find(|file| file.path == "candidate.bin")
        .and_then(|file| file.content_base64.as_deref())
        .context("later bytes missing")?;
    assert_eq!(STANDARD.decode(later_binary)?, b"Later file revision\n");
    let reopened = Store::open(database)?;
    assert_eq!(
        reopened
            .attempt_worktree_evidence(project.project_id.clone(), ids[0].clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved {
            artifact: first.clone()
        })
    );
    assert!(
        reopened
            .attempt_worktree_evidence(other.project_id.clone(), ids[0].clone())
            .await?
            .is_none()
    );
    assert!(
        reopened
            .attempt_worktree_evidence(project.project_id.clone(), ids[1].clone())
            .await?
            .is_none()
    );
    let sibling_live = reopened
        .live_managed_worktree(other.project_id, task.id.clone())
        .await?
        .context("sibling live checkout missing")?;
    assert_eq!(sibling_live.worktree, sibling);
    assert!(
        sibling_live
            .status
            .iter()
            .any(|entry| entry == "? sibling-only.txt")
    );
    fs::write(managed.root.join("live-only.txt"), "Live checkout only\n")?;
    let live = reopened
        .live_managed_worktree(project.project_id, task.id)
        .await?
        .context("live checkout missing")?;
    assert!(live.status.iter().any(|entry| entry == "? live-only.txt"));
    assert!(
        !first
            .candidate
            .status
            .iter()
            .any(|entry| entry.contains("live-only"))
    );
    Ok(())
}
