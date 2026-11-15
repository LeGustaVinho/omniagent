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
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, DownstreamWaitState, FailureCause, FailurePhase,
        GoalControlState, GoalDefinition, GoalStopAction, Job, JobContext, JobKind, JobPermission,
        JobRequest, JobRole, JobRule, JobScheduler, JobWorkspace, PipelineCommand,
        PipelineDispatch, PipelineNodeKind, QueueHistoryPolicy, RecoveryAction, ReleaseBlocker,
    },
    process_io::{CapturedProcess, Stream},
    providers::{
        Provider, RequestId, SessionMode,
        questions::{QuestionAction, QuestionOrigin, QuestionReply, QuestionRoute},
        runtime::start_managed_turn,
    },
    repository::{
        self, AttemptWorktreeEvidence, ManagedWorktree, ProjectCheckout, provision_task_worktree,
        with_managed_worktree_writer,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
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
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let parallel = Store::open(database)?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let base = git(&source, &["rev-parse", "HEAD"])?;
    let main_branch = git(&source, &["branch", "--show-current"])?;
    let alternate = temp.path().join("alternate-source");
    git(
        &source,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            alternate.as_os_str(),
            OsStr::new(&base),
        ],
    )?;
    let alternate = repository::register(&parallel, alternate).await?;
    assert_eq!(alternate.project_id, project.project_id);
    assert_ne!(alternate.checkout_id, project.checkout_id);
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
    let (first, concurrent) = tokio::join!(
        provision_task_worktree(
            &store,
            default.clone(),
            project.project_id.clone(),
            task.id.clone(),
            project.checkout_id.clone(),
        ),
        provision_task_worktree(
            &parallel,
            default.clone(),
            project.project_id.clone(),
            task.id.clone(),
            alternate.checkout_id,
        )
    );
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
    let tree = git(&source, &["rev-parse", "HEAD^{tree}"])?;
    let moved = git(
        &source,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-m",
            "Concurrent ref update",
        ],
    )?;
    let reference = format!("refs/heads/{branch}");
    git(&source, &["update-ref", &reference, &moved, &base])?;
    assert!(attach().await.is_err());
    assert!(!root.join("file.sample").exists());
    assert_eq!(git(&root, &["rev-parse", "HEAD"])?, moved);
    git(&source, &["update-ref", &reference, &base, &moved])?;

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
            register_fixture_bridge(&request).map_err(io::Error::other)?;
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

struct NativeGoalFixture(AtomicBool);

struct NativeGoalFixtureControl {
    inner: Arc<dyn omniagent::runner::RunControl>,
    uncertainty_marker: std::path::PathBuf,
}

impl omniagent::runner::RunControl for NativeGoalFixtureControl {
    fn cancel(&self) -> io::Result<bool> {
        self.inner.cancel()
    }

    fn wait(&self) -> io::Result<omniagent::process::TerminalOutcome> {
        self.inner.wait()
    }

    fn confirm_quiescence(&self) -> io::Result<()> {
        if self.uncertainty_marker.exists() {
            return Err(io::Error::other(
                "fixture dropped the worker's stop acknowledgment",
            ));
        }
        self.inner.confirm_quiescence()
    }
}

impl Runner for NativeGoalFixture {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }

    fn execution_platform(&self) -> Option<omniagent::runner::ExecutionPlatform> {
        DirectWindowsRunner.execution_platform()
    }

    fn executable_revision(&self, _path: &Path) -> Option<String> {
        Some("native-goal-fixture".into())
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments.first() == Some(&OsStr::new("plugin")) {
            let powershell = std::path::PathBuf::from(
                std::env::var_os("SystemRoot")
                    .context("missing SystemRoot")
                    .map_err(io::Error::other)?,
            )
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
            return DirectWindowsRunner.launch(LaunchRequest {
                executable: &powershell,
                arguments: &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    r#"Write-Output '{"installed":[]}'"#,
                ]
                .map(OsStr::new),
                ..request
            });
        }
        let native = request
            .arguments
            .contains(&OsStr::new("features.goals=true"));
        if native {
            self.0.store(true, Ordering::Release);
        }
        let marker = request.checkout.join("fixture-native-uncertain-stop");
        let mut process = CandidateWriter.launch(request)?;
        if native {
            process.control = Arc::new(NativeGoalFixtureControl {
                inner: process.control,
                uncertainty_marker: marker,
            });
        }
        Ok(process)
    }
}

fn register_fixture_bridge(request: &LaunchRequest<'_>) -> Result<()> {
    let Some(config) = request
        .arguments
        .iter()
        .filter_map(|arg| arg.to_str())
        .find(|arg| arg.starts_with("mcp_servers="))
    else {
        return Ok(());
    };
    let config = config.parse::<toml::Table>()?;
    let Some(server) = config
        .get("mcp_servers")
        .and_then(|entry| entry.get("omniagent_runtime"))
    else {
        return Ok(());
    };
    let command = server
        .get("command")
        .and_then(toml::Value::as_str)
        .context("fixture bridge command")?;
    let arguments = server
        .get("args")
        .and_then(toml::Value::as_array)
        .context("fixture bridge arguments")?;
    let arguments = arguments
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(OsStr::new)
                .context("non-text fixture bridge argument")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: Path::new(command),
        checkout: request.checkout,
        arguments: &arguments,
    })?;
    let result = (|| -> Result<()> {
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"writer-fixture","version":"1"}
            }}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        ] {
            process.input.send(format!("{message}\n").as_bytes())?;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            let frame = process.output.recv_timeout(remaining)??;
            if frame.stream != Stream::Stdout {
                continue;
            }
            let frame: Value = serde_json::from_str(&frame.data)?;
            if frame["id"] == 2 {
                ensure!(
                    frame["result"]["tools"]
                        .as_array()
                        .is_some_and(|tools| tools
                            .iter()
                            .any(|tool| tool["name"] == "report_goal_progress")),
                    "fixture could not register the goal bridge"
                );
                return Ok(());
            }
        }
        anyhow::bail!("fixture bridge registration timed out")
    })();
    process.control.cancel()?;
    process.control.wait()?;
    result
}

