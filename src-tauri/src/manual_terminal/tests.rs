use std::{fs, process::Command, time::Duration};

use anyhow::bail;
use omniagent::repository::{
    self,
    documents::{RepositoryDocumentOpen, RepositoryDocumentSave},
    git_review::{GitDiffSource, GitFileSide, GitPathStatus},
    project_search::{ProjectSearch, ProjectSearchMatch},
};
use tokio::sync::mpsc;
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

use super::*;
use std::os::windows::io::FromRawHandle;

fn git(root: &Path, arguments: &[&str]) -> Result<()> {
    let result = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()?;
    ensure!(
        result.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

struct TerminalOutput {
    events: mpsc::UnboundedReceiver<ManualTerminalEvent>,
    bytes: Vec<u8>,
    cursor_answered: usize,
}

impl TerminalOutput {
    async fn wait_for<T>(
        &mut self,
        session: &ManualSession,
        ready: impl Fn(&[u8]) -> Option<T>,
    ) -> Result<T> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(value) = ready(&self.bytes) {
                    return Ok(value);
                }
                match self
                    .events
                    .recv()
                    .await
                    .context("the terminal stream closed")?
                {
                    ManualTerminalEvent::Output { data } => {
                        self.bytes.extend(STANDARD.decode(data)?);
                        let queries = self
                            .bytes
                            .windows(4)
                            .filter(|window| *window == b"\x1b[6n")
                            .count();
                        for _ in self.cursor_answered..queries {
                            session.write("\x1b[1;1R".into())?;
                        }
                        self.cursor_answered = queries;
                    }
                    ManualTerminalEvent::Exited { exit_code } => {
                        bail!("PowerShell exited early with code {exit_code}")
                    }
                    ManualTerminalEvent::Error { message } => {
                        bail!("PowerShell output failed: {message}")
                    }
                }
            }
        })
        .await
        .with_context(|| {
            format!(
                "waiting for PowerShell output: {:?}",
                String::from_utf8_lossy(&self.bytes)
            )
        })?
    }

    async fn exited(&mut self) -> Result<u32> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match self
                    .events
                    .recv()
                    .await
                    .context("the terminal stream closed without an exit event")?
                {
                    ManualTerminalEvent::Output { data } => {
                        self.bytes.extend(STANDARD.decode(data)?)
                    }
                    ManualTerminalEvent::Exited { exit_code } => return Ok(exit_code),
                    ManualTerminalEvent::Error { message } => {
                        bail!("PowerShell output failed: {message}")
                    }
                }
            }
        })
        .await?
    }
}

fn contains(bytes: &[u8], needle: &str) -> Option<()> {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
        .then_some(())
}

async fn open_terminal(
    terminals: &ManualTerminals,
    store: &Store,
    checkout_id: String,
) -> Result<(ManualTerminalInfo, Arc<ManualSession>, TerminalOutput)> {
    let (send, events) = mpsc::unbounded_channel();
    let info = terminals
        .start(
            store,
            checkout_id,
            None,
            terminal_size(100, 28)?,
            None,
            move |notice| send.send(notice).is_ok(),
        )
        .await?;
    let session = terminals
        .find(&info.checkout_id, &info.terminal_id)?
        .context("manual terminal was not registered")?;
    let mut output = TerminalOutput {
        events,
        bytes: Vec::new(),
        cursor_answered: 0,
    };
    output
        .wait_for(&session, |bytes| contains(bytes, "PS "))
        .await?;
    Ok((info, session, output))
}

