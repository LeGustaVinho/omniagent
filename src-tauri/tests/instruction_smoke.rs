use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    providers::{
        ApprovalDecision, Provider, ProviderEventData, SessionIntent, SessionMode, SessionRequest,
        TurnInput, TurnOutcome,
    },
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
};
use uuid::Uuid;

struct Worktree {
    _temporary: tempfile::TempDir,
    primary: PathBuf,
    linked: PathBuf,
    cwd: PathBuf,
}

impl Worktree {
    fn new() -> Result<Self> {
        let temporary = tempfile::tempdir()?;
        let root = dunce::canonicalize(temporary.path())?;
        let primary = root.join("primary");
        let linked = root.join("linked");
        let cwd = linked.join("api");
        fs::create_dir(&primary)?;
        git(&primary, &["init", "--quiet"])?;
        git(
            &primary,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Fixture",
            ],
        )?;
        git(
            &primary,
            &[
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                linked.as_os_str(),
            ],
        )?;
        fs::create_dir(&cwd)?;
        Ok(Self {
            _temporary: temporary,
            primary,
            linked,
            cwd,
        })
    }
}

fn git(cwd: &Path, args: &[impl AsRef<OsStr>]) -> Result<()> {
    let output = Command::new("git.exe")
        .args(args)
        .current_dir(cwd)
        .output()?;
    ensure!(
        output.status.success(),
        "could not prepare the disposable worktree"
    );
    Ok(())
}

fn marker(document: &Path) -> Result<String> {
    let marker = format!("OMNI_{}", Uuid::new_v4().simple());
    fs::write(
        document,
        format!(
            "For the instruction audit, include this marker verbatim in your answer: {marker}.\n"
        ),
    )?;
    Ok(marker)
}

fn invoke(
    provider: Provider,
    model: &str,
    mode: SessionMode,
    cwd: &Path,
    launch_prefix: &[&str],
) -> Result<String> {
    let adapter = provider.adapter();
    let executable = adapter.discover(None)?;
    let mut session = adapter.create_session(SessionRequest {
        checkout: cwd.to_path_buf(),
        intent: SessionIntent::Start,
        mode,
        model: Some(model.into()),
        effort: None,
    })?;
    let arguments = launch_prefix
        .iter()
        .map(OsString::from)
        .chain(session.arguments())
        .collect::<Vec<_>>();
    let arguments = arguments
        .iter()
        .map(|value| value.as_os_str())
        .collect::<Vec<_>>();
    let mut process = DirectWindowsRunner.launch(LaunchRequest {
        version: CONTRACT_VERSION,
        executable: &executable,
        checkout: cwd,
        arguments: &arguments,
    })?;
    for frame in session.start()?.outbound {
        process.input.send(frame.as_bytes())?;
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut output = String::new();
    let mut sent = false;
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("instruction audit did not finish")??;
        let update = session.receive(frame)?;
        for frame in update.outbound {
            process.input.send(frame.as_bytes())?;
        }
        for event in update.events {
            let ready = matches!(event.data, ProviderEventData::SessionReady { .. })
                || (provider == Provider::Claude
                    && matches!(event.data, ProviderEventData::Handshake { .. }));
            if ready && !sent {
                for frame in session.send(TurnInput {
                    message_id: "instructions".into(),
                    text: "Instruction audit. Reply with the marker values from your instruction context. Do not read any files or use any tools. If there are no markers reply NONE.".into(),
                })?.outbound {
                    process.input.send(frame.as_bytes())?;
                }
                sent = true;
            }
            match event.data {
                ProviderEventData::AssistantMessage { text } => output.push_str(&text),
                ProviderEventData::Approval { request_id, .. } => {
                    for frame in session
                        .respond_approval(&request_id, ApprovalDecision::Deny)?
                        .outbound
                    {
                        process.input.send(frame.as_bytes())?;
                    }
                }
                ProviderEventData::Error { message } => {
                    bail!("instruction audit failed: {message}")
                }
                ProviderEventData::TurnFinished { outcome } => {
                    process.control.cancel()?;
                    process.control.wait()?;
                    ensure!(
                        outcome == TurnOutcome::Completed,
                        "instruction audit did not complete"
                    );
                    return Ok(output);
                }
                _ => {}
            }
        }
    }
}

#[test]
#[ignore = "requires authenticated Codex and gpt-6-luna"]
fn codex_loads_configured_fallback_and_linked_worktree_override() -> Result<()> {
    let worktree = Worktree::new()?;
    let excluded_primary = marker(&worktree.primary.join("AGENTS.md"))?;
    let fallback = marker(&worktree.linked.join("CODEX.md"))?;
    let override_marker = marker(&worktree.cwd.join("AGENTS.override.md"))?;
    let excluded_base = marker(&worktree.cwd.join("AGENTS.md"))?;
    let output = invoke(
        Provider::Codex,
        "gpt-6-luna",
        SessionMode::ReadOnly,
        &worktree.cwd,
        &["-c", "project_doc_fallback_filenames=[\"CODEX.md\"]"],
    )?;
    assert!(
        output.contains(&fallback),
        "Codex did not load the configured fallback in the linked root"
    );
    assert!(
        output.contains(&override_marker),
        "Codex did not load the nested override"
    );
    assert!(
        !output.contains(&excluded_base),
        "Codex used the shadowed nested AGENTS.md"
    );
    assert!(
        !output.contains(&excluded_primary),
        "Codex used an untracked primary checkout file"
    );
    Ok(())
}

#[test]
#[ignore = "requires authenticated Antigravity and Gemini 3.8 Flash"]
fn antigravity_discovers_linked_worktree_instructions() -> Result<()> {
    let worktree = Worktree::new()?;
    let excluded_primary = marker(&worktree.primary.join("AGENTS.md"))?;
    let root = marker(&worktree.linked.join("AGENTS.md"))?;
    let gemini = marker(&worktree.cwd.join("GEMINI.md"))?;
    fs::create_dir(worktree.cwd.join(".agents"))?;
    let custom = marker(&worktree.cwd.join(".agents/AGENTS.md"))?;
    let output = invoke(
        Provider::Antigravity,
        "gemini-3.8-flash",
        SessionMode::Plan,
        &worktree.cwd,
        &[],
    )?;
    assert!(
        output.contains(&root),
        "Antigravity did not load workspace root instructions"
    );
    assert!(
        output.contains(&gemini),
        "Antigravity did not load nested GEMINI.md"
    );
    assert!(
        output.contains(&custom),
        "Antigravity did not load .agents/AGENTS.md"
    );
    assert!(
        !output.contains(&excluded_primary),
        "Antigravity used an untracked primary checkout file"
    );
    Ok(())
}

#[test]
#[ignore = "requires authenticated Claude Code and Haiku 5.5"]
fn claude_safe_mode_excludes_linked_worktree_imports() -> Result<()> {
    let worktree = Worktree::new()?;
    let primary = marker(&worktree.primary.join("CLAUDE.local.md"))?;
    let linked = marker(&worktree.linked.join("AGENTS.md"))?;
    fs::write(worktree.linked.join("CLAUDE.md"), "@AGENTS.md")?;
    let local = marker(&worktree.linked.join("CLAUDE.local.md"))?;
    let direct = marker(&worktree.cwd.join("AGENTS.md"))?;
    let output = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.cwd,
        &[],
    )?;
    assert_eq!(
        output.trim(),
        "NONE",
        "Claude did not report an empty instruction audit"
    );
    for forbidden in [primary, linked, local, direct] {
        assert!(
            !output.contains(&forbidden),
            "Claude loaded implicit instructions in safe mode"
        );
    }
    Ok(())
}