#[test]
#[ignore = "native child for the managed worktree candidate test"]
fn candidate_fixture_worker() -> Result<()> {
    let root = std::env::current_dir()?;
    let native_fixture = root.join("fixture-native-goal").is_file();
    let thread = if native_fixture {
        uuid::Uuid::new_v4().to_string()
    } else {
        "fixture".into()
    };
    let mut native_goal = Value::Null;
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let frame: Value = serde_json::from_str(&line?)?;
        let result = match frame["method"].as_str() {
            Some("initialize") => json!({"userAgent":"fixture"}),
            Some("initialized") => continue,
            Some("thread/start") => json!({
                "thread":{"id":thread}, "cwd":root, "model":"fixture",
                "sandbox":{"type":"workspaceWrite"}
            }),
            Some("config/read") if native_fixture => json!({"config":{
                "sandbox_mode":"workspace-write","features":{"goals":true},
                "changedProfile":root.join("fixture-native-profile-changed").is_file()
            }}),
            Some("config/read") => json!({"config":{"sandbox_mode":"workspace-write"}}),
            Some("skills/list") if native_fixture => json!({"data":[{"cwd":root,"skills":[]}]}),
            Some("model/list" | "skills/list") => json!({"data":[]}),
            Some("thread/goal/set") if native_fixture => {
                let params = &frame["params"];
                if params["objective"].is_null() {
                    if root.join("fixture-native-uncertain-stop").exists()
                        && params["status"] == "paused"
                    {
                        let rejected = json!({"id":frame["id"], "error":{"code":-32000,"message":"Stop receipt unavailable"}});
                        writeln!(output, "{rejected}")?;
                        output.flush()?;
                        continue;
                    }
                    native_goal["status"] = params["status"].clone();
                    json!({"goal":native_goal})
                } else {
                    native_goal = json!({
                        "threadId":thread, "status":"active", "objective":params["objective"],
                        "tokenBudget":params["tokenBudget"], "tokensUsed":0u64
                    });
                    let start = |turn| {
                        json!({"method":"turn/started","params":{
                            "threadId":thread,"turn":{"id":turn}
                        }})
                    };
                    let done = |turn| {
                        json!({"method":"turn/completed","params":{
                            "threadId":thread,"turn":{"id":turn,"status":"completed"}
                        }})
                    };
                    fs::write(root.join("candidate.bin"), b"native first turn")?;
                    let reply = json!({"id":frame["id"],"result":{"goal":native_goal}});
                    native_goal["tokensUsed"] = json!(137);
                    writeln!(
                        output,
                        "{reply}\n{}\n{}\n{}\n{}",
                        start("native-first"),
                        done("native-first"),
                        start("native-second"),
                        done("native-first")
                    )?;
                    fs::write(root.join("candidate.bin"), b"native second turn")?;
                    if root.join("fixture-native-question").is_file() {
                        fs::write(
                            root.join("candidate.bin"),
                            b"native candidate awaiting input",
                        )?;
                        let question = json!({"id":72,"method":"item/tool/requestUserInput","params":{
                            "threadId":thread,"turnId":"native-second","itemId":"question",
                            "questions":[{"id":"target","question":"Where should I write?","options":[{"label":"This worktree"}]}]
                        }});
                        let expired = json!({"method":"serverRequest/resolved","params":{
                            "threadId":thread,"requestId":72
                        }});
                        writeln!(output, "{question}\n{expired}")?;
                    } else if root.join("fixture-native-complete").is_file() {
                        native_goal["status"] = json!("complete");
                        let observed = json!({"method":"thread/goal/updated","params":{
                            "threadId":thread,"turnId":"native-second","goal":native_goal
                        }});
                        writeln!(output, "{observed}\n{}", done("native-second"))?;
                    }
                    output.flush()?;
                    continue;
                }
            }
            Some("thread/goal/get") if native_fixture => json!({"goal":native_goal}),
            Some("thread/inject_items") if native_fixture => {
                let text = frame["params"]["items"][0]["content"][0]["text"]
                    .as_str()
                    .context("missing native goal fixture seed")?;
                ensure!(
                    text.contains("Save the writer result")
                        && !text.contains("report_goal_progress")
                );
                json!({})
            }
            Some("thread/goal/clear") if native_fixture => {
                native_goal = Value::Null;
                json!({"cleared":true})
            }
            Some("turn/start") => {
                let text = frame
                    .pointer("/params/input/0/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let report = if text.contains("The user answered questions from the original turn.")
                {
                    ensure!(
                        text.contains("\"target\":\"This worktree\""),
                        "The linked writer missed the typed answer"
                    );
                    fs::write(root.join("fixture-continued"), [])?;
                    Some("Continued the original write with the answer")
                } else if text.contains("\"selectedResults\"") {
                    ensure!(
                        text.contains("Selected downstream report"),
                        "The continuation missed the selected report"
                    );
                    ensure!(
                        text.contains("Selected command report"),
                        "The continuation missed the command output"
                    );
                    ensure!(
                        !text.contains("non-selected report"),
                        "The continuation received an unselected result"
                    );
                    Some("Continued with the selected report")
                } else if text == "Produce downstream report fixture" {
                    fs::write(
                        root.join("downstream.txt"),
                        "Child writer changed this file\n",
                    )?;
                    Some("Selected downstream report")
                } else {
                    None
                };
                if let Some(report) = report {
                    let accepted = json!({"id":frame["id"], "result":{"turn":{"id":"turn"}}});
                    let message = json!({"method":"item/completed","params":{
                        "threadId":thread, "turnId":"turn", "item":{"type":"agentMessage","id":"message","text":report}
                    }});
                    let terminal = json!({"method":"turn/completed","params":{
                        "threadId":thread, "turn":{"id":"turn","status":"completed"}
                    }});
                    writeln!(output, "{accepted}\n{message}\n{terminal}")?;
                    output.flush()?;
                    continue;
                }
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
                    "threadId":thread, "turn":{"id":"turn","status":"completed"}
                }});
                let accepted = json!({"id":frame["id"],"result":{"turn":{"id":"turn"}}});
                writeln!(output, "{accepted}")?;
                if root.join("fixture-expire-question").exists() {
                    let request = json!({"id":71,"method":"item/tool/requestUserInput","params":{
                        "threadId":thread,"turnId":"turn","itemId":"question",
                        "questions":[{"id":"target","question":"Where should I write?","options":[{"label":"This worktree"}]}]
                    }});
                    let expired = json!({"method":"serverRequest/resolved","params":{
                        "threadId":thread,"requestId":71
                    }});
                    writeln!(output, "{request}\n{expired}")?;
                } else if root.join("fixture-hold-open").exists() {
                    fs::write(root.join("fixture-ready"), [])?;
                } else {
                    writeln!(output, "{terminal}")?;
                }
                output.flush()?;
                // Keep the native process alive until the runtime stops its job object.
                continue;
            }
            Some("turn/interrupt") => {
                if root.join("fixture-expire-question").exists() {
                    fs::write(root.join("fixture-interrupted"), [])?;
                }
                let status = if root.join("fixture-complete-on-interrupt").exists() {
                    "completed"
                } else {
                    "interrupted"
                };
                let reply = json!({"id":frame["id"],"result":{}});
                let terminal = json!({"method":"turn/completed", "params":{
                    "threadId":thread, "turn":{"id":frame["params"]["turnId"],"status":status}
                }});
                writeln!(output, "{reply}\n{terminal}")?;
                output.flush()?;
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
    let tracked = managed.root.join("file.sample");
    let original_mtime = fs::metadata(&tracked)?.modified()?;
    fs::write(&tracked, b"Edited content\n")?;
    let original_times = fs::FileTimes::new().set_modified(original_mtime);
    fs::OpenOptions::new()
        .write(true)
        .open(&tracked)?
        .set_times(original_times)?;
    let index = git(
        &managed.root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    fs::OpenOptions::new()
        .write(true)
        .open(index)?
        .set_times(original_times)?;
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
                .any(|entry| entry.ends_with(file)),
            "captured Git status omitted {file}: {:?}",
            start.starting.status,
        );
    }
    assert_ne!(
        start.starting.staged_diff_digest,
        start.starting.unstaged_diff_digest
    );
    assert!(!managed.root.join("clean-ran").exists());
    let failure = store
        .job_failure(job.project_id.clone(), run_id.clone())
        .await?
        .context("failed launch has no record")?;
    assert_eq!(failure.outcome, RunStatus::Failed);
    assert_eq!(failure.checkout_id, managed.checkout_id);
    assert_eq!(failure.observation.phase, FailurePhase::Launch);
    assert_eq!(
        failure.observation.cause,
        Some(FailureCause::LaunchRejected)
    );
    assert!(failure.observation.process.is_none());
    assert!(failure.logs_available && !failure.saved_diff_available);
    assert!(
        store
            .job_failure("other-project".into(), run_id.clone())
            .await?
            .is_none()
    );
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

async fn queue_approved_writer(
    store: &Store,
    env: &AdmissionEnvironment,
    checkout: &ManagedWorktree,
    origin: &str,
    scope: &str,
    key: &str,
) -> Result<Job> {
    let job = store
        .submit_agent_job(
            origin.into(),
            key.into(),
            JobRequest {
                checkout_id: Some(checkout.checkout_id.clone()),
                task_id: Some(scope.into()),
                mode: SessionMode::ApprovedWrite,
                instructions: "Edit the fixture".into(),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
        )
        .await?;
    let inspection = store
        .inspect_job(job.project_id.clone(), job.id.clone(), env.clone())
        .await?
        .context("job not persisted")?;
    let approval = inspection
        .blockers
        .iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("expected pending writer approval")?;
    store
        .decide_job_approval(job.project_id.clone(), approval.clone(), true)
        .await?;
    Ok(job)
}

struct DownstreamFixtureRunner;

impl Runner for DownstreamFixtureRunner {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        (path.file_name() == Some(OsStr::new("powershell.exe")))
            .then(|| DirectWindowsRunner.executable_revision(path))
            .flatten()
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.executable.file_name() == Some(OsStr::new("powershell.exe")) {
            DirectWindowsRunner.launch(request)
        } else {
            CandidateWriter.launch(request)
        }
    }
}