#[tokio::test]
async fn powershell_uses_the_verified_worktree_and_streams_input_output_resize_and_exit()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("repository");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet"])?;
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initialize fixture",
        ],
    )?;
    let worktree = fixture.path().join("linked worktree");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().context("worktree path is not UTF-8")?,
        ],
    )?;
    fs::write(
        worktree.join("powershell.exe"),
        b"not the system executable",
    )?;
    let database = fixture.path().join("profile.sqlite");
    let store = Store::open(database.clone())?;
    let primary = repository::register(&store, root).await?;
    let linked = repository::register(&store, worktree.clone()).await?;
    assert_eq!(linked.project_id, primary.project_id);
    assert_ne!(linked.checkout_id, primary.checkout_id);
    let terminals = ManualTerminals::default();
    assert!(
        terminals
            .start(
                &store,
                "unknown-checkout".into(),
                None,
                terminal_size(80, 24)?,
                None,
                |_| false
            )
            .await
            .is_err()
    );
    assert!(terminal_size(0, 24).is_err());
    let (info, session, mut output) = open_terminal(&terminals, &store, linked.checkout_id).await?;
    let child_directory = worktree.join("child folder");
    fs::create_dir(&child_directory)?;
    session.write("Set-Location -LiteralPath 'child folder'\r".into())?;
    let observed = format!("]633;P;OmniAgentCwd={};{}", info.terminal_id,
        STANDARD.encode(dunce::canonicalize(&child_directory)?.to_str().context("fixture path")?));
    output.wait_for(&session, |bytes| contains(bytes, &observed)).await?;
    session.write("Set-Location ..\r".into())?;
    assert!(
        terminals
            .find(&primary.checkout_id, &info.terminal_id)
            .is_err()
    );
    session.write(
        "Write-Output ('OMNI'+'_CWD='+$PWD.Path); Write-Output ('OMNI'+'_UTF='+[char]0x00E9)\r"
            .into(),
    )?;
    let cwd = format!("OMNI_CWD={}", dunce::canonicalize(&worktree)?.display());
    output
        .wait_for(&session, |bytes| contains(bytes, &cwd))
        .await?;
    output
        .wait_for(&session, |bytes| contains(bytes, "OMNI_UTF=é"))
        .await?;

    session.resize(terminal_size(112, 34)?)?;
    session.write(
        "Write-Output ('OMNI'+'_SIZE='+[Console]::WindowWidth+'x'+[Console]::WindowHeight)\r"
            .into(),
    )?;
    output
        .wait_for(&session, |bytes| contains(bytes, "OMNI_SIZE=112x34"))
        .await?;
    let (split_info, split, mut split_output) =
        open_terminal(&terminals, &store, info.checkout_id.clone()).await?;
    assert_ne!(split_info.terminal_id, info.terminal_id);
    session.write("exit 7\r".into())?;
    assert_eq!(output.exited().await?, 7);
    assert!(
        terminals
            .find(&info.checkout_id, &info.terminal_id)?
            .is_none()
    );
    split.write("Write-Output ('OMNI'+'_SPLIT_STILL_LIVE')\r".into())?;
    split_output
        .wait_for(&split, |bytes| contains(bytes, "OMNI_SPLIT_STILL_LIVE"))
        .await?;
    split.write("exit 0\r".into())?;
    assert_eq!(split_output.exited().await?, 0);
    let db = rusqlite::Connection::open(database)?;
    let managed: (u32, u32) = db.query_row(
        "SELECT (SELECT count(*) FROM jobs), (SELECT count(*) FROM runs)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(managed, (0, 0));
    Ok(())
}

#[tokio::test]
async fn closing_a_manual_terminal_terminates_its_power_shell_descendant() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("repository");
    fs::create_dir(&root)?;
    git(&root, &["init", "--quiet"])?;
    let store = Store::open(fixture.path().join("profile.sqlite"))?;
    let checkout = repository::register(&store, root).await?.checkout_id;
    let terminals = ManualTerminals::default();
    let (info, session, mut output) = open_terminal(&terminals, &store, checkout).await?;
    session.write(concat!(
        "$child = Start-Process (Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\\v1.0\\powershell.exe') ",
        r#"-ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 120"' -NoNewWindow -PassThru;"#,
        "Write-Output ('OMNI'+'_CHILD='+$child.Id)\r",
    ).into())?;
    let emitted_pid = regex::bytes::Regex::new(r"OMNI_CHILD=([0-9]+)[^0-9]")?;
    let pid = output
        .wait_for(&session, |bytes| {
            std::str::from_utf8(emitted_pid.captures(bytes)?.get(1)?.as_bytes())
                .ok()?
                .parse::<u32>()
                .ok()
        })
        .await?;
    let child = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    ensure!(!child.is_null(), "expected a live descendant process");
    // Hold the original process handle before termination so PID reuse cannot satisfy this proof.
    let child = unsafe { OwnedHandle::from_raw_handle(child) };
    assert_ne!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), 0) },
        WAIT_OBJECT_0
    );
    terminals.close(&info.checkout_id, &info.terminal_id)?;
    output.exited().await?;
    assert_eq!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), 5_000) },
        WAIT_OBJECT_0
    );
    assert!(
        terminals
            .find(&info.checkout_id, &info.terminal_id)?
            .is_none()
    );
    terminals.close(&info.checkout_id, &info.terminal_id)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires real ripgrep.exe on PATH"]
