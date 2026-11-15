use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use omniagent::{
    conversations::SessionScope,
    dispatch::NativeHistory,
    providers::{
        ApprovalDecision, Provider, ProviderEventData, SessionIntent, SessionMode, SessionRequest,
        TurnInput, TurnOutcome,
        profiles::{ConfiguredExecutable, probe_cli},
        runtime::start_managed_turn,
    },
    repository,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
};
use uuid::Uuid;

mod support;

const INSTRUCTION_AUDIT: &str = "Instruction audit. Reply with the marker values from your instruction context. Do not read any files or use any tools. If there are no markers reply NONE.";

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
    prompt: &str,
    configure: impl FnOnce(Vec<OsString>) -> Vec<OsString>,
) -> Result<(String, Vec<ProviderEventData>)> {
    let adapter = provider.adapter();
    let executable = adapter.discover(None)?;
    let mut session = adapter.create_session(SessionRequest {
        checkout: cwd.to_path_buf(),
        intent: SessionIntent::Start,
        mode,
        model: Some(model.into()),
        effort: None,
    })?;
    let arguments = configure(session.arguments());
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
    let mut observations = Vec::new();
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
                for frame in session
                    .send(TurnInput {
                        message_id: "instructions".into(),
                        text: prompt.into(),
                    })?
                    .outbound
                {
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
                    return Ok((output, observations));
                }
                evidence @ (ProviderEventData::ContextRead { .. }
                | ProviderEventData::SessionCustomizationsReported { .. }
                | ProviderEventData::SkillUsed { .. }) => observations.push(evidence),
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
    let (output, _) = invoke(
        Provider::Codex,
        "gpt-6-luna",
        SessionMode::ReadOnly,
        &worktree.cwd,
        INSTRUCTION_AUDIT,
        |args| {
            ["-c", "project_doc_fallback_filenames=[\"CODEX.md\"]"]
                .map(OsString::from)
                .into_iter()
                .chain(args)
                .collect()
        },
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
    let (output, _) = invoke(
        Provider::Antigravity,
        "gemini-3.8-flash",
        SessionMode::Plan,
        &worktree.cwd,
        INSTRUCTION_AUDIT,
        std::convert::identity,
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
    let (output, _) = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.cwd,
        INSTRUCTION_AUDIT,
        std::convert::identity,
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

#[test]
#[ignore = "requires authenticated Codex and gpt-6-luna"]
fn codex_invokes_the_skill_from_the_linked_checkout() -> Result<()> {
    let worktree = Worktree::new()?;
    let primary = support::write_skill(&worktree.primary, ".agents/skills")?;
    let linked = support::write_skill(&worktree.linked, ".agents/skills")?;
    let (answer, _) = invoke(
        Provider::Codex,
        "gpt-6-luna",
        SessionMode::ReadOnly,
        &worktree.cwd,
        "$omniagent-skill-fixture Follow the explicitly named skill and return its exact marker.",
        std::convert::identity,
    )?;
    assert!(
        answer.contains(&linked) && !answer.contains(&primary),
        "unexpected Codex skill result: {answer}"
    );
    Ok(())
}

#[test]
#[ignore = "requires authenticated Antigravity and Gemini 3.8 Flash"]
fn antigravity_invokes_and_reads_the_skill_from_the_linked_checkout() -> Result<()> {
    let worktree = Worktree::new()?;
    let primary = support::write_skill(&worktree.primary, ".agents/skills")?;
    let linked = support::write_skill(&worktree.linked, ".agents/skills")?;
    let (answer, events) = invoke(
        Provider::Antigravity,
        "gemini-3.8-flash",
        SessionMode::Plan,
        &worktree.cwd,
        "/omniagent-skill-fixture",
        std::convert::identity,
    )?;
    assert!(
        answer.contains(&linked) && !answer.contains(&primary),
        "unexpected Antigravity skill result: {answer}"
    );
    assert!(events.iter().any(|event| match event {
        ProviderEventData::ContextRead { path, tool } if tool == "read_file" => {
            dunce::canonicalize(path.trim_start_matches('/')).is_ok_and(|path| {
                path == worktree
                    .linked
                    .join(".agents/skills/omniagent-skill-fixture/SKILL.md")
            })
        }
        _ => false,
    }));
    Ok(())
}

fn claude_native_fixture(mut arguments: Vec<OsString>) -> Vec<OsString> {
    arguments.retain(|argument| argument != "--safe-mode");
    let sources = arguments
        .iter()
        .position(|arg| arg == "--setting-sources")
        .expect("setting sources");
    arguments[sources + 1] = "project".into();
    arguments.extend(["--settings".into(), r#"{"disableAllHooks":true,"autoMemoryEnabled":false,"enabledPlugins":{"cc-plugin-agents-md@builtin":true},"pluginConfigs":{"cc-plugin-agents-md@builtin":{"options":{"instructionFiles":"claude-md-or-agents-md"}}}}"#.into()]);
    arguments
}

#[test]
#[ignore = "requires authenticated Claude Code v2.1.285+ with Haiku 5.5"]
fn claude_resolves_direct_agents_and_the_main_checkout_skill_fallback() -> Result<()> {
    let worktree = Worktree::new()?;
    let primary_instructions = marker(&worktree.primary.join("CLAUDE.md"))?;
    let shared = marker(&worktree.linked.join("AGENTS.md"))?;
    let nested = marker(&worktree.cwd.join("AGENTS.md"))?;
    let primary_skill = support::write_skill(&worktree.primary, ".claude/skills")?;
    let (direct, _) = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.cwd,
        INSTRUCTION_AUDIT,
        claude_native_fixture,
    )?;
    assert!(
        direct.contains(&shared)
            && direct.contains(&nested)
            && !direct.contains(&primary_instructions),
        "unexpected Claude direct instruction result: {direct}"
    );

    let claude_file = worktree.linked.join("CLAUDE.md");
    let claude = marker(&claude_file)?;
    let text = fs::read_to_string(&claude_file)?;
    fs::write(claude_file, format!("@AGENTS.md\n{text}"))?;
    // Launch from the linked root so the import target is inside the CLI's working directory.
    let (import, _) = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.linked,
        INSTRUCTION_AUDIT,
        claude_native_fixture,
    )?;
    assert!(
        import.contains(&claude)
            && import.contains(&shared)
            && !import.contains(&nested)
            && !import.contains(&primary_instructions),
        "unexpected Claude import result: {import}; expected {claude} and {shared}"
    );

    let qualified_prompt = "/omniagent-skill-fixture";
    let (inherited, _) = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.linked,
        qualified_prompt,
        claude_native_fixture,
    )?;
    assert!(
        inherited.contains(&primary_skill),
        "unexpected Claude inherited skill result: {inherited}"
    );
    let linked_skill = support::write_skill(&worktree.linked, ".claude/skills")?;
    let (local, _) = invoke(
        Provider::Claude,
        "claude-haiku-5-5",
        SessionMode::ReadOnly,
        &worktree.linked,
        qualified_prompt,
        claude_native_fixture,
    )?;
    assert!(
        local.contains(&linked_skill) && !local.contains(&primary_skill),
        "unexpected Claude local skill result: {local}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires authenticated Claude Code with the restricted Haiku 5.5 profile"]
async fn managed_linked_worktree_records_inherited_skill_candidates_and_refreshes_on_resume()
-> Result<()> {
    let worktree = Worktree::new()?;
    let inherited_marker = support::write_skill(&worktree.primary, ".claude/skills")?;
    let primary_skill = worktree
        .primary
        .join(".claude/skills/omniagent-skill-fixture/SKILL.md");
    let primary_markdown = worktree.primary.join("CLAUDE.md");
    let primary_marker = marker(&primary_markdown)?;
    let imported = worktree.linked.join("AGENTS.md");
    let imported_marker = marker(&imported)?;
    fs::write(worktree.linked.join("CLAUDE.md"), "@AGENTS.md")?;

    let store = Store::open(worktree._temporary.path().join("fixture.sqlite"))?;
    let checkout = repository::register(&store, worktree.cwd.clone()).await?;
    let conversation = store
        .create_conversation(checkout.checkout_id, "Linked sources".into())
        .await?;
    let cli = probe_cli(ConfiguredExecutable {
        provider: Provider::Claude,
        executable: None,
    })?;
    let scope = SessionScope {
        provider: Provider::Claude,
        profile_id: "linked-instructions-fixture".into(),
        account_binding: Some("vendor-test-account".into()),
        checkout: worktree.cwd.clone(),
        executable: cli.executable.context("no Claude executable")?.into(),
        cli_version: cli.version.context("no Claude CLI version")?,
        runner: DirectWindowsRunner.identity().into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("claude-haiku-5-5".into()),
        effort: None,
    };
    let mut previous_inventory = None;
    for resumed in [false, true] {
        let candidate = if resumed {
            support::write_skill(&worktree.linked, ".claude/skills")?;
            worktree
                .linked
                .join(".claude/skills/omniagent-skill-fixture/SKILL.md")
        } else {
            primary_skill.clone()
        };
        let current = store.conversation(conversation.id.clone()).await?;
        let turn = store.prepare_agent_attempt(current.id, current.context_id, scope.clone(),
            "/omniagent-skill-fixture\nInstruction audit. Include all markers from the supplied repository instructions. Do not use tools.".into()).await?;
        let run_id = turn.run.id.clone();
        let status = tokio::time::timeout(
            Duration::from_secs(60),
            start_managed_turn(store.clone(), Arc::new(DirectWindowsRunner), turn, None).wait(),
        )
        .await??;
        assert_eq!(status, RunStatus::Succeeded);
        let dispatch = store
            .agent_dispatch(conversation.id.clone(), run_id.clone())
            .await?
            .context("missing dispatch")?;
        let snapshot = dispatch.snapshot;
        assert_eq!(
            matches!(
                snapshot.native_history,
                NativeHistory::ProviderCursor { .. }
            ),
            resumed
        );
        let skills: Vec<_> = snapshot
            .filesystem_skills
            .iter()
            .filter(|entry| entry.name == "omniagent-skill-fixture")
            .collect();
        assert_eq!(skills.len(), 1);
        let skill = skills[0];
        assert_eq!(skill.path, candidate);
        assert_eq!(
            skill.scope.as_deref(),
            Some(if resumed { "repo" } else { "primary_checkout" })
        );
        assert!(skill.revision.is_some());
        assert_eq!(
            snapshot
                .requested_skill
                .context("missing skill request")?
                .matching_ids,
            [skill.id.clone()]
        );
        assert!(!snapshot.reported_skills.unwrap().reported);
        let plugins = snapshot
            .plugin_inventory
            .context("missing plugin inventory")?;
        assert_ne!(
            previous_inventory.replace(plugins.id.clone()),
            Some(plugins.id)
        );
        let instructions = snapshot.instructions.context("missing instructions")?;
        assert_eq!(instructions.primary_root, worktree.primary);
        assert!(
            instructions
                .sources
                .iter()
                .any(|source| source.path == imported
                    && source.imported_from.is_some()
                    && source.revision.is_some())
        );
        let projected = snapshot.scoped_launch.context("missing scoped launch")?;
        assert!(
            projected
                .instruction_sources
                .iter()
                .all(|source| source.path.starts_with(&worktree.linked))
        );
        let messages = store.conversation_messages(conversation.id.clone()).await?;
        let answer = &messages.last().context("missing answer")?.text;
        assert!(answer.contains(&imported_marker));
        assert!(!answer.contains(&primary_marker) && !answer.contains(&inherited_marker));
        let events = store
            .agent_observations(conversation.id.clone(), run_id, 0)
            .await?;
        assert!(events.iter().any(|entry| matches!(
            &entry.event.data,
            ProviderEventData::SessionCustomizationsReported {
                plugins: Some(_),
                ..
            }
        )));
        assert!(
            events
                .iter()
                .all(|entry| !matches!(entry.event.data, ProviderEventData::SkillUsed { .. }))
        );
    }
    Ok(())
}
