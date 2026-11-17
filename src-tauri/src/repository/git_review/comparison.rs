use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::{
    GitFileSide, IsolatedGit, ReviewScope, base_commit, branch_reference, content, inspect,
    passive, validate_oid, validate_path,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchRange {
    pub base_reference: String,
    pub head_reference: Option<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchRevisions {
    pub base: String,
    pub head: String,
    pub merge_base: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchComparison {
    pub revisions: GitBranchRevisions,
    pub changes: Vec<GitBranchComparisonFile>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
pub struct GitBranchComparisonFile {
    pub path: String,
    pub status: char,
}

#[derive(Debug, Serialize)]
pub struct GitBranchDiff {
    pub path: String,
    pub original: GitFileSide,
    pub modified: GitFileSide,
}

impl Store {
    pub async fn git_branch_comparison(
        &self,
        checkout: String,
        range: GitBranchRange,
    ) -> Result<GitBranchComparison> {
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let (view, revisions) = comparison(&scope, &range)?;
            let changes = changed_files(&view, &revisions)?;
            Ok(GitBranchComparison { revisions, changes })
        })
        .await?
    }

    pub async fn git_branch_comparison_diff(
        &self,
        checkout: String,
        range: GitBranchRange,
        expected: GitBranchRevisions,
        path: String,
    ) -> Result<GitBranchDiff> {
        validate_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let (view, revisions) = comparison(&scope, &range)?;
            ensure!(
                revisions == expected,
                "the selected branches changed; refresh the comparison and try again"
            );
            let original = content::read_tree(&view, &revisions.merge_base, &path)?;
            let modified = content::read_tree(&view, &revisions.head, &path)?;
            Ok(GitBranchDiff {
                path,
                original,
                modified,
            })
        })
        .await?
    }
}

fn comparison(
    scope: &ReviewScope,
    range: &GitBranchRange,
) -> Result<(IsolatedGit, GitBranchRevisions)> {
    let paths = inspect(&scope.root)?;
    ensure!(
        paths.git_dir == scope.git_dir && paths.git_common_dir == scope.common,
        "checkout identity changed during branch comparison"
    );
    let base = branch_reference(&scope.root, &range.base_reference)?.sha;
    let head = match &range.head_reference {
        Some(reference) => branch_reference(&scope.root, reference)?.sha,
        None => base_commit(&scope.root)?.context("the selected checkout has no HEAD commit")?,
    };
    let merge_base = String::from_utf8(
        passive(&scope.root, &["merge-base", &base, &head])
            .context("find the common ancestor of the selected branches")?,
    )?
    .trim()
    .to_owned();
    validate_oid(&merge_base)?;
    let view = IsolatedGit::open(&paths, &scope.root, &head)?;
    Ok((
        view,
        GitBranchRevisions {
            base,
            head,
            merge_base,
        },
    ))
}

fn changed_files(
    view: &IsolatedGit,
    revisions: &GitBranchRevisions,
) -> Result<Vec<GitBranchComparisonFile>> {
    let output = String::from_utf8(view.read(&[
        "diff",
        "--name-status",
        "-z",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
        &revisions.merge_base,
        &revisions.head,
        "--",
    ])?)?;
    let fields: Vec<_> = output.split_terminator('\0').collect();
    ensure!(
        fields.len().is_multiple_of(2),
        "Git returned an incomplete compared path"
    );
    fields
        .chunks_exact(2)
        .map(|entry| {
            let mut status = entry[0].chars();
            let code = status
                .next()
                .context("Git returned a path without a status")?;
            ensure!(
                status.next().is_none(),
                "Git returned an invalid comparison status"
            );
            Ok(GitBranchComparisonFile {
                path: entry[1].into(),
                status: code,
            })
        })
        .collect()
}
