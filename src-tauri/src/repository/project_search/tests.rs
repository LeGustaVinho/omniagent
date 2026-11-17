use std::{fs, process::Command, time::Instant};

use serde_json::json;

use super::*;

fn git(root: &Path, arguments: &[&str]) -> Result<()> {
    let git = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()?;
    ensure!(
        git.status.success(),
        "search fixture Git command failed: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    Ok(())
}

#[test]
fn match_records_are_checkout_bound_and_previews_keep_the_literal_hit() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let root = dunce::canonicalize(fixture.path())?;
    fs::create_dir(root.join("docs"))?;
    fs::write(root.join("docs/note with spaces.txt"), "match")?;
    let records = [
        r".\docs\note with spaces.txt",
        r"..\outside\note.txt",
        r"C:\outside\note.txt",
        ".git/config",
        "docs/missing.txt",
        "docs/note with spaces.txt:stream",
    ];
    let text = "é🚀".repeat(650) + " a.*b " + &"ç".repeat(450) + "\r\n";
    let offset = text.find("a.*b").unwrap();
    let mut newline_json = Vec::new();
    for path in records {
        serde_json::to_writer(
            &mut newline_json,
            &json!({ "type": "match", "data": {
                "path": { "text": path }, "lines": { "text": text }, "line_number": 42,
                "submatches": [{ "start": offset }],
            }}),
        )?;
        newline_json.push(b'\n');
    }
    newline_json.extend(b"{incomplete");
    let (matches, truncated) = read_matches(&root, &newline_json, None)?;
    assert!(!truncated);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].path, "docs/note with spaces.txt");
    assert_eq!(matches[0].line, 42);
    assert_eq!(matches[0].columns, [1952]);
    assert_eq!(matches[0].occurrences, 1);
    assert!(matches[0].line_text.is_none());
    assert!(matches[0].excerpt.contains("a.*b"));
    assert_eq!(matches[0].excerpt.chars().count(), 322);
    assert!(matches[0].excerpt.starts_with('…') && matches[0].excerpt.ends_with('…'));
    let (outside_scope, _) = read_matches(
        &root,
        &newline_json,
        Some((Path::new("docs/other.txt"), true)),
    )?;
    assert!(outside_scope.is_empty());
    assert!(validate_query("a.*b").is_ok());
    assert!(validate_query("  literal spaces  ").is_ok());
    assert!(validate_query("line one\nline two").is_err());
    assert!(validate_query(&"a".repeat(MAX_QUERY_UNITS + 1)).is_err());
    Ok(())
}

#[test]
fn windows_search_process_has_a_deadline_and_bounded_output() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let cwd = dunce::canonicalize(fixture.path())?;
    let powershell =
        PathBuf::from(env::var_os("SystemRoot").context("Windows system root not found")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let run = |command: &str, time, output| {
        run_bounded(
            &powershell,
            &cwd,
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ]
            .map(OsStr::new),
            time,
            output,
        )
    };
    let start = Instant::now();
    let timed = run("Start-Sleep -Seconds 20", Duration::from_millis(200), 128)?;
    assert!(timed.truncated);
    assert!(matches!(timed.outcome, TerminalOutcome::Cancelled(_)));
    assert!(start.elapsed() < Duration::from_secs(10));
    let limited = run(
        "[Console]::WriteLine('a' * 50000); Start-Sleep -Seconds 20",
        Duration::from_secs(4),
        128,
    )?;
    assert_eq!(limited.stdout.len(), 128);
    assert!(limited.truncated);
    Ok(())
}