#[tokio::test]
async fn waiting_writer_yields_its_only_slot_and_resumes_once_with_frozen_child_results()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("downstream.sqlite");
    let store = Store::open(database.clone())?;
    let source_path = temp.path().join("source");
    let project = repository(&store, &source_path).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Downstream fixture".into())
        .await?;
    let managed = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    fs::create_dir_all(source_path.join(".git/info"))?;
    fs::write(
        source_path.join(".git/info/exclude"),
        "fixture-hold-open\nfixture-ready\nignored.txt\n",
    )?;
    fs::write(managed.root.join("fixture-hold-open"), [])?;
    let runtime = Arc::new(DownstreamFixtureRunner);
    store.set_job_queue_capacity(1).await?;
    let env = AdmissionEnvironment {
        runner: runtime.identity().into(),
        bridge_available: false,
    };
    let pipeline = "downstream-fixture".to_owned();
    store
        .register_job_pipeline(project.project_id.clone(), pipeline.clone())
        .await?;
    for (node, kind) in [
        ("parent", PipelineNodeKind::Execution),
        ("command", PipelineNodeKind::Execution),
        ("approval", PipelineNodeKind::ImplementationApproval),
    ] {
        store
            .register_job_pipeline_node(
                project.project_id.clone(),
                pipeline.clone(),
                node.into(),
                kind,
            )
            .await?;
    }
    store
        .set_job_pipeline_plan(
            project.project_id.clone(),
            pipeline.clone(),
            None,
            "plan-v1".into(),
        )
        .await?;
    let release = store
        .record_job_pipeline_release(
            project.project_id.clone(),
            pipeline.clone(),
            "approval".into(),
            "plan-v1".into(),
            managed.checkout_id.clone(),
            task.id.clone(),
        )
        .await?;
    let binding = |node: &str| PipelineDispatch {
        pipeline_run_id: pipeline.clone(),
        node_instance_id: node.into(),
        plan_revision: Some(release.plan_revision.clone()),
        release_id: Some(release.id.clone()),
    };
    store
        .set_project_job_rules(
            project.project_id.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let conversation = store
        .create_conversation(managed.checkout_id.clone(), "Pinned stage".into())
        .await?;
    let parent = store
        .submit_pinned_pipeline_agent_job(
            binding("parent"),
            SessionScope {
                provider: Provider::Codex,
                profile_id: "downstream-fixture".into(),
                account_binding: None,
                checkout: managed.root.clone(),
                executable: std::env::current_exe()?,
                cli_version: "0.169.0-alpha.11.1".into(),
                runner: runtime.identity().into(),
                permission_revision: "writer-fixture".into(),
                mode: SessionMode::ApprovedWrite,
                model: None,
                effort: None,
            },
            JobRequest {
                checkout_id: Some(managed.checkout_id.clone()),
                task_id: Some(task.id.clone()),
                mode: SessionMode::ApprovedWrite,
                instructions: "Edit the fixture and wait for downstream work".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: conversation.context_id.clone(),
            },
            QuestionRoute::Native,
        )
        .await?;
    let scheduler = JobScheduler::start(store.clone(), runtime.clone(), None);
    tokio::time::timeout(std::time::Duration::from_secs(90), async {
        while !managed.root.join("fixture-ready").is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    let current = store
        .job(project.project_id.clone(), parent.id.clone())
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert_eq!(current.status, RunStatus::Running);
    let submit = |request_id: &str, instructions: &str| {
        store.submit_agent_job(
            current.run_id.clone(),
            request_id.into(),
            JobRequest {
                checkout_id: None,
                task_id: None,
                mode: SessionMode::ApprovedWrite,
                instructions: instructions.into(),
                dependencies: vec![],
                requires_bridge: false,
            },
        )
    };
    let selected = submit("selected", "Produce downstream report fixture").await?;
    let cancelled = submit("cancelled", "This selected command is cancelled").await?;
    let ignored = submit("ignored", "non-selected report").await?;
    let command = store
        .submit_pipeline_command_job(
            binding("command"),
            managed.checkout_id.clone(),
            task.id.clone(),
            vec![],
            PipelineCommand {
                executable: std::path::PathBuf::from(
                    std::env::var_os("SystemRoot").context("Windows root is unavailable")?,
                )
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
                arguments: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-Command".into(),
                    "Write-Output 'Selected command report'".into(),
                ],
            },
        )
        .await?;
    assert!(
        store
            .request_downstream_wait(
                current.run_id.clone(),
                "wrong-route".into(),
                vec![command.id.clone()]
            )
            .await
            .is_err()
    );
    let inspection = store
        .inspect_job(project.project_id.clone(), selected.id.clone(), env.clone())
        .await?
        .unwrap();
    assert!(inspection.blockers.iter().any(|blocker| matches!(
        blocker,
        AdmissionBlocker::Capacity {
            active: 1,
            limit: 1
        }
    )));
    assert!(inspection.blockers.iter().any(|blocker| matches!(blocker, AdmissionBlocker::Writer { run_id, .. } if run_id == &current.run_id)));
    let approval = inspection
        .blockers
        .into_iter()
        .find_map(|blocker| match blocker {
            AdmissionBlocker::Approval { approval_id } => Some(approval_id),
            _ => None,
        })
        .context("The child didn't request approval")?;
    store
        .decide_job_approval(project.project_id.clone(), approval, true)
        .await?;
    store.set_job_queue_paused(true).await?;
    let wait = store
        .request_pipeline_downstream_wait(
            current.run_id.clone(),
            "fixture-wait".into(),
            vec![
                selected.id.clone(),
                cancelled.id.clone(),
                command.id.clone(),
            ],
        )
        .await?;
    let duplicate = store
        .request_pipeline_downstream_wait(
            current.run_id.clone(),
            "fixture-wait".into(),
            vec![
                cancelled.id.clone(),
                command.id.clone(),
                selected.id.clone(),
            ],
        )
        .await?;
    assert_eq!(duplicate.id, wait.id);
    assert!(
        store
            .request_downstream_wait(
                current.run_id.clone(),
                "fixture-wait".into(),
                vec![ignored.id]
            )
            .await
            .is_err()
    );
    store
        .set_pending_job_state(
            project.project_id.clone(),
            cancelled.id.clone(),
            omniagent::jobs::QueueState::Cancelled,
        )
        .await?;
    let parked = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let wait = store
                .downstream_wait(project.project_id.clone(), current.run_id.clone())
                .await?
                .unwrap();
            if wait.state != DownstreamWaitState::Yielding {
                return Ok::<_, anyhow::Error>(wait);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await??;
    assert_eq!(parked.state, DownstreamWaitState::Waiting);
    let text = "Wait for the owning pipeline before opening a separate turn";
    store
        .save_conversation_draft(conversation.id.clone(), text.into())
        .await?;
    let queued_instruction = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: conversation.context_id.clone(),
            },
            SessionScope {
                provider: Provider::Codex,
                profile_id: "read-only operator".into(),
                account_binding: None,
                checkout: managed.root.clone(),
                executable: std::env::current_exe()?,
                cli_version: "0.169.0-alpha.11.1".into(),
                runner: runtime.identity().into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            text.into(),
        )
        .await?;
    let pending = store
        .inspect_job(
            queued_instruction.project_id.clone(),
            queued_instruction.id.clone(),
            env.clone(),
        )
        .await?
        .context("The instruction did not stay visible while waiting for the pipeline")?;
    assert!(
        pending
            .blockers
            .contains(&AdmissionBlocker::ContinuationOwner {
                run_id: current.run_id.clone()
            })
    );
    store
        .set_pending_job_state(
            queued_instruction.project_id,
            queued_instruction.id,
            omniagent::jobs::QueueState::Cancelled,
        )
        .await?;
    let parent_conversation = current
        .conversation_id
        .as_ref()
        .context("The parent conversation is unavailable")?;
    assert_eq!(parent_conversation, &conversation.id);
    assert_ne!(parked.context_id.as_ref(), Some(&conversation.context_id));
    store
        .set_conversation_queue_history_policy(
            conversation.id.clone(),
            Some(QueueHistoryPolicy::Recorded),
        )
        .await?;
    let visible_after_clear = store
        .rewind_conversation_history(conversation.id.clone(), conversation.context_id, None)
        .await?;
    let queue = store
        .inspect_project_queue(project.project_id.clone(), env.clone())
        .await?;
    assert_eq!(queue.active, 0);
    let selected_after_yield = queue
        .jobs
        .iter()
        .find(|job| job.job.id == selected.id)
        .unwrap();
    assert_eq!(
        selected_after_yield.blockers,
        [AdmissionBlocker::QueuePaused]
    );
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.project_id.clone(), current.run_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    scheduler.shutdown().await?;
    drop(store);

    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    assert_eq!(
        store
            .downstream_wait(project.project_id.clone(), current.run_id.clone())
            .await?,
        Some(parked)
    );
    let scheduler = JobScheduler::start(store.clone(), runtime, None);
    store.set_job_queue_paused(false).await?;
    let continued = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        loop {
            let attempt = store
                .job(project.project_id.clone(), parent.id.clone())
                .await?
                .unwrap()
                .latest_attempt
                .unwrap();
            if attempt.ordinal > current.ordinal && attempt.status.is_terminal() {
                return Ok::<_, anyhow::Error>(attempt);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await??;
    assert_eq!(continued.status, RunStatus::Succeeded);
    assert_eq!(continued.ordinal, 2);
    assert_eq!(continued.previous_run_id.as_ref(), Some(&current.run_id));
    let evidence = store
        .downstream_wait(project.project_id.clone(), current.run_id.clone())
        .await?
        .unwrap();
    assert_eq!(evidence.state, DownstreamWaitState::Continued);
    assert_eq!(
        evidence.continuation_run_id.as_ref(),
        Some(&continued.run_id)
    );
    let observed_child = evidence
        .inputs
        .iter()
        .find(|input| input.child_job_id == selected.id)
        .unwrap();
    assert_eq!(observed_child.status, Some(RunStatus::Succeeded));
    assert!(observed_child.child_run_id.is_some() && observed_child.result_context_id.is_some());
    assert!(
        evidence
            .inputs
            .iter()
            .any(|input| input.child_job_id == command.id
                && input.status == Some(RunStatus::Succeeded)
                && input.child_run_id.is_some()
                && input.result_context_id.is_none())
    );
    assert!(
        evidence
            .inputs
            .iter()
            .any(|input| input.child_job_id == cancelled.id
                && input.status == Some(RunStatus::Cancelled)
                && input.child_run_id.is_none())
    );
    let dispatch = store
        .agent_dispatch(parent_conversation.clone(), continued.run_id)
        .await?
        .context("Missing continuation dispatch")?;
    let continuation = &dispatch
        .snapshot
        .submitted_messages
        .last()
        .context("Missing continuation input")?
        .text;
    assert!(continuation.contains("Selected downstream report") && continuation.contains(&wait.id));
    assert!(continuation.contains("Selected command report"));
    assert!(!continuation.contains("non-selected report"));
    assert_eq!(
        store
            .conversation(conversation.id.clone())
            .await?
            .context_id,
        visible_after_clear
    );
    assert!(
        store
            .conversation_messages(conversation.id)
            .await?
            .is_empty()
    );
    assert!(managed.root.join("downstream.txt").is_file());
    scheduler.shutdown().await?;
    assert!(store.claim_next_job(env).await?.is_none());
    Ok(())
}

struct VerifyCommandOnRetry(AtomicBool);

impl Runner for VerifyCommandOnRetry {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        if self.0.swap(false, Ordering::SeqCst) {
            return None;
        }
        DirectWindowsRunner.executable_revision(path)
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        DirectWindowsRunner.launch(request)
    }
}

#[tokio::test]
async fn pipeline_commands_need_the_exact_release_and_share_supervision_and_linked_recovery()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("command.sqlite");
    let store = Store::open(database.clone())?;
    let source = repository(&store, &temp.path().join("source")).await?;
    let project = source.project_id;
    let task = store
        .create_project_task(project.clone(), "Command scope".into())
        .await?;
    let other = store
        .create_project_task(project.clone(), "Different scope".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        source.checkout_id.clone(),
    )
    .await?;
    let other_tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        other.id.clone(),
        source.checkout_id,
    )
    .await?;
    let pipeline = "command-pipeline".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "command".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "approval".into(),
            PipelineNodeKind::ImplementationApproval,
        )
        .await?;
    store
        .set_job_pipeline_plan(project.clone(), pipeline.clone(), None, "plan-a".into())
        .await?;
    let release = store
        .record_job_pipeline_release(
            project.clone(),
            pipeline.clone(),
            "approval".into(),
            "plan-a".into(),
            tree.checkout_id.clone(),
            task.id.clone(),
        )
        .await?;
    store
        .set_project_job_rules(
            project.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Pipeline,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let executable = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").context("Windows root is unavailable")?,
    )
    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let enqueue =
        |tree: &ManagedWorktree, task_id: &str, release_id: Option<String>, script: &str| {
            store.submit_pipeline_command_job(
                PipelineDispatch {
                    pipeline_run_id: pipeline.clone(),
                    node_instance_id: "command".into(),
                    plan_revision: Some("plan-a".into()),
                    release_id,
                },
                tree.checkout_id.clone(),
                task_id.into(),
                vec![],
                PipelineCommand {
                    executable: executable.clone(),
                    arguments: vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-Command".into(),
                        script.into(),
                    ],
                },
            )
        };
    let missing = enqueue(&tree, &task.id, None, "throw 'unreleased command'").await?;
    let wrong = enqueue(
        &other_tree,
        &other.id,
        Some(release.id.clone()),
        "throw 'wrong scope'",
    )
    .await?;
    let command = enqueue(
        &tree,
        &task.id,
        Some(release.id.clone()),
        "Set-Content -LiteralPath './command.txt' -Value 'approved'; Write-Output 'command-output'",
    )
    .await?;
    assert_eq!(command.kind, JobKind::Command);
    let scheduler = JobScheduler::start(
        store.clone(),
        Arc::new(VerifyCommandOnRetry(AtomicBool::new(true))),
        None,
    );
    let env = scheduler.environment();
    let inspect = |job: &Job| store.inspect_job(project.clone(), job.id.clone(), env.clone());
    for (blocked, reason) in [
        (&missing, ReleaseBlocker::Missing),
        (&wrong, ReleaseBlocker::WrongScope),
    ] {
        let job = inspect(blocked).await?.unwrap();
        assert!(job.job.latest_attempt.is_none());
        assert_eq!(job.blockers, [AdmissionBlocker::PipelineRelease { reason }]);
    }
    let terminal = |job: Job, store: Store| async move {
        tokio::time::timeout(std::time::Duration::from_secs(45), async {
            loop {
                if let Some(attempt) = store
                    .job(job.project_id.clone(), job.id.clone())
                    .await?
                    .unwrap()
                    .latest_attempt
                    .filter(|attempt| attempt.status.is_terminal())
                {
                    return Ok::<_, anyhow::Error>(attempt);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await?
    };
    let failed = terminal(command.clone(), store.clone()).await?;
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(!tree.root.join("command.txt").exists());
    store
        .queue_job_recovery(
            project.clone(),
            command.id.clone(),
            failed.run_id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let retry = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            let done = terminal(command.clone(), store.clone()).await?;
            if done.ordinal > failed.ordinal {
                return Ok::<_, anyhow::Error>(done);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await??;
    assert_eq!(retry.status, RunStatus::Succeeded);
    assert_eq!(retry.previous_run_id.as_ref(), Some(&failed.run_id));
    assert!(retry.conversation_id.is_none());
    assert_eq!(
        store
            .attempt_pipeline_origin(project.clone(), retry.run_id.clone())
            .await?
            .unwrap()
            .release
            .as_ref(),
        Some(&release)
    );
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), retry.run_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));

    let held = enqueue(
        &tree,
        &task.id,
        Some(release.id),
        "Set-Content -LiteralPath './command-ready.txt' -Value 'partial'; Start-Sleep -Seconds 90",
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while !tree.root.join("command-ready.txt").is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    let active = inspect(&held).await?.unwrap().job.latest_attempt.unwrap();
    store
        .request_project_job_interrupt(project.clone(), held.id.clone(), active.run_id)
        .await?;
    let cancelled = terminal(held, store.clone()).await?;
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), cancelled.run_id)
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    scheduler.shutdown().await?;

    let reopened = Store::open(database)?;
    let logs = reopened
        .command_job_output(project, retry.run_id.clone())
        .await?
        .unwrap();
    assert!(logs.stdout.contains("command-output"));
    assert_eq!(logs.process.unwrap().code, 0);
    assert!(
        reopened
            .command_job_output("another-project".into(), retry.run_id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn expired_writer_question_retains_partial_work_and_releases_the_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Partial question".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    fs::create_dir_all(source.join(".git/info"))?;
    fs::write(
        source.join(".git/info/exclude"),
        "ignored.txt\nfixture-expire-question\nfixture-interrupted\nfixture-continued\n",
    )?;
    fs::write(tree.root.join("fixture-expire-question"), [])?;
    let runner = Arc::new(CandidateWriter);
    let parent = candidate_parent(&store, &project, &runner).await?;
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let job =
        queue_approved_writer(&store, &env, &tree, &parent, &task.id, "question-writer").await?;
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), None);
    let queue = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let queue = store
                .inspect_project_queue(project.project_id.clone(), env.clone())
                .await?;
            if queue.jobs.iter().any(|entry| {
                entry.job.id == job.id
                    && entry
                        .job
                        .latest_attempt
                        .as_ref()
                        .is_some_and(|a| a.status.is_terminal())
            }) {
                break Ok::<_, anyhow::Error>(queue);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .context("writer did not settle after the provider expired its question")??;
    assert!(tree.root.join("fixture-interrupted").exists());
    assert_eq!(queue.active, 0);
    let inspection = queue
        .jobs
        .into_iter()
        .find(|entry| entry.job.id == job.id)
        .unwrap();
    assert_eq!(inspection.question_wait.unwrap().saved, 1);
    let attempt = inspection.job.latest_attempt.unwrap();
    let owner = attempt
        .conversation_id
        .context("writer missing a conversation")?;
    assert!(matches!(
        store.attempt_worktree_evidence(project.project_id.clone(), attempt.run_id.clone()).await?,
        Some(AttemptWorktreeEvidence::Saved { artifact }) if artifact.candidate.changes.as_ref()
            .is_some_and(|changes| changes.untracked.iter().any(|file| file.path == "candidate.bin"))
    ));
    with_managed_worktree_writer(
        &store,
        project.project_id.clone(),
        tree.checkout_id,
        task.id,
        |_| Ok(()),
    )
    .await?;
    scheduler.shutdown().await?;
    let reopened = Store::open(database)?;
    reopened.recover_agent_attempts_after_restart().await?;
    let saved = reopened
        .saved_conversation_questions(owner.clone(), attempt.run_id.clone())
        .await?;
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (&*saved[0].state, &*saved[0].reason),
        ("saved", "provider_expired")
    );
    let origin = QuestionOrigin {
        run_id: attempt.run_id.clone(),
        request_id: RequestId::Number(71),
    };
    let reply = QuestionReply {
        action: QuestionAction::Accept,
        values: [("target".into(), json!("This worktree"))]
            .into_iter()
            .collect(),
    };
    reopened
        .answer_saved_question(owner.clone(), origin.clone(), reply.clone())
        .await?;
    let candidate = tree.root.join("candidate.bin");
    let original = fs::read(&candidate)?;
    fs::write(&candidate, b"Edited after answering")?;
    let scheduler = JobScheduler::start(reopened.clone(), runner, None);
    let settled = |ordinal| {
        let store = &reopened;
        let project = &project.project_id;
        let job = &job;
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(60), async {
                loop {
                    let current = store
                        .job(project.clone(), job.id.clone())
                        .await?
                        .context("missing writer job")?;
                    if let Some(attempt) = current.latest_attempt
                        && attempt.ordinal == ordinal
                        && attempt.status.is_terminal()
                    {
                        break Ok::<_, anyhow::Error>(attempt);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await?
        }
    };
    let rejected = settled(2).await?;
    assert_eq!(rejected.status, RunStatus::Failed);
    assert_eq!(rejected.previous_run_id, Some(attempt.run_id));
    assert!(
        reopened
            .agent_dispatch(owner.clone(), rejected.run_id.clone())
            .await?
            .is_none()
    );
    assert!(!tree.root.join("fixture-continued").exists());
    assert!(
        reopened
            .answer_saved_question(owner, origin, reply)
            .await
            .is_err()
    );
    fs::write(&candidate, original)?;
    reopened
        .queue_job_recovery(
            project.project_id.clone(),
            job.id.clone(),
            rejected.run_id,
            RecoveryAction::Retry,
        )
        .await?;
    assert_eq!(settled(3).await?.status, RunStatus::Succeeded);
    assert!(tree.root.join("fixture-continued").is_file());
    scheduler.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn qualified_native_goal_keeps_one_writer_reservation_across_autonomous_turns() -> Result<()>
{
    use omniagent::{
        jobs::{GoalContinuationOwner, GoalRevision},
        providers::{
            Capability,
            capabilities::{CapabilitySubject, EvidenceOrigin, ObservedResult},
            goal::NativeGoalAction,
        },
    };
    use std::{collections::BTreeSet, time::Duration};

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let checkout = repository(&store, &source).await?;
    let project = checkout.project_id;
    let task = store
        .create_project_task(project.clone(), "Native goal writer".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        checkout.checkout_id,
    )
    .await?;
    fs::create_dir_all(source.join(".git/info"))?;
    fs::write(
        source.join(".git/info/exclude"),
        "ignored.txt\nfixture-native-goal\nfixture-native-complete\nfixture-native-profile-changed\nfixture-native-uncertain-stop\nfixture-native-question\n",
    )?;
    fs::write(tree.root.join("fixture-native-goal"), [])?;
    let runner = Arc::new(NativeGoalFixture(AtomicBool::new(false)));
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: true,
    };
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "native-goal-writer".into(),
        account_binding: Some("fixture-account".into()),
        checkout: tree.root.clone(),
        executable: std::env::current_exe()?,
        cli_version: "0.169.0-alpha.11.1".into(),
        runner: runner.identity().into(),
        permission_revision: "native-goal-fixture".into(),
        mode: SessionMode::ApprovedWrite,
        model: Some("fixture".into()),
        effort: None,
    };
    let queue = |title: &'static str, max_turns, token_budget| {
        let (store, project, task, scope, checkout, env) = (
            store.clone(),
            project.clone(),
            task.id.clone(),
            scope.clone(),
            tree.checkout_id.clone(),
            env.clone(),
        );
        async move {
            let chat = store.create_conversation(checkout, title.into()).await?;
            let goal = store
                .start_conversation_goal(
                    JobContext {
                        conversation_id: chat.id.clone(),
                        context_id: chat.context_id,
                    },
                    scope,
                    Some(task),
                    GoalDefinition {
                        objective: title.into(),
                        criteria: vec!["Save the writer result".into()],
                        max_turns,
                        max_seconds: Some(90),
                        token_budget,
                    },
                    None,
                )
                .await?;
            let inspect = store
                .inspect_job(project.clone(), goal.job.id.clone(), env)
                .await?
                .unwrap();
            let [AdmissionBlocker::Approval { approval_id }] = inspect.blockers.as_slice() else {
                anyhow::bail!("expected goal approval, got {:?}", inspect.blockers);
            };
            store
                .decide_job_approval(project, approval_id.clone(), true)
                .await?;
            Ok::<_, anyhow::Error>((chat.id, goal))
        }
    };
    async fn finished_goal(store: &Store, chat: &str, ordinal: u32) -> Result<GoalRevision> {
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                let goal = store
                    .active_conversation_goal(chat.into())
                    .await?
                    .context("fixture goal disappeared")?;
                if goal
                    .job
                    .latest_attempt
                    .as_ref()
                    .is_some_and(|a| a.ordinal == ordinal && a.status.is_terminal())
                {
                    return Ok::<_, anyhow::Error>(goal);
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .with_context(|| format!("goal {chat} did not settle attempt {ordinal}"))?
    }
    async fn native_turn(
        store: &Store,
        database: &Path,
        chat: &str,
        ordinal: u32,
    ) -> Result<(String, String)> {
        tokio::time::timeout(Duration::from_secs(35), async {
            loop {
                let goal = store.active_conversation_goal(chat.into()).await?.context("missing writer goal")?;
                if let Some(attempt) = goal.job.latest_attempt.as_ref().filter(|value| value.ordinal == ordinal) {
                    ensure!(!attempt.status.is_terminal(), "native goal ended before its second turn: {goal:?}");
                    let open: bool = rusqlite::Connection::open(database)?.query_row(
                        "SELECT EXISTS(SELECT 1 FROM native_goal_turns
                         WHERE run_id = ?1 AND provider_turn_id = 'native-second' AND outcome IS NULL)",
                        [&attempt.run_id], |row| row.get(0),
                    )?;
                    if open {
                        let session = goal.provider_session_id.context("native attempt has no thread")?;
                        return Ok::<_, anyhow::Error>((attempt.run_id.clone(), session));
                    }
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }).await.context("native writer did not reach its second turn")?
    }
    store.set_job_queue_capacity(1).await?;
    let bridge = std::path::PathBuf::from(env!("CARGO_BIN_EXE_omniagent"));
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), Some(bridge.clone()));
    let (calibration_chat, _) =
        queue("Record the fixture's effective profile", Some(1), None).await?;
    let calibration = finished_goal(&store, &calibration_chat, 1)
        .await
        .context("effective profile calibration")?;
    let run = calibration
        .job
        .latest_attempt
        .context("missing calibration attempt")?;
    assert_eq!(run.status, RunStatus::Succeeded);
    assert!(!runner.0.load(Ordering::Acquire));
    let assessment = store
        .agent_dispatch(calibration_chat, run.run_id.clone())
        .await?
        .context("missing calibration dispatch")?
        .snapshot
        .capabilities
        .context("missing capability scope")?;
    let descriptor = assessment
        .entries
        .iter()
        .find(|entry| {
            entry.descriptor.subject
                == CapabilitySubject::Protocol {
                    capability: Capability::NativeGoalCommand,
                }
        })
        .context("missing native goal capability")?
        .descriptor
        .clone();
    store
        .record_capability_evidence(
            assessment.scope.clone(),
            descriptor,
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Succeeded,
            run.run_id,
        )
        .await?;
    let controls = BTreeSet::from([
        NativeGoalAction::SeedContext,
        NativeGoalAction::Start,
        NativeGoalAction::Status,
        NativeGoalAction::Pause,
        NativeGoalAction::Resume,
        NativeGoalAction::Cancel,
    ]);
    store
        .register_native_goal_qualification(
            scope.clone(),
            assessment.scope.clone(),
            controls.clone(),
            true,
        )
        .await?;

    let (chat, goal) = queue("Run two native turns", None, Some(40_000)).await?;
    let (first_run, first_session) = native_turn(&store, &database, &chat, 1).await?;
    assert!(runner.0.load(Ordering::Acquire));
    let rejected_delegation = store
        .submit_agent_job(
            first_run.clone(),
            "outside-budget".into(),
            JobRequest {
                checkout_id: None,
                task_id: None,
                mode: SessionMode::ReadOnly,
                instructions: "Consume a separate model session".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        rejected_delegation.to_string(),
        "Delegated work cannot enforce this goal's token budget"
    );
    assert_eq!(
        store
            .inspect_project_queue(project.clone(), env.clone())
            .await?
            .active,
        1
    );
    assert!(
        with_managed_worktree_writer(
            &store,
            project.clone(),
            tree.checkout_id.clone(),
            task.id.clone(),
            |_| Ok(())
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
            .await?,
        GoalControlState::Pausing
    );
    let paused = finished_goal(&store, &chat, 1).await?;
    assert_eq!(paused.control_state, GoalControlState::Paused);
    assert_eq!(
        paused.job.latest_attempt.unwrap().status,
        RunStatus::Interrupted
    );
    assert_eq!(
        store
            .inspect_project_queue(project.clone(), env.clone())
            .await?
            .active,
        0
    );
    scheduler.shutdown().await?;
    let reopened = Store::open(database.clone())?;
    reopened.recover_agent_attempts_after_restart().await?;
    assert!(reopened.claim_next_job(env.clone()).await?.is_none());
    assert!(
        reopened
            .queue_job_recovery(
                project.clone(),
                goal.job.id.clone(),
                first_run.clone(),
                RecoveryAction::Resume
            )
            .await
            .is_err()
    );
    fs::write(tree.root.join("candidate.bin"), b"external drift")?;
    assert!(
        reopened
            .resume_goal(project.clone(), goal.id.clone())
            .await
            .is_err()
    );
    fs::write(tree.root.join("candidate.bin"), b"native second turn")?;
    reopened
        .resume_goal(project.clone(), goal.id.clone())
        .await?;
    assert!(
        reopened
            .resume_goal(project.clone(), goal.id.clone())
            .await
            .is_err()
    );
    assert_eq!(
        reopened
            .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
            .await?,
        GoalControlState::Paused
    );
    assert!(reopened.claim_next_job(env.clone()).await?.is_none());
    reopened
        .resume_goal(project.clone(), goal.id.clone())
        .await?;
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), Some(bridge.clone()));
    let (second_run, second_session) = native_turn(&store, &database, &chat, 2).await?;
    assert_ne!(first_session, second_session);
    let resumed_budget: i64 = rusqlite::Connection::open(&database)?.query_row(
        "SELECT token_budget FROM native_goal_executors WHERE run_id = ?1",
        [&second_run],
        |row| row.get(0),
    )?;
    assert_eq!(resumed_budget, 40_000 - 137);
    assert_eq!(
        store
            .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Cancel)
            .await?,
        GoalControlState::Cancelling
    );
    let cancelled = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = store
                .job(project.clone(), goal.job.id.clone())
                .await?
                .unwrap();
            if let Some(attempt) = job.latest_attempt.filter(|a| a.status.is_terminal()) {
                return Ok::<_, anyhow::Error>(attempt);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .context("native writer did not settle cancellation")??;
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.ordinal, 2);
    assert_eq!(cancelled.run_id, second_run);
    assert_eq!(
        cancelled.previous_run_id.as_deref(),
        Some(first_run.as_str())
    );
    assert!(
        store
            .active_conversation_goal(chat.clone())
            .await?
            .is_none()
    );
    let initial_dispatch = store
        .agent_dispatch(chat, cancelled.run_id.clone())
        .await?
        .context("missing native context dispatch")?;
    let [seed] = initial_dispatch.frames.as_slice() else {
        anyhow::bail!("native context has more than one initial dispatch");
    };
    let wire: Value = serde_json::from_slice(seed)?;
    assert_eq!(wire["method"], "thread/inject_items");
    let turns: Vec<(String, String)> = rusqlite::Connection::open(&database)?
        .prepare("SELECT provider_turn_id, outcome FROM native_goal_turns WHERE run_id = ?1 ORDER BY started_event_seq")?
        .query_map([&cancelled.run_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    assert_eq!(
        turns,
        [
            ("native-first".into(), "completed".into()),
            ("native-second".into(), "interrupted".into())
        ]
    );
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.clone(), cancelled.run_id)
        .await?
    else {
        anyhow::bail!("native cancellation lost its worktree candidate");
    };
    let binary = artifact
        .candidate
        .changes
        .as_ref()
        .and_then(|changes| {
            changes
                .untracked
                .iter()
                .find(|item| item.path == "candidate.bin")
        })
        .and_then(|file| file.content_base64.as_deref())
        .context("missing saved native worktree bytes")?;
    assert_eq!(STANDARD.decode(binary)?, b"native second turn");
    with_managed_worktree_writer(
        &store,
        project.clone(),
        tree.checkout_id.clone(),
        task.id.clone(),
        |_| Ok(()),
    )
    .await?;

    fs::write(tree.root.join("fixture-native-complete"), [])?;
    let (chat, _) = queue("Finish a native goal", None, None).await?;
    let completed = finished_goal(&store, &chat, 1)
        .await
        .context("native completion")?;
    assert_eq!(completed.continuation_owner, GoalContinuationOwner::Native);
    let completed_run = completed
        .job
        .latest_attempt
        .as_ref()
        .unwrap()
        .run_id
        .clone();
    let ledger: (u32, u32, u32) = rusqlite::Connection::open(&database)?.query_row(
        "SELECT (SELECT count(*) FROM native_goal_turns WHERE run_id = ?1 AND outcome = 'completed'),
                (SELECT count(*) FROM goal_checkpoints WHERE revision_id = ?2),
                (SELECT count(*) FROM goal_continuations WHERE revision_id = ?2)",
        [&completed_run, &completed.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    // The fixture repeats the first turn's terminal message after the second starts.
    assert_eq!(ledger, (2, 1, 1));
    assert_eq!(
        completed.job.latest_attempt.unwrap().status,
        RunStatus::Succeeded
    );
    assert_eq!(
        completed
            .checkpoint
            .context("missing native checkpoint")?
            .next_action,
        "review"
    );
    assert_eq!(
        store
            .inspect_project_queue(project.clone(), env.clone())
            .await?
            .active,
        0
    );

    fs::write(tree.root.join("fixture-native-question"), [])?;
    let (chat, _) = queue(
        "Wait for input during a native writer goal",
        None,
        Some(40_000),
    )
    .await?;
    let waiting = finished_goal(&store, &chat, 1)
        .await
        .context("native question wait")?;
    assert_eq!(waiting.control_state, GoalControlState::Active);
    let prior_session = waiting
        .provider_session_id
        .context("native wait lost its provider thread")?;
    let prior_run = waiting
        .job
        .latest_attempt
        .context("native wait lost its attempt")?
        .run_id;
    let saved = store
        .saved_conversation_questions(chat.clone(), prior_run.clone())
        .await?;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].state, "saved");
    assert_eq!(
        store
            .inspect_project_queue(project.clone(), env.clone())
            .await?
            .active,
        0
    );
    fs::remove_file(tree.root.join("fixture-native-question"))?;
    store
        .answer_saved_question(
            chat.clone(),
            QuestionOrigin {
                run_id: prior_run.clone(),
                request_id: RequestId::Number(72),
            },
            QuestionReply {
                action: QuestionAction::Accept,
                values: [("target".into(), json!("This worktree"))]
                    .into_iter()
                    .collect(),
            },
        )
        .await?;
    let answered = finished_goal(&store, &chat, 2)
        .await
        .context("native question continuation")?;
    assert_eq!(answered.continuation_owner, GoalContinuationOwner::Native);
    assert_ne!(
        answered.provider_session_id.as_deref(),
        Some(prior_session.as_str())
    );
    let resumed = answered
        .job
        .latest_attempt
        .context("missing resumed question attempt")?;
    assert_eq!(resumed.previous_run_id.as_deref(), Some(prior_run.as_str()));
    assert_eq!(resumed.status, RunStatus::Succeeded);
    let seed = store
        .agent_dispatch(chat, resumed.run_id.clone())
        .await?
        .context("missing native question dispatch")?;
    let request: Value = serde_json::from_slice(&seed.frames[0])?;
    assert_eq!(request["method"], "thread/inject_items");
    assert!(
        request
            .pointer("/params/items/0/content/0/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("\"target\":\"This worktree\""))
    );
    let budget: i64 = rusqlite::Connection::open(&database)?.query_row(
        "SELECT token_budget FROM native_goal_executors WHERE run_id = ?1",
        [&resumed.run_id],
        |row| row.get(0),
    )?;
    assert_eq!(budget, 40_000 - 137);

    fs::remove_file(tree.root.join("fixture-native-complete"))?;
    let (chat, native) = queue("Hand off a paused native goal", None, None).await?;
    let (native_run, native_session) = native_turn(&store, &database, &chat, 1).await?;
    fs::write(tree.root.join("fixture-native-uncertain-stop"), [])?;
    assert_eq!(
        store
            .request_goal_stop(project.clone(), native.id.clone(), GoalStopAction::Pause)
            .await?,
        GoalControlState::Pausing
    );
    let review = finished_goal(&store, &chat, 1).await?;
    assert_eq!(review.control_state, GoalControlState::NeedsReview);
    let blocked_chat = store.conversation(chat.clone()).await?;
    let mut read_only = scope.clone();
    read_only.mode = SessionMode::ReadOnly;
    let overlapping = store
        .prepare_agent_attempt(
            chat.clone(),
            blocked_chat.context_id,
            read_only,
            "A separate read-only turn during the uncertain native stop".into(),
        )
        .await;
    assert!(
        overlapping.is_err_and(|error| error.to_string().contains("already has an active turn"))
    );
    assert!(
        store
            .confirm_native_goal_stop(project.clone(), native.id.clone(), native_run.clone())
            .await
            .is_err()
    );
    assert!(
        with_managed_worktree_writer(
            &store,
            project.clone(),
            tree.checkout_id.clone(),
            task.id.clone(),
            |_| Ok(())
        )
        .await
        .is_err()
    );
    assert!(
        store
            .resume_goal(project.clone(), native.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .confirm_native_goal_stop(project.clone(), native.id.clone(), first_run)
            .await
            .is_err()
    );
    let current_chat = store.conversation(chat.clone()).await?;
    assert!(
        store
            .start_conversation_goal(
                JobContext {
                    conversation_id: chat.clone(),
                    context_id: current_chat.context_id,
                },
                scope.clone(),
                Some(task.id.clone()),
                native.definition.clone(),
                None
            )
            .await
            .is_err()
    );
    scheduler.shutdown().await?;

    let reopened = Store::open(database.clone())?;
    reopened.recover_agent_attempts_after_restart().await?;
    assert!(reopened.claim_next_job(env.clone()).await?.is_none());
    assert_eq!(
        reopened
            .confirm_native_goal_stop(project.clone(), native.id.clone(), native_run.clone())
            .await?,
        GoalControlState::Paused
    );
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), native_run.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    fs::write(tree.root.join("fixture-native-profile-changed"), [])?;
    fs::remove_file(tree.root.join("fixture-native-uncertain-stop"))?;
    reopened
        .resume_goal(project.clone(), native.id.clone())
        .await?;
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), Some(bridge));
    let fallback = match finished_goal(&store, &chat, 3).await {
        Ok(goal) => goal,
        Err(error) => {
            let live = store.active_conversation_goal(chat.clone()).await?.unwrap();
            let inspect = store.inspect_job(project.clone(), live.job.id, env).await?;
            return Err(error).context(format!("managed fallback state: {inspect:?}"));
        }
    };
    assert_eq!(fallback.continuation_owner, GoalContinuationOwner::Managed);
    assert_ne!(
        fallback.provider_session_id.as_deref(),
        Some(native_session.as_str())
    );
    let attempt = fallback
        .job
        .latest_attempt
        .context("missing managed retry")?;
    assert_eq!(attempt.status, RunStatus::Succeeded);
    assert_ne!(
        attempt.previous_run_id.as_deref(),
        Some(native_run.as_str())
    );
    assert!(
        store
            .agent_dispatch(chat.clone(), attempt.previous_run_id.unwrap())
            .await?
            .is_none()
    );
    let retry = store
        .agent_dispatch(chat, attempt.run_id.clone())
        .await?
        .context("managed fallback was not dispatched")?;
    assert!(
        retry
            .snapshot
            .scoped_launch
            .context("managed fallback launch evidence")?
            .inputs
            .iter()
            .any(|input| input.contents == "features.goals=false")
    );
    let ledger: (u32, u32, String) = rusqlite::Connection::open(&database)?.query_row(
        "SELECT (SELECT count(*) FROM native_goal_executors WHERE revision_id = ?1 AND stop_confirmed),
                (SELECT count(*) FROM native_goal_executors WHERE revision_id = ?1 AND NOT stop_confirmed),
                (SELECT run_id FROM goal_checkpoints WHERE revision_id = ?1 AND ordinal = 1)",
        [&native.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(ledger, (1, 0, attempt.run_id));
    scheduler.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn paused_goal_resumes_only_from_the_confirmed_writer_candidate() -> Result<()> {
    use std::time::Duration;

    let temp = tempfile::tempdir()?;
    let database = temp.path().join("goal.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let checkout = repository(&store, &source).await?;
    let project = checkout.project_id;
    let task = store
        .create_project_task(project.clone(), "Goal writer".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        checkout.checkout_id,
    )
    .await?;
    fs::create_dir_all(source.join(".git/info"))?;
    fs::write(
        source.join(".git/info/exclude"),
        "ignored.txt\nfixture-hold-open\nfixture-ready\n",
    )?;
    fs::write(tree.root.join("fixture-hold-open"), [])?;
    let ready = tree.root.join("fixture-ready");
    let conversation = store
        .create_conversation(tree.checkout_id.clone(), "Writer goal".into())
        .await?;
    let runner = Arc::new(CandidateWriter);
    let bridge = std::path::PathBuf::from(env!("CARGO_BIN_EXE_omniagent"));
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: true,
    };
    let goal = store
        .start_conversation_goal(
            JobContext {
                conversation_id: conversation.id.clone(),
                context_id: conversation.context_id,
            },
            SessionScope {
                provider: Provider::Codex,
                profile_id: "writer".into(),
                account_binding: None,
                checkout: tree.root.clone(),
                executable: std::env::current_exe()?,
                cli_version: "0.169.0-alpha.11.1".into(),
                runner: runner.identity().into(),
                permission_revision: "writer-fixture".into(),
                mode: SessionMode::ApprovedWrite,
                model: None,
                effort: None,
            },
            Some(task.id.clone()),
            GoalDefinition {
                objective: "Edit a candidate and wait for the operator".into(),
                criteria: Vec::new(),
                max_turns: Some(2),
                max_seconds: None,
                token_budget: None,
            },
            None,
        )
        .await?;
    let job = &goal.job.id;
    let pending = store
        .inspect_job(project.clone(), job.clone(), env.clone())
        .await?
        .unwrap();
    let [AdmissionBlocker::Approval { approval_id }] = pending.blockers.as_slice() else {
        anyhow::bail!("Expected a writer approval, got {:?}", pending.blockers);
    };
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    store
        .decide_job_approval(project.clone(), approval_id.clone(), true)
        .await?;
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), Some(bridge.clone()));
    tokio::time::timeout(Duration::from_secs(90), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("The goal writer did not reach the provider")?;
    let original = store
        .job(project.clone(), job.clone())
        .await?
        .unwrap()
        .latest_attempt
        .unwrap();
    assert_eq!(
        store
            .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Pause)
            .await?,
        GoalControlState::Pausing
    );
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let current = store
                .active_conversation_goal(conversation.id.clone())
                .await?
                .unwrap();
            if current.control_state == GoalControlState::Paused {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), original.run_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    assert_eq!(
        store
            .inspect_project_queue(project.clone(), env.clone())
            .await?
            .active,
        0
    );
    assert!(
        store
            .queue_job_recovery(
                project.clone(),
                job.clone(),
                original.run_id.clone(),
                RecoveryAction::Resume
            )
            .await
            .is_err()
    );
    scheduler.shutdown().await?;
    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    fs::remove_file(&ready)?;
    let file = tree.root.join("candidate.bin");
    let prior = fs::read(&file)?;
    fs::write(&file, b"External edit before requesting resume")?;
    assert!(
        store
            .resume_goal(project.clone(), goal.id.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("worktree content changed")
    );
    assert_eq!(
        store
            .active_conversation_goal(conversation.id.clone())
            .await?
            .unwrap()
            .control_state,
        GoalControlState::Paused
    );
    fs::write(&file, &prior)?;
    store.resume_goal(project.clone(), goal.id.clone()).await?;
    let reserved = store
        .claim_next_job(env.clone())
        .await?
        .context("Resume did not claim a writer")?;
    assert_eq!(reserved.previous_run_id.as_ref(), Some(&original.run_id));
    assert!(matches!(
        reserved.intent,
        omniagent::providers::SessionIntent::Start
    ));
    store.recover_agent_attempts_after_restart().await?;
    fs::write(&file, b"External edit after reserving the resumed attempt")?;
    let scheduler = JobScheduler::start(store.clone(), runner, Some(bridge));
    let settled = |ordinal| {
        let (store, project, job) = (&store, &project, job.clone());
        async move {
            tokio::time::timeout(Duration::from_secs(90), async {
                loop {
                    if let Some(attempt) = store
                        .job(project.clone(), job.clone())
                        .await?
                        .unwrap()
                        .latest_attempt
                        && attempt.ordinal == ordinal
                        && attempt.status.is_terminal()
                    {
                        break Ok::<_, anyhow::Error>(attempt);
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await?
        }
    };
    let rejected = settled(3).await?;
    assert_eq!(rejected.status, RunStatus::Failed);
    assert!(
        store
            .agent_dispatch(conversation.id.clone(), rejected.run_id.clone())
            .await?
            .is_none()
    );
    assert!(!ready.is_file());
    fs::write(&file, &prior)?;
    store
        .queue_job_recovery(
            project.clone(),
            job.clone(),
            rejected.run_id,
            RecoveryAction::Retry,
        )
        .await?;
    tokio::time::timeout(Duration::from_secs(90), async {
        while !ready.is_file() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("The restored candidate was not resumed")?;
    assert_eq!(fs::read(&file)?, b"Later file revision\n");
    assert_eq!(
        store
            .request_goal_stop(project.clone(), goal.id.clone(), GoalStopAction::Cancel)
            .await?,
        GoalControlState::Cancelling
    );
    let cancelled = settled(4).await?;
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert!(
        store
            .active_conversation_goal(conversation.id)
            .await?
            .is_none()
    );
    assert!(matches!(
        store
            .attempt_worktree_evidence(project.clone(), cancelled.run_id)
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    with_managed_worktree_writer(&store, project, tree.checkout_id, task.id, |_| Ok(())).await?;
    scheduler.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn exit_saves_partial_writer_evidence_and_keeps_it_on_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let source = temp.path().join("source");
    let project = repository(&store, &source).await?;
    let task = store
        .create_project_task(project.project_id.clone(), "Partial edits".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        task.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    let info = source.join(".git/info");
    fs::create_dir_all(&info)?;
    fs::write(
        info.join("exclude"),
        "ignored.txt\nfixture-hold-open\nfixture-ready\n",
    )?;
    fs::write(tree.root.join("fixture-hold-open"), [])?;
    let runner = Arc::new(CandidateWriter);
    let parent = candidate_parent(&store, &project, &runner).await?;
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let job = queue_approved_writer(&store, &env, &tree, &parent, &task.id, "exit-writer").await?;
    let scheduler = JobScheduler::start(store.clone(), runner, None);
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !tree.root.join("fixture-ready").is_file() {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .context("writer did not produce partial edits")?;
    scheduler.shutdown().await?;
    let interrupted = store
        .job(project.project_id.clone(), job.id.clone())
        .await?
        .unwrap();
    assert!(interrupted.queued_recovery.is_none());
    let attempt = interrupted.latest_attempt.unwrap();
    assert_eq!(attempt.status, RunStatus::Interrupted);
    let failure = store
        .job_failure(project.project_id.clone(), attempt.run_id.clone())
        .await?
        .context("interrupted writer has no failure record")?;
    assert_eq!(failure.observation.phase, FailurePhase::Execution);
    assert_eq!(failure.observation.cause, Some(FailureCause::Shutdown));
    assert!(failure.observation.process.is_some());
    assert_eq!(failure.provider_outcome.as_deref(), Some("interrupted"));
    assert!(failure.saved_diff_available);
    let saved = store
        .attempt_worktree_evidence(project.project_id.clone(), attempt.run_id.clone())
        .await?
        .context("interrupted writer evidence missing")?;
    let AttemptWorktreeEvidence::Saved { artifact } = &saved else {
        anyhow::bail!("partial candidate was not captured during exit");
    };
    let files = &artifact
        .candidate
        .changes
        .as_ref()
        .context("missing captured changes")?
        .untracked;
    assert!(
        files
            .iter()
            .any(|file| file.path == "candidate.bin" && file.content_base64.is_some())
    );
    fs::write(tree.root.join("candidate.bin"), b"new live content")?;
    let reopened = Store::open(database)?;
    reopened.recover_agent_attempts_after_restart().await?;
    assert_eq!(
        reopened
            .job_failure(project.project_id.clone(), attempt.run_id.clone())
            .await?,
        Some(failure)
    );
    assert_eq!(
        reopened
            .attempt_worktree_evidence(project.project_id.clone(), attempt.run_id.clone())
            .await?,
        Some(saved)
    );
    let recovered = reopened.job(project.project_id, job.id).await?.unwrap();
    assert_eq!(recovered.latest_attempt.unwrap(), attempt);
    assert!(recovered.queued_recovery.is_none());
    Ok(())
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
    let shared = store
        .create_project_task(project.project_id.clone(), "Shared scope".into())
        .await?;
    store
        .assign_shared_worktree(
            project.project_id.clone(),
            shared.id.clone(),
            managed.checkout_id.clone(),
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
    for (target, origin, scope, key) in [
        (&managed, &parent, &shared.id, "earlier"),
        (&sibling, &other_parent, &task.id, "earlier"),
        (&managed, &parent, &shared.id, "later"),
    ] {
        let job = queue_approved_writer(&store, &env, target, origin, scope, key).await?;
        assert_eq!(job.task_id.as_ref(), Some(scope));
        assert_eq!(
            job.worktree_owner_task_id.as_ref(),
            Some(&target.owner_task_id)
        );
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
    let competing =
        queue_approved_writer(&store, &env, &managed, &parent, &task.id, "owner").await?;
    let blockers = store
        .inspect_job(project.project_id.clone(), competing.id, env.clone())
        .await?
        .context("competing job missing")?
        .blockers;
    assert_eq!(
        blockers,
        [AdmissionBlocker::PendingWorktreeChanges {
            checkout_id: managed.checkout_id.clone(),
            scope_task_id: shared.id,
            producing_run_id: Some(ids[0].clone()),
        }]
    );
    assert!(store.claim_next_job(env).await?.is_none());
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

#[tokio::test]
async fn writer_cancellation_and_completion_preserve_the_first_terminal_outcome() -> Result<()> {
    for (complete_on_interrupt, expected, late_outcome) in [
        (true, RunStatus::Succeeded, RunStatus::Cancelled),
        (false, RunStatus::Cancelled, RunStatus::Succeeded),
    ] {
        let temp = tempfile::tempdir()?;
        let database = temp.path().join("store.sqlite");
        let store = Store::open(database.clone())?;
        let observer = Store::open(database)?;
        let source = temp.path().join("source");
        let project = repository(&store, &source).await?;
        let project_id = project.project_id.clone();
        let task = store
            .create_project_task(project_id.clone(), "Writer race".into())
            .await?;
        let tree = provision_task_worktree(
            &store,
            temp.path().join("worktrees"),
            project_id.clone(),
            task.id.clone(),
            project.checkout_id.clone(),
        )
        .await?;
        let info = source.join(".git/info");
        fs::create_dir_all(&info)?;
        fs::write(
            info.join("exclude"),
            "ignored.txt\nfixture-hold-open\nfixture-ready\nfixture-complete-on-interrupt\n",
        )?;
        fs::write(tree.root.join("fixture-hold-open"), [])?;
        if complete_on_interrupt {
            fs::write(tree.root.join("fixture-complete-on-interrupt"), [])?;
        }

        let runner = Arc::new(CandidateWriter);
        let parent = candidate_parent(&store, &project, &runner).await?;
        let environment = AdmissionEnvironment {
            runner: runner.identity().into(),
            bridge_available: false,
        };
        let job =
            queue_approved_writer(&store, &environment, &tree, &parent, &task.id, "race").await?;
        let scheduler = JobScheduler::start(store.clone(), runner, None);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while !tree.root.join("fixture-ready").is_file() {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .context("writer did not reach the interrupt boundary")?;
        let running = observer
            .job(project_id.clone(), job.id.clone())
            .await?
            .and_then(|job| job.latest_attempt)
            .context("writer attempt missing at the interrupt boundary")?;
        assert_eq!(running.status, RunStatus::Running);
        assert!(
            with_managed_worktree_writer(
                &observer,
                project_id.clone(),
                tree.checkout_id.clone(),
                task.id.clone(),
                |_| Ok(())
            )
            .await
            .is_err()
        );
        let run_id = running.run_id;
        observer
            .request_project_job_interrupt(project_id.clone(), job.id.clone(), run_id.clone())
            .await?;
        let finished = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let attempt = observer
                    .job(project_id.clone(), job.id.clone())
                    .await?
                    .and_then(|job| job.latest_attempt)
                    .context("writer attempt missing during settlement")?;
                if attempt.status.is_terminal() {
                    break Ok::<_, anyhow::Error>(attempt);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .context("writer did not finish after interrupt")??;
        assert_eq!(finished.run_id, run_id);
        assert_eq!(finished.status, expected);
        assert!(!finished.cancel_requested);
        let evidence = observer
            .attempt_worktree_evidence(project_id.clone(), run_id.clone())
            .await?;
        let Some(AttemptWorktreeEvidence::Saved { artifact }) = &evidence else {
            anyhow::bail!("terminal writer attempt has no saved candidate");
        };
        assert_eq!(artifact.start.worktree, tree);
        let changes = artifact
            .candidate
            .changes
            .as_ref()
            .context("no candidate changes")?;
        let binary = changes
            .untracked
            .iter()
            .find(|file| file.path == "candidate.bin")
            .and_then(|file| file.content_base64.as_deref())
            .context("missing candidate bytes")?;
        assert_eq!(STANDARD.decode(binary)?, [0, 255, 68, 13, 10]);

        assert!(
            observer
                .request_project_job_interrupt(project_id.clone(), job.id.clone(), run_id.clone())
                .await
                .is_err()
        );
        assert!(
            store
                .finish_agent_attempt(run_id.clone(), late_outcome, None)
                .await
                .is_err()
        );
        assert_eq!(
            observer
                .job(project_id.clone(), job.id)
                .await?
                .and_then(|job| job.latest_attempt),
            Some(finished)
        );
        with_managed_worktree_writer(
            &observer,
            project_id.clone(),
            tree.checkout_id,
            task.id,
            |worktree| {
                Ok(fs::write(
                    worktree.root.join("candidate.bin"),
                    b"later writer",
                )?)
            },
        )
        .await?;
        assert_eq!(
            observer
                .attempt_worktree_evidence(project_id, run_id)
                .await?,
            evidence
        );
        scheduler.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn app_and_cli_writers_share_reservations_and_keep_uncertain_writes_after_restart()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let project = repository(&store, &temp.path().join("source")).await?;
    let owner = store
        .create_project_task(project.project_id.clone(), "Owner".into())
        .await?;
    let scope = store
        .create_project_task(project.project_id.clone(), "Shared".into())
        .await?;
    let tree = provision_task_worktree(
        &store,
        temp.path().join("worktrees"),
        project.project_id.clone(),
        owner.id.clone(),
        project.checkout_id.clone(),
    )
    .await?;
    store
        .assign_shared_worktree(
            project.project_id.clone(),
            scope.id.clone(),
            tree.checkout_id.clone(),
        )
        .await?;
    let runner = CandidateWriter;
    let parent = candidate_parent(&store, &project, &runner).await?;
    let env = AdmissionEnvironment {
        runner: runner.identity().into(),
        bridge_available: false,
    };
    let owner_job = queue_approved_writer(&store, &env, &tree, &parent, &owner.id, "owner").await?;
    let (entered, wait_entered) = tokio::sync::oneshot::channel();
    let (proceed, wait_proceed) = std::sync::mpsc::channel();
    let app = tokio::spawn({
        let (store, tree, scope) = (store.clone(), tree.clone(), scope.id.clone());
        async move {
            with_managed_worktree_writer(
                &store,
                tree.project_id,
                tree.checkout_id,
                scope,
                move |worktree| {
                    entered.send(()).map_err(|_| {
                        anyhow::anyhow!("test did not observe the active app write")
                    })?;
                    wait_proceed.recv_timeout(std::time::Duration::from_secs(20))?;
                    fs::write(
                        worktree.root.join("app-change.txt"),
                        "App write from the shared scope\n",
                    )?;
                    Ok(())
                },
            )
            .await
        }
    });
    wait_entered.await?;
    let inspect_owner = || {
        store.inspect_job(
            project.project_id.clone(),
            owner_job.id.clone(),
            env.clone(),
        )
    };
    assert!(
        matches!(inspect_owner().await?.unwrap().blockers.as_slice(),
        [AdmissionBlocker::AppWriter { checkout_id, .. }] if checkout_id == &tree.checkout_id)
    );
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    proceed.send(())?;
    app.await??;
    assert!(
        matches!(inspect_owner().await?.unwrap().blockers.as_slice(),
        [AdmissionBlocker::PendingWorktreeChanges { scope_task_id, producing_run_id: None, .. }] if scope_task_id == &scope.id)
    );

    queue_approved_writer(
        &store,
        &env,
        &tree,
        &parent,
        &scope.id,
        "cancel-before-launch",
    )
    .await?;
    let before_launch = store
        .claim_next_job(env.clone())
        .await?
        .context("same scope was blocked")?;
    let app_attempt = || {
        with_managed_worktree_writer(
            &store,
            project.project_id.clone(),
            tree.checkout_id.clone(),
            scope.id.clone(),
            |_| Ok(()),
        )
    };
    assert!(
        app_attempt()
            .await
            .unwrap_err()
            .to_string()
            .contains(&before_launch.run.id)
    );
    store
        .finish_agent_attempt(before_launch.run.id, RunStatus::Cancelled, None)
        .await?;
    app_attempt().await?;

    queue_approved_writer(&store, &env, &tree, &parent, &scope.id, "lost-after-launch").await?;
    let uncertain = store
        .claim_next_job(env.clone())
        .await?
        .context("uncertain fixture could not claim")?;
    store.mark_agent_running(uncertain.run.id.clone()).await?;
    let reopened = Store::open(database)?;
    reopened.recover_agent_attempts_after_restart().await?;
    let blockers = reopened
        .inspect_job(project.project_id.clone(), owner_job.id, env.clone())
        .await?
        .context("owner job missing after restart")?
        .blockers;
    assert!(matches!(blockers.as_slice(),
        [AdmissionBlocker::Writer { run_id, needs_review: true, .. }] if run_id == &uncertain.run.id));
    assert!(reopened.claim_next_job(env).await?.is_none());
    let rejected = with_managed_worktree_writer(
        &reopened,
        project.project_id,
        tree.checkout_id,
        scope.id,
        |_| Ok(()),
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains(&uncertain.run.id));
    Ok(())
}
