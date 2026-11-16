use std::{io::Read, path::PathBuf, process::Stdio};

use anyhow::{Context, Result, ensure};
use rusqlite::params;

use super::{
    super::{
        git_command, git_path, inspect,
        isolated_git::check,
        verification::materialize::{MaterializedCandidate, ScratchGit},
    },
    DestinationDriftOutcome, DestinationPin, VerifiedCommit, moved,
};
use crate::store::Store;

#[derive(Clone)]
pub(super) struct DestinationRepository {
    git: PathBuf,
    checkout: PathBuf,
    common: PathBuf,
    source_ref: String,
    source_sha: String,
    destination_ref: String,
}

pub(super) enum MergePreview {
    Conflicted(Vec<String>),
    Clean {
        tree: String,
        changed_paths: Vec<String>,
    },
}

impl DestinationRepository {
    pub(super) async fn load(store: &Store, source: &VerifiedCommit, branch: &str) -> Result<Self> {
        let (checkout, git_dir, common): (String, String, String) = store
            .execute({
                let source = source.clone();
                move |db| {
                    Ok(db.query_row(
                        "SELECT c.root, c.git_dir, p.git_common_dir FROM checkouts c
                     JOIN projects p ON p.id = c.project_id WHERE c.id = ?1 AND p.id = ?2",
                        params![source.checkout_id, source.project_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?)
                }
            })
            .await?;
        ensure!(
            source.repository == common,
            "the source repository registration changed"
        );
        let source_ref = format!("refs/heads/{}", source.branch);
        let source_sha = source.sha.clone();
        let destination_ref = format!("refs/heads/{branch}");
        tokio::task::spawn_blocking(move || {
            let git = git_path()?;
            let path = PathBuf::from(&checkout);
            let observed = inspect(&path)?;
            ensure!(
                observed.checkout_root == checkout
                    && observed.git_dir == git_dir
                    && observed.git_common_dir == common,
                "the registered source repository changed"
            );
            check(
                git_command(&git, &path)
                    .args(["check-ref-format", &destination_ref])
                    .output()?,
                "validate the destination branch",
            )?;
            Ok(Self {
                git,
                checkout: path,
                common: common.into(),
                source_ref,
                source_sha,
                destination_ref,
            })
        })
        .await?
    }

    pub(super) async fn movement(&self, expected: &str) -> Result<Option<DestinationDriftOutcome>> {
        let repository = self.clone();
        let expected = expected.to_owned();
        tokio::task::spawn_blocking(move || {
            ensure!(
                repository.tip(&repository.source_ref)?.as_deref() == Some(&repository.source_sha),
                "the approved working branch moved before destination review"
            );
            let actual = repository.tip(&repository.destination_ref)?;
            Ok((actual.as_deref() != Some(&expected)).then(|| moved(&expected, actual)))
        })
        .await?
    }

    pub(super) async fn preview(&self, pin: &DestinationPin) -> Result<MergePreview> {
        let repository = self.clone();
        let pin = pin.clone();
        tokio::task::spawn_blocking(move || {
            let scratch = ScratchGit::new(&repository.common, &pin.destination_sha)?;
            let merge = preview_tree(&scratch, &pin)?;
            let MergePreview::Clean { tree, .. } = merge else {
                return Ok(merge);
            };
            let files = scratch
                .command()
                .args([
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    "--no-ext-diff",
                    "--no-textconv",
                    &pin.destination_sha,
                    &tree,
                    "--",
                ])
                .output()?;
            let files = check(files, "list destination candidate paths")?;
            Ok(MergePreview::Clean {
                tree,
                changed_paths: paths(&files),
            })
        })
        .await?
    }

    pub(super) fn materialize(
        &self,
        pin: &DestinationPin,
        expected_tree: &str,
    ) -> Result<MaterializedCandidate> {
        let scratch = self.reproduce_tree(pin, expected_tree)?;
        scratch.materialize_tree(
            expected_tree,
            &[&pin.destination_sha, &pin.source_sha],
            b"Prepared destination update\n",
        )
    }

    pub(super) async fn patch(&self, pin: DestinationPin, tree: String) -> Result<(Vec<u8>, bool)> {
        let repository = self.clone();
        tokio::task::spawn_blocking(move || {
            const PATCH_LIMIT: usize = 1024 * 1024;
            let scratch = repository.reproduce_tree(&pin, &tree)?;
            let mut process = scratch
                .command()
                .args([
                    "diff",
                    "--binary",
                    "--full-index",
                    "--no-renames",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    &pin.destination_sha,
                    &tree,
                    "--",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let stdout = process.stdout.take().expect("Git diff stdout was piped");
            let mut bytes = Vec::new();
            let read = stdout
                .take((PATCH_LIMIT + 1) as u64)
                .read_to_end(&mut bytes);
            let truncated = bytes.len() > PATCH_LIMIT;
            if truncated || read.is_err() {
                let _ = process.kill();
            }
            let status = process.wait()?;
            read?;
            ensure!(
                truncated || status.success(),
                "Git could not read the exact integration diff"
            );
            bytes.truncate(PATCH_LIMIT);
            Ok((bytes, truncated))
        })
        .await?
    }

    fn reproduce_tree(&self, pin: &DestinationPin, expected_tree: &str) -> Result<ScratchGit> {
        let scratch = ScratchGit::new(&self.common, &pin.destination_sha)?;
        let MergePreview::Clean { tree, .. } = preview_tree(&scratch, pin)? else {
            anyhow::bail!("the pinned destination update cannot be recreated without conflicts");
        };
        ensure!(
            tree == expected_tree,
            "the prepared destination candidate changed"
        );
        Ok(scratch)
    }

    fn tip(&self, branch_ref: &str) -> Result<Option<String>> {
        let output = git_command(&self.git, &self.checkout)
            .arg("--git-dir")
            .arg(&self.common)
            .args(["rev-parse", "--verify", "--quiet", branch_ref])
            .output()?;
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        let bytes = check(output, "resolve the pinned branch")?;
        Ok(Some(normalized_oid(std::str::from_utf8(&bytes)?.trim())?))
    }
}

fn preview_tree(scratch: &ScratchGit, pin: &DestinationPin) -> Result<MergePreview> {
    let output = scratch
        .command()
        .env("GIT_ATTR_SOURCE", &pin.destination_sha)
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            &pin.destination_sha,
            &pin.source_sha,
        ])
        .output()?;
    if output.status.code() == Some(1) {
        let files = output
            .stdout
            .splitn(2, |byte| *byte == 0)
            .nth(1)
            .unwrap_or_default();
        return Ok(MergePreview::Conflicted(paths(files)));
    }
    let output = check(output, "prepare the destination update")?;
    let tree = output
        .split(|byte| *byte == 0)
        .next()
        .context("Git did not return a merge tree")?;
    let tree = normalized_oid(std::str::from_utf8(tree)?.trim())?;
    Ok(MergePreview::Clean {
        tree,
        changed_paths: Vec::new(),
    })
}

fn paths(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

pub(super) fn normalized_oid(value: &str) -> Result<String> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a full Git object ID is required"
    );
    Ok(value.to_ascii_lowercase())
}
