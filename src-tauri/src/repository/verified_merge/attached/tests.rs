use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::repository::{
    documents::{open_for_rename, rename},
    git_path, git_query, inspect,
};

use super::{AttachedMergePlan, AttachedRecovery, file_backup};

struct Fixture {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    common: String,
    old: String,
    new: String,
}

impl Fixture {
    fn new(linked: bool) -> Result<Self> {
        let scratch = tempfile::tempdir()?;
        let main = scratch.path().join("principal");
        fs::create_dir(&main)?;
        git(&main, &["init", "--quiet", "--initial-branch=destination"])?;
        git(&main, &["config", "core.autocrlf", "false"])?;
        fs::write(main.join("edited.txt"), b"old bytes\n")?;
        fs::write(main.join("removed.txt"), b"remove me\n")?;
        fs::write(main.join("untouched.txt"), b"keep me\n")?;
        fs::write(main.join(".gitignore"), b"*.ignored\n")?;
        let old = commit(&main, "old destination")?;
        git(&main, &["switch", "--quiet", "-c", "delivery"])?;
        fs::write(main.join("edited.txt"), b"reviewed bytes\n")?;
        fs::remove_file(main.join("removed.txt"))?;
        fs::create_dir_all(main.join("new/subdir"))?;
        fs::write(main.join("new/subdir/added.txt"), b"approved new file\n")?;
        fs::write(
            main.join("colliding.ignored"),
            b"approved new ignored path\n",
        )?;
        git(&main, &["add", "-f", "--", "colliding.ignored"])?;
        let new = commit(&main, "reviewed changes")?;
        let root = if linked {
            let linked = scratch.path().join("linked");
            git(
                &main,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    linked.to_str().unwrap(),
                    "destination",
                ],
            )?;
            linked
        } else {
            git(&main, &["switch", "--quiet", "destination"])?;
            main
        };
        let root = dunce::canonicalize(root)?;
        let common = inspect(&root)?.git_common_dir;
        Ok(Self {
            _scratch: scratch,
            root,
            common,
            old,
            new,
        })
    }

    fn prepare(&self) -> Result<AttachedMergePlan> {
        AttachedMergePlan::prepare(
            &self.root,
            &self.common,
            "destination",
            &self.old,
            &self.new,
        )
    }

    fn assert_installed(&self) -> Result<()> {
        assert_eq!(fs::read(self.root.join("edited.txt"))?, b"reviewed bytes\n");
        assert_eq!(fs::read(self.root.join("untouched.txt"))?, b"keep me\n");
        assert_eq!(
            fs::read(self.root.join("new/subdir/added.txt"))?,
            b"approved new file\n"
        );
        assert!(!self.root.join("removed.txt").exists());
        assert_eq!(
            git(&self.root, &["write-tree"])?,
            git(
                &self.root,
                &["rev-parse", &format!("{}^{{tree}}", self.new)]
            )?
        );
        Ok(())
    }
}

#[test]
fn applies_and_reconciles_primary_and_linked_destinations() -> Result<()> {
    for linked in [false, true] {
        let fixture = Fixture::new(linked)?;
        fs::write(
            fixture.root.join("private.ignored"),
            b"keep local ignored content",
        )?;
        let plan = fixture.prepare()?;
        assert_eq!(plan.checkout(), fixture.root);
        assert!(!plan.publication_started()?);
        let mut guard = plan.reserve()?;
        assert!(plan.git_dir().join("index.lock").exists());
        assert!(plan.reserve().is_err());
        guard.apply()?;
        guard.verify_updated()?;
        assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"])?, fixture.old);
        assert!(plan.publication_started()?);
        drop(guard);
        fixture.assert_installed()?;
        assert_eq!(
            fs::read(fixture.root.join("private.ignored"))?,
            b"keep local ignored content"
        );

        let resuming: AttachedMergePlan = serde_json::from_slice(&serde_json::to_vec(&plan)?)?;
        let mut resumed = resuming.reserve()?;
        resumed.apply()?;
        resumed.verify_updated()?;
        drop(resumed);
        git(
            &fixture.root,
            &[
                "update-ref",
                "refs/heads/destination",
                &fixture.new,
                &fixture.old,
            ],
        )?;
        git(&fixture.root, &["update-index", "--refresh"])?;
        let published = resuming.reserve()?;
        published.verify_updated()?;
        drop(published);
        assert_eq!(git(&fixture.root, &["status", "--porcelain"])?, "");
        resuming.cleanup_after_receipt()?;
        assert!(!resuming.metadata().exists());
    }
    Ok(())
}

