use crate::repository::{checkout_items::mutations::recycle_checked, documents::file_identity};

use super::*;

pub(super) struct ReviewedRecycle {
    path: PathBuf,
    file: File,
    directories: Vec<File>,
    pub(super) snapshot: String,
    pub(super) preview: PreviewFile,
}

fn require_untracked(review: &LiveReview, path: &str) -> Result<()> {
    ensure!(
        review.status()?.into_iter().any(|entry| entry.path == path
            && entry.working == Some('?')
            && entry.staged.is_none()),
        "the selected file is no longer untracked in this worktree"
    );
    // A staged deletion and an untracked replacement can share a status path.
    // Status already rejects those, and the exact literal path must be absent from the copied index.
    let indexed = review.view.read(&[
        "--literal-pathspecs",
        "ls-files",
        "--stage",
        "-z",
        "--",
        path,
    ])?;
    ensure!(indexed.is_empty(), "the selected file is in the Git index");
    Ok(())
}

impl ReviewedRecycle {
    pub(super) fn prepare(scope: &ReviewScope, review: &LiveReview, path: &str) -> Result<Self> {
        require_untracked(review, path)?;
        let (directory, name) = path.rsplit_once('/').unwrap_or(("", path));
        let directory = checkout_directory(&scope.root, directory)?;
        let directories = lock_directories(&directory)?;
        let destination = directory.join(name);
        let mut file = open_locked_local_file(&destination)
            .context("open the reviewed regular untracked file")?;
        let identity = file_identity(&file)?;
        let size = file.metadata()?.len();
        let (sha256, preview) = if size <= MAX_PREVIEW_BYTES as u64 {
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_PREVIEW_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 == size,
                "the untracked file changed during review"
            );
            (digest(&bytes), PreviewFile::from_bytes(Some(&bytes)))
        } else {
            let mut hash = Sha256::new();
            let read = io::copy(&mut file, &mut hash)?;
            ensure!(read == size, "the untracked file changed during review");
            let sha256 = format!("{:x}", hash.finalize());
            let preview = PreviewFile::Bytes {
                size,
                sha256: sha256.clone(),
                reason: "The file exceeds the text preview limit.",
            };
            (sha256, preview)
        };
        let snapshot = digest(&serde_json::to_vec(&(
            &scope.id,
            path,
            &review.revision,
            "untracked",
            identity,
            size,
            sha256,
        ))?);
        Ok(Self {
            path: destination,
            file,
            directories,
            snapshot,
            preview,
        })
    }

    pub(super) fn apply(self, relative: &str, review: &LiveReview) -> Result<()> {
        let Self {
            path,
            file,
            directories,
            ..
        } = self;
        let _directories = directories;
        recycle_checked(&path, || {
            review.verify()?;
            require_untracked(review, relative)?;
            // The Shell needs delete-sharing for the original path. Hold the identity and bytes
            // until immediately before PerformOperations, retaining the parent and index guards.
            drop(file);
            Ok(())
        })
    }
}