#[tokio::test]
#[ignore = "requires real ripgrep.exe on PATH"]
async fn real_search_respects_ignores_literals_scope_and_persisted_executable() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let primary = fixture.path().join("primary");
    let root = fixture.path().join("linked checkout");
    let outside = fixture.path().join("outside");
    fs::create_dir(&primary)?;
    git(&primary, &["init", "--quiet"])?;
    git(
        &primary,
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
    git(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            root.to_str().context("linked path is not UTF-8")?,
        ],
    )?;
    fs::write(primary.join("main-only.txt"), "a.*b primary checkout only")?;
    fs::create_dir(root.join("src"))?;
    fs::create_dir(&outside)?;
    fs::write(root.join(".gitignore"), "excluded-by-git.txt\n")?;
    fs::write(root.join(".ignore"), "excluded-by-rg.txt\n")?;
    fs::write(root.join("excluded-by-git.txt"), "a.*b")?;
    fs::write(root.join("excluded-by-rg.txt"), "a.*b")?;
    fs::write(root.join(".hidden.txt"), "a.*b")?;
    fs::write(
        root.join("src/ação file.txt"),
        "aABCb\r\n  A.*b literal\r\n--version is file text\r\n",
    )?;
    fs::write(
        outside.join("secret.txt"),
        "a.*b private outside the checkout",
    )?;
    let junction = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(root.join("escape"))
        .arg(&outside)
        .output()?;
    ensure!(
        junction.status.success(),
        "fixture junction failed: {}",
        String::from_utf8_lossy(&junction.stderr)
    );
    assert!(root.join("escape/secret.txt").is_file());

    let configured = fixture.path().join("selected-rg.exe");
    fs::copy(
        discover_ripgrep().context("a real ripgrep executable is required")?,
        &configured,
    )?;
    let database = fixture.path().join("profile.sqlite");
    let store = Store::open(database.clone())?;
    let primary = crate::repository::register(&store, primary).await?;
    let linked = crate::repository::register(&store, root.clone()).await?;
    assert_eq!(primary.project_id, linked.project_id);
    assert_ne!(primary.checkout_id, linked.checkout_id);
    let checkout = linked.checkout_id;
    let default = store.ripgrep_status().await?;
    assert!(default.configured_executable.is_none());
    assert!(
        default
            .version
            .as_deref()
            .is_some_and(|version| version.starts_with("ripgrep "))
    );
    let status = store
        .set_ripgrep_executable(Some(configured.clone()))
        .await?;
    assert_eq!(
        status.configured_executable,
        Some(dunce::canonicalize(&configured)?)
    );
    assert!(
        store
            .set_ripgrep_executable(Some(PathBuf::from("rg.exe")))
            .await
            .is_err()
    );
    drop(store);
    let store = Store::open(database)?;
    assert_eq!(store.ripgrep_status().await?.executable, status.executable);
    assert!(
        store
            .search_repository_files("unregistered".into(), "literal".into())
            .await
            .is_err()
    );

    let result = store
        .search_repository_files(checkout.clone(), "a.*b".into())
        .await?;
    let ProjectSearch::Results { matches, truncated } = result else {
        anyhow::bail!("fixture search unavailable: {result:?}")
    };
    assert!(!truncated);
    assert_eq!(
        matches,
        [ProjectSearchMatch {
            path: "src/ação file.txt".into(),
            line: 2,
            excerpt: "  A.*b literal".into(),
            columns: vec![3],
            occurrences: 1,
            line_text: Some("  A.*b literal".into()),
        }]
    );
    fs::write(root.join("src/sibling.txt"), "a.*b")?;
    let exact = store
        .search_repository_file(checkout.clone(), "a.*b".into(), "src/ação file.txt".into())
        .await?;
    assert!(
        matches!(exact, ProjectSearch::Results { matches: scoped, truncated: false } if scoped == matches)
    );
    assert!(
        store
            .search_repository_file(checkout.clone(), "a.*b".into(), "escape/secret.txt".into())
            .await
            .is_err()
    );
    let primary_result = store
        .search_repository_files(primary.checkout_id, "a.*b".into())
        .await?;
    assert!(
        matches!(primary_result, ProjectSearch::Results { matches, truncated: false }
        if matches.len() == 1 && matches[0].path == "main-only.txt" && matches[0].line == 1)
    );
    let result = store
        .search_repository_files(checkout.clone(), "--version".into())
        .await?;
    assert!(
        matches!(result, ProjectSearch::Results { matches, truncated: false } if matches.len() == 1 && matches[0].line == 3)
    );
    let result = store
        .search_repository_files(checkout.clone(), "absent token".into())
        .await?;
    assert!(
        matches!(result, ProjectSearch::Results { matches, truncated: false } if matches.is_empty())
    );

    fs::write(
        root.join("src/many.txt"),
        "many identical hits\n".repeat(RESULT_LIMIT + 2),
    )?;
    let result = store
        .search_repository_files(checkout.clone(), "many identical hits".into())
        .await?;
    assert!(
        matches!(result, ProjectSearch::Results { matches, truncated: true } if matches.len() == RESULT_LIMIT)
    );
    fs::remove_file(configured)?;
    let result = store
        .search_repository_files(checkout, "a.*b".into())
        .await?;
    assert!(
        matches!(result, ProjectSearch::Unavailable { reason } if reason.contains("configured ripgrep"))
    );
    let reset = store.set_ripgrep_executable(None).await?;
    assert!(reset.configured_executable.is_none() && reset.executable.is_some());
    Ok(())
}