#[test]
fn refuses_dirty_and_colliding_files_without_touching_live_index() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let untouched = fixture.root.join("untouched.txt");
    fs::write(&untouched, b"staged user content")?;
    git(&fixture.root, &["add", "--", "untouched.txt"])?;
    assert!(fixture.prepare().is_err());
    assert_eq!(fs::read(&untouched)?, b"staged user content");
    git(
        &fixture.root,
        &[
            "restore",
            "--source=HEAD",
            "--staged",
            "--worktree",
            "--",
            "untouched.txt",
        ],
    )?;
    let index = PathBuf::from(inspect(&fixture.root)?.git_dir).join("index");
    let original = fs::read(&index)?;
    let collision = fixture.root.join("colliding.ignored");
    fs::write(&collision, b"local ignored content")?;
    assert!(fixture.prepare().is_err());
    assert_eq!(fs::read(&collision)?, b"local ignored content");
    fs::remove_file(&collision)?;
    let plan = fixture.prepare()?;
    fs::write(&collision, b"appeared after prepare")?;
    let mut guard = plan.reserve()?;
    assert!(guard.apply().is_err());
    assert!(!plan.publication_started()?);
    assert_eq!(fs::read(&collision)?, b"appeared after prepare");
    assert_eq!(fs::read(index)?, original);
    assert_eq!(fs::read(fixture.root.join("edited.txt"))?, b"old bytes\n");
    Ok(())
}

#[test]
fn compares_the_pinned_git_blob_but_retains_the_exact_displaced_bytes() -> Result<()> {
    let fixture = Fixture::new(false)?;
    git(&fixture.root, &["config", "core.autocrlf", "true"])?;
    let plan = fixture.prepare()?;
    let position = plan
        .files
        .iter()
        .position(|item| item.path == "edited.txt")
        .unwrap();
    let mut guard = plan.reserve()?;
    guard.apply()?;
    guard.verify_updated()?;
    drop(guard);
    assert_eq!(
        fs::read(fixture.root.join("edited.txt"))?,
        b"reviewed bytes\r\n"
    );
    assert_eq!(
        fs::read(file_backup(&plan.backups(), position))?,
        b"old bytes\n"
    );
    Ok(())
}

#[test]
fn resumes_a_displaced_file_and_stops_at_unknown_edits() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let plan = fixture.prepare()?;
    fs::File::create(plan.metadata().join("started"))?.sync_all()?;
    let position = plan
        .files
        .iter()
        .position(|item| item.path == "edited.txt")
        .unwrap();
    let edited = fixture.root.join("edited.txt");
    let displaced = file_backup(&plan.backups(), position);
    rename(&open_for_rename(&edited)?, &displaced)?;
    assert!(!edited.exists());
    let mut resume = plan.reserve()?;
    resume.apply()?;
    resume.verify_updated()?;
    drop(resume);
    fixture.assert_installed()?;

    fs::write(&edited, b"an independent edit")?;
    let mut conflict = plan.reserve()?;
    let failure = conflict.apply().unwrap_err();
    assert!(failure.downcast_ref::<AttachedRecovery>().is_some());
    assert_eq!(fs::read(&edited)?, b"an independent edit");
    assert_eq!(fs::read(displaced)?, b"old bytes\n");
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"])?, fixture.old);
    Ok(())
}

#[test]
fn a_replaced_index_invalidates_an_unstarted_intent_without_touching_files() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let plan = fixture.prepare()?;
    assert!(plan.unstarted_matches_current()?);
    let index = plan.git_dir().join("index");
    let bytes = fs::read(&index)?;
    let replacement = plan.git_dir().join("alternate-fixture-index");
    fs::write(&replacement, &bytes)?;
    let original = plan.git_dir().join("original-fixture-index");
    fs::rename(&index, original)?;
    fs::rename(&replacement, &index)?;

    assert!(!plan.unstarted_matches_current()?);
    let mut guard = plan.reserve()?;
    assert!(guard.apply().is_err());
    drop(guard);
    assert!(!plan.publication_started()?);
    assert_eq!(fs::read(&index)?, bytes);
    assert_eq!(fs::read(fixture.root.join("edited.txt"))?, b"old bytes\n");
    assert!(fixture.prepare()?.unstarted_matches_current()?);
    Ok(())
}

#[test]
fn does_not_adopt_an_independently_staged_index_after_applying_files() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let plan = fixture.prepare()?;
    let mut guard = plan.reserve()?;
    guard.apply()?;
    drop(guard);
    let untouched = fixture.root.join("untouched.txt");
    fs::write(&untouched, b"staged after update")?;
    git(&fixture.root, &["add", "--", "untouched.txt"])?;
    let mut resumed = plan.reserve()?;
    assert!(
        resumed
            .apply()
            .unwrap_err()
            .downcast_ref::<AttachedRecovery>()
            .is_some()
    );
    assert!(
        resumed
            .verify_updated()
            .unwrap_err()
            .downcast_ref::<AttachedRecovery>()
            .is_some()
    );
    assert_eq!(fs::read(untouched)?, b"staged after update");
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"])?, fixture.old);
    Ok(())
}

fn commit(root: &Path, message: &str) -> Result<String> {
    git(root, &["add", "-A"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "--quiet",
            "-m",
            message,
        ],
    )?;
    git(root, &["rev-parse", "HEAD"])
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    Ok(git_query(&git_path()?, root, args)?.trim().into())
}