async fn ide_edit_search_terminal_and_git_review_share_the_managed_worktree() -> Result<()> {
    const PATH: &str = "src/build settings.txt";
    const INITIAL: &str = "build configuration\noriginal value\n";
    const MATCH: &str = "ide .* linked smoke";
    let edited = format!("build configuration\n{MATCH}\n");
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("repository");
    fs::create_dir_all(root.join("src"))?;
    git(&root, &["init", "--quiet"])?;
    git(&root, &["config", "core.autocrlf", "false"])?;
    fs::write(root.join(PATH), INITIAL)?;
    git(&root, &["add", "--", PATH])?;
    git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "Initialize IDE fixture",
        ],
    )?;
    let store = Store::open(fixture.path().join("profile.sqlite"))?;
    let primary = repository::register(&store, root.clone()).await?;
    let task = store
        .create_project_task(primary.project_id.clone(), "IDE smoke".into())
        .await?;
    let linked = repository::provision_task_worktree(
        &store,
        fixture.path().join("managed worktrees"),
        primary.project_id.clone(),
        task.id.clone(),
        primary.checkout_id.clone(),
    )
    .await?;
    let checkout = linked.checkout_id.clone();
    assert_eq!(linked.project_id, primary.project_id);
    assert_ne!(checkout, primary.checkout_id);
    let RepositoryDocumentOpen::Document { document } = store
        .open_repository_source_file(checkout.clone(), PATH.into())
        .await?
    else {
        bail!("IDE did not open the source document");
    };
    assert_eq!(document.revision.checkout_id, checkout);
    assert_eq!(
        document.canonical_path,
        dunce::canonicalize(linked.root.join(PATH))?
    );
    assert_eq!(document.markdown, INITIAL);

    let (held, entered) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let writer = tokio::spawn({
        let store = store.clone();
        let checkout = checkout.clone();
        async move {
            repository::with_managed_worktree_writer(
                &store,
                primary.project_id,
                checkout,
                task.id,
                move |_| {
                    held.send(())
                        .map_err(|()| anyhow::anyhow!("writer fixture observer closed"))?;
                    released.recv_timeout(Duration::from_secs(20))?;
                    Ok(())
                },
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(20), entered).await??;
    let blocked = store
        .save_repository_source_file(
            checkout.clone(),
            PATH.into(),
            document.revision.id.clone(),
            edited.clone(),
        )
        .await;
    release.send(())?;
    writer.await??;
    let error = blocked.expect_err("IDE save bypassed the active managed writer");
    assert!(
        error
            .to_string()
            .contains("worktree reserved by app operation"),
        "{error:#}"
    );
    assert_eq!(fs::read_to_string(linked.root.join(PATH))?, INITIAL);
    let saved = store
        .save_repository_source_file(
            checkout.clone(),
            PATH.into(),
            document.revision.id,
            edited.clone(),
        )
        .await?;
    let RepositoryDocumentSave::Saved { document } = saved else {
        bail!("IDE save failed after writer release: {saved:?}");
    };
    assert_eq!(document.markdown, edited);

    let search = store
        .search_repository_files(checkout.clone(), MATCH.into())
        .await?;
    let ProjectSearch::Results {
        matches,
        truncated: false,
    } = search
    else {
        bail!("real ripgrep did not return complete results: {search:?}");
    };
    assert_eq!(
        matches,
        [ProjectSearchMatch {
            path: PATH.into(),
            line: 2,
            excerpt: MATCH.into(),
            columns: vec![1],
            occurrences: 1,
            line_text: Some(MATCH.into()),
        }]
    );
    let primary_search = store
        .search_repository_files(primary.checkout_id.clone(), MATCH.into())
        .await?;
    assert!(
        matches!(primary_search, ProjectSearch::Results { matches, truncated: false } if matches.is_empty())
    );

    let terminals = ManualTerminals::default();
    let (info, session, mut output) = open_terminal(&terminals, &store, checkout.clone()).await?;
    assert_eq!(info.checkout_id, checkout);
    let expected_root = linked
        .root
        .to_str()
        .context("linked root is not UTF-8")?
        .replace('\'', "''");
    session.write(format!(
        "Write-Output ('OMNI'+'_IDE_CWD='+[string]($PWD.Path -ieq '{expected_root}'))\r"
    ))?;
    output
        .wait_for(&session, |bytes| contains(bytes, "OMNI_IDE_CWD=True"))
        .await?;
    session.write(format!(
        "Write-Output ('OMNI'+'_IDE_EDIT='+(Get-Content -LiteralPath '{PATH}' -Tail 1))\r"
    ))?;
    output
        .wait_for(&session, |bytes| {
            contains(bytes, &format!("OMNI_IDE_EDIT={MATCH}"))
        })
        .await?;
    session.write("exit 0\r".into())?;
    assert_eq!(output.exited().await?, 0);

    let review = store.git_checkout_review(checkout.clone()).await?;
    assert!(review.managed);
    assert_eq!(review.checkout_id, checkout);
    assert_eq!(PathBuf::from(&review.root), linked.root);
    assert_eq!(
        review.changes,
        [GitPathStatus {
            path: PATH.into(),
            staged: None,
            working: Some('M')
        }]
    );
    let diff = store
        .git_checkout_diff(checkout, PATH.into(), GitDiffSource::Working)
        .await?;
    assert_eq!(diff.path, PATH);
    assert_eq!(
        diff.original,
        GitFileSide::Text {
            content: INITIAL.into()
        }
    );
    assert_eq!(diff.modified, GitFileSide::Text { content: edited });
    assert!(
        store
            .git_checkout_review(primary.checkout_id)
            .await?
            .changes
            .is_empty()
    );
    assert_eq!(fs::read_to_string(root.join(PATH))?, INITIAL);
    Ok(())
}
