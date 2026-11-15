use std::{ffi::OsStr, fs, path::Path, process::Command};

use anyhow::{Result, ensure};
use omniagent::{repository, store::Store};

fn git(checkout: &Path, args: &[impl AsRef<OsStr>]) -> Result<()> {
    let output = Command::new("git.exe")
        .args(args)
        .current_dir(checkout)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[tokio::test]
async fn task_identity_survives_reopen_and_is_shared_only_by_its_project_worktrees() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let main = fixture.path().join("main");
    let linked = fixture.path().join("linked");
    let other = fixture.path().join("other");
    for directory in [&main, &other] {
        fs::create_dir(directory)?;
        git(directory, &["init", "--quiet"])?;
    }
    git(
        &main,
        &[
            "-c",
            "user.name=OmniAgent Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=.git/disabled-hooks",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "Fixture root",
        ],
    )?;
    git(
        &main,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--quiet"),
            OsStr::new("--detach"),
            linked.as_os_str(),
        ],
    )?;

    let database = fixture.path().join("omniagent.sqlite3");
    let store = Store::open(database.clone())?;
    let main = repository::register(&store, main).await?;
    let task = store
        .create_project_task(main.project_id.clone(), "Review change ☕".into())
        .await?;
    assert!(
        store
            .create_project_task("missing-project".into(), "Unowned task".into())
            .await
            .is_err()
    );
    assert!(
        store
            .create_project_task(main.project_id.clone(), "\n\t ".into())
            .await
            .is_err()
    );
    drop(store);

    let store = Store::open(database)?;
    let linked = repository::register(&store, linked).await?;
    let other = repository::register(&store, other).await?;
    assert_eq!(main.project_id, linked.project_id);
    assert_ne!(main.checkout_id, linked.checkout_id);
    assert_eq!(
        store
            .project_task(linked.project_id, task.id.clone())
            .await?,
        Some(task.clone())
    );
    assert_eq!(store.project_task(other.project_id, task.id).await?, None);
    Ok(())
}
