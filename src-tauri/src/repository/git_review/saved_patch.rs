use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;

use crate::{
    protection,
    repository::{
        AttemptWorktreeEvidence, CandidateRevision, checkout_items::validate_name,
        documents::file_identity, verification::materialize::ScratchGit,
    },
    store::Store,
};

use super::{
    GitReviewRevision, LiveReview, ReviewScope, lock_directories, lock_git_file,
    reviewed_files::{ReviewedFile, ReviewedFiles, open_target},
    validate_path, with_managed_worktree_writer,
};

const MAX_PATCH: usize = 16 * 1024 * 1024;
const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: usize = 24 * 1024 * 1024;
const MAX_PATHS: usize = 256;
const MAX_TEXT_PREVIEW: usize = 512 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PatchPart {
    Committed,
    Staged,
    Working,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPatchRequest {
    project_id: String,
    run_id: String,
    capture_revision: String,
    checkout_id: String,
    part: PatchPart,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PreviewFile {
    Missing,
    Text { content: String },
    Bytes { size: usize, sha256: String },
}

impl PreviewFile {
    fn for_bytes(bytes: Option<&[u8]>) -> Self {
        let Some(bytes) = bytes else {
            return Self::Missing;
        };
        if bytes.len() <= MAX_TEXT_PREVIEW
            && let Ok(text) = std::str::from_utf8(bytes)
            && !text.contains('\0')
        {
            return Self::Text {
                content: text.into(),
            };
        }
        Self::Bytes {
            size: bytes.len(),
            sha256: digest(bytes),
        }
    }
}

#[derive(Serialize)]
struct PreviewEntry {
    path: String,
    before: PreviewFile,
    after: PreviewFile,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPatchPreview {
    snapshot: String,
    destination: String,
    revision: GitReviewRevision,
    files: Vec<PreviewEntry>,
}

impl Store {
    pub async fn preview_saved_patch(
        &self,
        request: SavedPatchRequest,
    ) -> Result<SavedPatchPreview> {
        let scope = self.git_review_scope(request.checkout_id.clone()).await?;
        ensure!(
            scope.project == request.project_id,
            "Select a registered worktree in the recorded attempt's project."
        );
        let (request, capture) = self
            .execute(move |db| {
                let capture = saved_capture(db, &request)?;
                Ok((request, capture))
            })
            .await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let patch = prepare(&scope, &review, &request, &capture)?;
            review.verify()?;
            Ok(SavedPatchPreview {
                snapshot: patch.snapshot,
                destination: scope.root.to_string_lossy().into_owned(),
                revision: review.revision,
                files: patch
                    .files
                    .entries
                    .into_iter()
                    .map(|entry| PreviewEntry {
                        path: entry.path,
                        before: PreviewFile::for_bytes(entry.before.as_deref()),
                        after: PreviewFile::for_bytes(entry.after.as_deref()),
                    })
                    .collect(),
            })
        })
        .await?
    }

    pub async fn apply_saved_patch(
        &self,
        request: SavedPatchRequest,
        snapshot: String,
    ) -> Result<()> {
        let scope = self.git_review_scope(request.checkout_id.clone()).await?;
        ensure!(
            scope.project == request.project_id,
            "Select a registered worktree in the recorded attempt's project."
        );
        let (request, capture) = self
            .execute(move |db| {
                let capture = saved_capture(db, &request)?;
                Ok((request, capture))
            })
            .await?;
        let checkout = scope.id.clone();
        let (project, owner) = (scope.project.clone(), scope.owner.clone());
        let store = self.clone();
        let runtime = Handle::current();
        let publish = move || {
            let review = LiveReview::open(&scope)?;
            let index = Path::new(&review.paths.git_dir);
            let _directories = lock_directories(index)?;
            let _lock =
                lock_git_file(&index.join("index.lock")).context("lock the worktree index")?;
            review.verify()?;
            let patch = prepare(&scope, &review, &request, &capture)?;
            ensure!(
                patch.snapshot == snapshot,
                "The reviewed patch or its destination changed. Review the current comparison again."
            );
            runtime.block_on(store.execute(move |db| {
                let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                saved_capture(&transaction, &request)?;
                patch.files.verify_no_drafts(&transaction, &scope.id)?;
                review.verify()?;
                patch.publish(&scope.root)
            }))
        };
        match owner {
            Some(task) => {
                with_managed_worktree_writer(self, project, checkout, task, move |_| publish())
                    .await
            }
            None => tokio::task::spawn_blocking(publish).await?,
        }
    }
}

fn saved_capture(db: &Connection, request: &SavedPatchRequest) -> Result<Box<CandidateRevision>> {
    let payload: Vec<u8> = db.query_row(
        "SELECT candidate.evidence FROM job_worktree_candidates candidate
         JOIN job_attempts attempt ON attempt.run_id = candidate.run_id JOIN jobs job ON job.id = attempt.job_id
         WHERE job.project_id = ?1 AND candidate.run_id = ?2 AND candidate.capture_confirmed = 1
           AND NOT EXISTS(SELECT 1 FROM history_deleted_attempt_diffs deleted WHERE deleted.run_id = candidate.run_id)",
        params![request.project_id, request.run_id], |row| row.get(0),
    ).optional()?.context("The saved patch is unavailable for this attempt.")?;
    let AttemptWorktreeEvidence::Saved { artifact } =
        serde_json::from_slice(&protection::unprotect(&payload)?)?
    else {
        anyhow::bail!("The saved patch is unavailable for this attempt.");
    };
    ensure!(
        artifact.revision == request.capture_revision,
        "The saved capture changed. Open it again to review its patch."
    );
    Ok(artifact)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Prepared {
    files: ReviewedFiles,
    snapshot: String,
}

fn prepare(
    scope: &ReviewScope,
    review: &LiveReview,
    request: &SavedPatchRequest,
    capture: &CandidateRevision,
) -> Result<Prepared> {
    ensure!(
        capture.start.git_common_dir == Path::new(&scope.common),
        "The saved patch belongs to a different Git repository."
    );
    let git = ScratchGit::new(&capture.start.git_common_dir, &capture.start.starting.head)?;
    git.run(&["read-tree", &capture.start.starting.head], None)?;
    let bytes = |text: &str| -> Result<Vec<u8>> {
        ensure!(
            text.len() <= MAX_PATCH * 2,
            "The recorded patch exceeds the review limit."
        );
        let bytes = STANDARD.decode(text)?;
        ensure!(
            bytes.len() <= MAX_PATCH,
            "The recorded patch exceeds the review limit."
        );
        Ok(bytes)
    };
    let changes = capture.candidate.changes.as_ref();
    let parts = [
        capture.committed_patch_base64.as_str(),
        changes.map_or("", |changes| changes.staged_patch_base64.as_str()),
        changes.map_or("", |changes| changes.unstaged_patch_base64.as_str()),
    ];
    let selected = match request.part {
        PatchPart::Committed => 0,
        PatchPart::Staged => 1,
        PatchPart::Working => 2,
    };
    for part in &parts[..selected] {
        git.apply(&bytes(part)?)?;
    }
    let base = git.object(&["write-tree"])?;
    let patch = bytes(parts[selected])?;
    ensure!(
        !patch.is_empty(),
        "This saved selection contains no Git patch."
    );
    git.apply(&patch)?;
    let after = git.object(&["write-tree"])?;
    let paths = changed_paths(&git, &base, &after)?;
    let mut identities = BTreeMap::new();
    let mut files = ReviewedFiles::default();
    let mut total = 0;
    for path in paths {
        let (parent, identity) = files.lock_parent(&scope.root, &path)?;
        identities.insert(parent, identity);
        let destination = scope.root.join(&path);
        let held = open_target(&destination)?;
        let before = held
            .as_ref()
            .map(|file| read_bytes(file.try_clone()?))
            .transpose()?;
        total += before.as_ref().map_or(0, Vec::len);
        ensure!(
            total <= MAX_TOTAL,
            "The destination contents exceed the review limit."
        );
        if let Some(before) = &before {
            let temp_path = git.root().join(&path);
            fs::create_dir_all(temp_path.parent().context("patch file has no parent")?)?;
            fs::write(temp_path, before)?;
        }
        files.entries.push(ReviewedFile {
            path,
            before,
            after: None,
            held,
        });
    }
    ensure!(
        review.status()?.iter().all(|status| !status.unmerged()
            || files.entries.iter().all(|entry| entry.path != status.path)),
        "Resolve conflicts in the affected destination files before applying this patch."
    );
    let attributes =
        project_checkout_attributes(&scope.root, review, &git, &files.entries, &mut files.guards)?;
    let options = review.view.passive_options();
    let args: Vec<_> = options
        .iter()
        .flat_map(|option| ["-c", option.as_str()])
        .chain(["apply", "--binary", "--whitespace=nowarn", "-"])
        .collect();
    git.run(&args, Some(&patch))
        .context("The selected patch does not apply to this destination's current files")?;
    for entry in &mut files.entries {
        entry.after = open_target(&git.root().join(&entry.path))?
            .map(read_bytes)
            .transpose()?;
        total += entry.after.as_ref().map_or(0, Vec::len);
        ensure!(
            total <= MAX_TOTAL,
            "The before and after comparison exceeds the review limit."
        );
    }
    files.entries.retain(|entry| entry.before != entry.after);
    ensure!(
        !files.entries.is_empty(),
        "The selected patch produces no file content changes in this worktree."
    );
    let fingerprints = files
        .entries
        .iter()
        .map(|entry| {
            Ok((
                &entry.path,
                entry.held.as_ref().map(file_identity).transpose()?,
                entry.before.as_deref().map(digest),
                entry.after.as_deref().map(digest),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = digest(&serde_json::to_vec(&(
        request,
        digest(&patch),
        &review.revision,
        options,
        attributes,
        identities,
        &files.missing_folders,
        fingerprints,
    ))?);
    Ok(Prepared { files, snapshot })
}

fn project_checkout_attributes(
    root: &Path,
    review: &LiveReview,
    git: &ScratchGit,
    entries: &[ReviewedFile],
    held: &mut Vec<File>,
) -> Result<Vec<(String, Option<String>)>> {
    let index_path = review.view.index_path();
    let isolated = index_path
        .parent()
        .context("the isolated index has no directory")?;
    let scratch_metadata = git.root().join(".git");
    if review.revision.index.is_some() {
        for entry in fs::read_dir(isolated)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == "index"
                || name
                    .to_str()
                    .is_some_and(|name| name.starts_with("sharedindex."))
            {
                fs::copy(entry.path(), scratch_metadata.join(name))?;
            }
        }
    } else {
        git.run(&["read-tree", "--empty"], None)?;
    }
    let app_owned_attributes = isolated.join("info/attributes");
    let mut signatures = Vec::new();
    if let Some(file) = open_target(&app_owned_attributes)? {
        let bytes = read_bytes(file)?;
        let destination = scratch_metadata.join("info/attributes");
        fs::create_dir_all(destination.parent().context("attributes have no parent")?)?;
        fs::write(destination, &bytes)?;
        signatures.push((".git/info/attributes".into(), Some(digest(&bytes))));
    }
    let paths: BTreeSet<_> = entries
        .iter()
        .flat_map(|entry| Path::new(&entry.path).ancestors().skip(1))
        .map(|parent| parent.join(".gitattributes"))
        .collect();
    for path in paths {
        let relative = path
            .to_str()
            .context("invalid checkout attribute path")?
            .replace('\\', "/");
        if entries
            .iter()
            .any(|entry| entry.path.eq_ignore_ascii_case(&relative))
        {
            continue;
        }
        let fingerprint = if let Some(file) = open_target(&root.join(&path))? {
            let bytes = read_bytes(file.try_clone()?)?;
            let destination = git.root().join(&path);
            fs::create_dir_all(destination.parent().context("attributes have no parent")?)?;
            fs::write(destination, &bytes)?;
            held.push(file);
            Some(digest(&bytes))
        } else {
            None
        };
        signatures.push((relative, fingerprint));
    }
    Ok(signatures)
}

fn changed_paths(git: &ScratchGit, base: &str, after: &str) -> Result<Vec<String>> {
    let output = git.run(
        &[
            "diff-tree",
            "--raw",
            "--no-commit-id",
            "--no-renames",
            "-r",
            "-z",
            base,
            after,
        ],
        None,
    )?;
    let names = std::str::from_utf8(&output)?;
    let mut fields = names.split_terminator('\0');
    let mut aliases = BTreeMap::new();
    let mut paths = Vec::new();
    while let Some(metadata) = fields.next() {
        let path = fields.next().context("Git omitted a changed patch path")?;
        validate_path(path)?;
        path.split('/').try_for_each(validate_name)?;
        let mut modes = metadata.trim_start_matches(':').split_ascii_whitespace();
        let before = modes.next().context("Git omitted the previous file mode")?;
        let after = modes.next().context("Git omitted the new file mode")?;
        ensure!(
            matches!(before, "000000" | "100644" | "100755")
                && matches!(after, "000000" | "100644" | "100755"),
            "The saved patch includes a symlink or submodule at {path}."
        );
        ensure!(
            before == after || after == "000000" || before == "000000" && after == "100644",
            "The saved patch changes a Git file mode at {path}. Apply it from Git to preserve that mode."
        );
        for item in Path::new(path)
            .ancestors()
            .filter(|item| !item.as_os_str().is_empty())
        {
            let name = item.to_str().context("invalid patch path")?;
            ensure!(
                aliases
                    .insert(name.to_uppercase(), name.to_owned())
                    .is_none_or(|prior| prior == name),
                "The saved patch contains paths that collide on Windows."
            );
        }
        paths.push(path.into());
        ensure!(
            paths.len() <= MAX_PATHS,
            "The saved patch contains too many files for a single review."
        );
    }
    Ok(paths)
}

fn read_bytes(file: File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE,
        "An affected file exceeds the saved patch review limit."
    );
    Ok(bytes)
}

impl Prepared {
    fn publish(self, root: &Path) -> Result<()> {
        self.files.publish(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::{
        GitStartingState, ManagedWorktree, WorktreeStart, git_path, git_query, register,
    };

    fn git(root: &Path, args: &[&str]) -> Result<String> {
        git_query(&git_path()?, root, args)
    }

    fn commit(root: &Path, subject: &str) -> Result<String> {
        git(root, &["add", "--all"])?;
        git(
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                subject,
            ],
        )?;
        Ok(git(root, &["rev-parse", "HEAD"])?.trim().into())
    }

    #[tokio::test]
    async fn previews_exact_file_bytes_and_restores_the_originals_if_a_destination_appears()
    -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("source");
        fs::create_dir(&root)?;
        git(&root, &["init", "--quiet"])?;
        git(&root, &["config", "core.autocrlf", "false"])?;
        fs::write(root.join("alpha.txt"), "original\n")?;
        fs::write(root.join("remove.txt"), "remove me\n")?;
        let base = commit(&root, "Base")?;
        fs::write(root.join("alpha.txt"), "saved change\n")?;
        fs::remove_file(root.join("remove.txt"))?;
        fs::create_dir(root.join("letters"))?;
        fs::write(root.join("letters/number.bin"), [0, 255, 1, 10])?;
        fs::write(root.join("z-last.txt"), "saved addition\n")?;
        let head = commit(&root, "Captured")?;
        let patch = git(
            &root,
            &["diff", "--binary", "--full-index", &base, &head, "--"],
        )?;
        let destination = fixture.path().join("destination");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                destination.to_str().unwrap(),
                &base,
            ],
        )?;
        let database = Store::open(fixture.path().join("store.sqlite3"))?;
        let source = register(&database, root.clone()).await?;
        let target = register(&database, destination.clone()).await?;
        let scope = database
            .git_review_scope(target.checkout_id.clone())
            .await?;
        let request = SavedPatchRequest {
            project_id: source.project_id.clone(),
            run_id: "run".into(),
            checkout_id: target.checkout_id,
            capture_revision: "capture".into(),
            part: PatchPart::Committed,
        };
        let start = GitStartingState {
            head: base.clone(),
            status: vec![],
            index_digest: String::new(),
            staged_diff_digest: String::new(),
            unstaged_diff_digest: String::new(),
            changes: None,
        };
        let capture = CandidateRevision {
            revision: request.capture_revision.clone(),
            committed_patch_base64: STANDARD.encode(patch),
            candidate: GitStartingState {
                head,
                ..start.clone()
            },
            start: WorktreeStart {
                worktree: ManagedWorktree {
                    project_id: source.project_id,
                    owner_task_id: "task".into(),
                    checkout_id: source.checkout_id,
                    root: root.clone(),
                    branch: "fixture".into(),
                    base_commit: base,
                },
                repository_root: root,
                git_common_dir: scope.common.clone().into(),
                starting: start,
                index_entries_digest: None,
            },
        };
        let review = LiveReview::open(&scope)?;
        let initial = prepare(&scope, &review, &request, &capture)?;
        assert_eq!(initial.files.entries.len(), 4);
        assert_eq!(
            initial.files.entries[0].before.as_deref(),
            Some(b"original\n".as_slice())
        );
        assert_eq!(
            initial.files.entries[0].after.as_deref(),
            Some(b"saved change\n".as_slice())
        );
        assert!(!destination.join("letters").exists());
        let index_path = Path::new(&review.paths.git_dir).join("index");
        let index = fs::read(&index_path)?;
        let snapshot = initial.snapshot.clone();
        // Simulate another process producing a file after all prior destination contents were locked.
        fs::write(destination.join("z-last.txt"), "external file\n")?;
        assert!(initial.publish(&destination).is_err());
        assert_eq!(fs::read(destination.join("alpha.txt"))?, b"original\n");
        assert_eq!(fs::read(destination.join("remove.txt"))?, b"remove me\n");
        assert_eq!(
            fs::read(destination.join("z-last.txt"))?,
            b"external file\n"
        );
        assert!(!destination.join("letters").exists());
        assert!(prepare(&scope, &review, &request, &capture).is_err());
        fs::remove_file(destination.join("z-last.txt"))?;
        let prepared = prepare(&scope, &review, &request, &capture)?;
        assert_eq!(prepared.snapshot, snapshot);
        prepared.publish(&destination)?;
        assert_eq!(fs::read(destination.join("alpha.txt"))?, b"saved change\n");
        assert_eq!(
            fs::read(destination.join("letters/number.bin"))?,
            [0, 255, 1, 10]
        );
        assert!(!destination.join("remove.txt").exists());
        assert_eq!(fs::read(&index_path)?, index);
        assert!(
            database.preview_saved_patch(request).await.is_err(),
            "do not admit a capture ID absent from the database"
        );
        Ok(())
    }
}
