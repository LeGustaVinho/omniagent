use std::{
    ffi::OsStr,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

use crate::{
    process::TerminalOutcome,
    process_io::{CapturedProcess, OutputFrame, ProcessInput, Stream},
    runner::{DirectWindowsRunner, LaunchRequest, RunControl, Runner},
};

use super::*;

const WORKER: &str =
    "workflows::git_integration::diff_review::review_runner::frozen_diff_protocol_worker";
const THREAD: &str = "frozen-diff-fixture";
const TURN: &str = "frozen-diff-turn";

pub(super) struct DiffReviewRunner {
    pub live_checkout: PathBuf,
    pub tree: String,
}

struct ProbeDone;

impl RunControl for ProbeDone {
    fn cancel(&self) -> io::Result<bool> {
        Ok(false)
    }

    fn wait(&self) -> io::Result<TerminalOutcome> {
        Ok(TerminalOutcome::Exited(0))
    }
}

impl Runner for DiffReviewRunner {
    fn identity(&self) -> &str {
        "fixture-frozen-review"
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        DirectWindowsRunner.executable_revision(path)
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments == ["plugin", "list", "--json"].map(OsStr::new) {
            let (sender, output) = mpsc::channel();
            sender
                .send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: r#"{"installed":[]}"#.into(),
                }))
                .map_err(io::Error::other)?;
            return Ok(CapturedProcess {
                input: ProcessInput::new(io::sink()),
                output,
                control: Arc::new(ProbeDone),
            });
        }
        if !request.arguments.contains(&OsStr::new("app-server")) {
            return Err(io::Error::other(
                "Unexpected request to the diff review test runner",
            ));
        }
        if request.checkout == self.live_checkout
            || git(request.checkout, &["rev-parse", "HEAD^{tree}"]).map_err(io::Error::other)?
                != self.tree
        {
            return Err(io::Error::other(
                "Review attempted to launch outside the selected immutable Git tree",
            ));
        }
        DirectWindowsRunner.launch(LaunchRequest {
            arguments: &["--quiet", "--ignored", "--exact", WORKER, "--nocapture"].map(OsStr::new),
            ..request
        })
    }
}

#[test]
#[ignore = "launched through the read-only workflow provider adapter"]
fn frozen_diff_protocol_worker() -> Result<()> {
    let root = std::env::current_dir()?;
    assert_eq!(
        fs::read_to_string(root.join("first.txt"))?.trim(),
        "first.txt"
    );
    assert!(git(&root, &["status", "--porcelain"])?.is_empty());
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let message: serde_json::Value = serde_json::from_str(&line?)?;
        let result = match message["method"].as_str() {
            Some("initialize") => json!({"userAgent":"frozen-diff-test"}),
            Some("initialized") => continue,
            Some("thread/start") => json!({
                "thread":{"id":THREAD}, "cwd":root, "model":"fixture", "sandbox":{"type":"readOnly"}
            }),
            Some("config/read") => json!({"config":{}}),
            Some("model/list" | "skills/list" | "mcpServerStatus/list") => json!({"data":[]}),
            Some("turn/start") => {
                writeln!(
                    stdout,
                    "{}",
                    json!({"id":message["id"],"result":{"turn":{"id":TURN}}})
                )?;
                writeln!(
                    stdout,
                    "{}",
                    json!({"method":"turn/started","params":{
                        "threadId":THREAD,"turn":{"id":TURN,"status":"inProgress"}
                    }})
                )?;
                writeln!(
                    stdout,
                    "{}",
                    json!({"method":"item/completed","params":{
                        "threadId":THREAD,"turnId":TURN,
                        "item":{"type":"agentMessage","id":"review","text":"Reviewed the frozen file diff."}
                    }})
                )?;
                writeln!(
                    stdout,
                    "{}",
                    json!({"method":"turn/completed","params":{
                        "threadId":THREAD,"turn":{"id":TURN,"status":"completed"}
                    }})
                )?;
                stdout.flush()?;
                continue;
            }
            Some("turn/interrupt") => json!({}),
            _ => anyhow::bail!("Unexpected diff protocol request {message}"),
        };
        writeln!(stdout, "{}", json!({"id":message["id"], "result":result}))?;
        stdout.flush()?;
    }
    Ok(())
}
