use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};

use super::{DeliveryMergeRequest, VerifiedCommit};
use crate::repository::{
    CheckoutPaths,
    documents::lock_directories,
    git_command, git_path, inspect,
    isolated_git::{IsolatedGit, check},
};

pub(super) struct MergeRepository {
    git: PathBuf,
    root: PathBuf,
    paths: CheckoutPaths,
}

pub(super) enum Plan {
    Ready(String),
    Conflicted,
}

#[derive(Debug)]
pub(super) enum Publication {
    Published,
    DestinationMoved(Option<String>),
    DestinationCheckedOut(String),
}

struct Commit {
    tree: String,
    parents: Vec<String>,
}

impl MergeRepository {
    pub(super) fn open(root: PathBuf, common: &str) -> Result<Self> {
        let paths = inspect(&root)?;
        ensure!(
            paths.git_common_dir == common
                && Path::new(&paths.checkout_root) == root
                && Path::new(&paths.repository_root) == root,
            "registered merge repository identity changed"
        );
        Ok(Self {
            git: git_path()?,
            root,
            paths,
        })
    }

    pub(super) fn primary_branch(&self) -> Result<String> {
        let branch = String::from_utf8(self.read(&["symbolic-ref", "--quiet", "HEAD"])?)?;
        Ok(branch
            .trim_end()
            .strip_prefix("refs/heads/")
            .context("configure an integration branch for the project")?
            .into())
    }

    fn branch_ref(&self, branch: &str) -> Result<String> {
        let qualified = format!("refs/heads/{branch}");
        self.read(&["check-ref-format", &qualified])
            .context("a valid local branch is required")?;
        Ok(qualified)
    }

    pub(super) fn branch_tip(&self, branch: &str) -> Result<String> {
        self.find_branch_tip(branch)?
            .context("the local delivery branch is missing")
    }

    pub(super) fn find_branch_tip(&self, branch: &str) -> Result<Option<String>> {
        let reference = self.branch_ref(branch)?;
        let symbolic = self
            .command()
            .args(["symbolic-ref", "--quiet", "--no-recurse", &reference])
            .output()?;
        ensure!(
            symbolic.status.code() == Some(1) && symbolic.stderr.is_empty(),
            "the delivery branch must be a direct local Git ref"
        );
        let output = self
            .command()
            .args(["rev-parse", "--verify", "--quiet", &reference])
            .output()?;
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(None);
        }
        Ok(Some(text_oid(check(
            output,
            "read the local delivery branch",
        )?)?))
    }

    pub(super) fn attached_checkout(&self, branch: &str) -> Result<Option<String>> {
        let wanted = self.branch_ref(branch)?;
        let list = String::from_utf8(self.read(&["worktree", "list", "--porcelain", "-z"])?)?;
        let mut checkout = None;
        for field in list.split('\0') {
            if let Some(path) = field.strip_prefix("worktree ") {
                checkout = Some(path);
            } else if field.strip_prefix("branch ") == Some(wanted.as_str()) {
                return Ok(Some(
                    checkout
                        .context("Git did not identify the destination checkout")?
                        .to_owned(),
                ));
            }
        }
        Ok(None)
    }

    pub(super) fn verify_chain(&self, commits: &[VerifiedCommit]) -> Result<()> {
        let first = commits
            .first()
            .context("delivery has no verified commit chain")?;
        let view = self.objects(&first.base_commit)?;
        let mut previous = first.base_commit.as_str();
        for evidence in commits {
            let item = read_commit(&view, &evidence.sha)?;
            ensure!(
                evidence.branch == first.branch
                    && evidence.checkout_id == first.checkout_id
                    && evidence.base_commit == previous
                    && item.tree == evidence.tree
                    && item.parents == [previous],
                "repository history does not match the approved commit chain"
            );
            previous = &evidence.sha;
        }
        Ok(())
    }

    pub(super) fn verify_incorporation(
        &self,
        request: &DeliveryMergeRequest,
        sha: &str,
        commits: &[VerifiedCommit],
    ) -> Result<()> {
        let view = self.objects(&request.expected_destination_sha)?;
        read_commit(&view, sha)?;
        ensure!(
            is_ancestor(&view, &request.expected_destination_sha, sha)?,
            "integration does not retain the approved destination history"
        );
        for commit in commits {
            ensure!(
                is_ancestor(&view, &commit.sha, sha)?,
                "integration is missing approved commit {}",
                commit.sha
            );
        }
        Ok(())
    }

    pub(super) fn require_tree(&self, sha: &str, tree: Option<&str>) -> Result<()> {
        if let Some(tree) = tree {
            ensure!(
                read_commit(&self.objects(sha)?, sha)?.tree == tree,
                "integration tree differs from the accepted destination update"
            );
        }
        Ok(())
    }

    pub(super) fn verify_external(&self, request: &DeliveryMergeRequest, sha: &str) -> Result<()> {
        let destination = &request.expected_destination_sha;
        let source = &request.expected_source_sha;
        if sha == source {
            return Ok(());
        }
        let view = self.objects(destination)?;
        let integration = read_commit(&view, sha)?;
        let parent = if sha == destination {
            // The branch may already contain the approved chain when first observed.
            // Check the integration tree too when the observed tip is its merge.
            match integration.parents.as_slice() {
                [parent, secondary] if secondary == source => parent,
                _ => anyhow::bail!("the observed commit is not the exact approved integration"),
            }
        } else {
            ensure!(
                integration.parents == [destination.as_str(), source.as_str()],
                "the external merge has different approved parents; review the destination update"
            );
            destination
        };
        let expected_tree = merge_tree(&view, parent, source)?
            .context("the external merge resolved conflicts and requires new acceptance")?;
        ensure!(
            integration.tree == expected_tree,
            "external merge tree differs from the approved integration"
        );
        Ok(())
    }

    pub(super) fn external_update_parent(
        &self,
        request: &DeliveryMergeRequest,
        sha: &str,
    ) -> Result<String> {
        let integration = read_commit(&self.objects(sha)?, sha)?;
        match integration.parents.as_slice() {
            [parent, source] if source == &request.expected_source_sha => Ok(parent.clone()),
            _ => anyhow::bail!("an external destination update must record both accepted parents"),
        }
    }

    pub(super) fn prepare(
        &self,
        request: &DeliveryMergeRequest,
        commits: &[VerifiedCommit],
    ) -> Result<Plan> {
        let destination = &request.expected_destination_sha;
        let source = &request.expected_source_sha;
        let view = self.objects(destination)?;
        if is_ancestor(&view, destination, source)? {
            return Ok(Plan::Ready(source.clone()));
        }
        if is_ancestor(&view, source, destination)? {
            self.verify_external(request, destination)?;
            return Ok(Plan::Ready(destination.clone()));
        }
        let Some(tree) = merge_tree(&view, destination, source)? else {
            return Ok(Plan::Conflicted);
        };
        let date = &commits
            .last()
            .context("delivery has no verified commits")?
            .verified_at;
        let sha = text_oid(check(
            view.command()
                .env("GIT_AUTHOR_NAME", "OmniAgent")
                .env("GIT_AUTHOR_EMAIL", "automation@omniagent.invalid")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_NAME", "OmniAgent")
                .env("GIT_COMMITTER_EMAIL", "automation@omniagent.invalid")
                .env("GIT_COMMITTER_DATE", date)
                .args([
                    "commit-tree",
                    "--no-gpg-sign",
                    &tree,
                    "-p",
                    destination,
                    "-p",
                    source,
                    "-m",
                    "OmniAgent verified delivery integration",
                ])
                .output()?,
            "prepare the approved integration commit",
        )?)?;
        let result = read_commit(&view, &sha)?;
        ensure!(
            result.tree == tree && result.parents == [destination.as_str(), source.as_str()],
            "prepared merge object does not match the approved refs and integration tree"
        );
        Ok(Plan::Ready(sha))
    }

    pub(super) fn publish(&self, request: &DeliveryMergeRequest, sha: &str) -> Result<Publication> {
        let _root = lock_directories(&self.root)?;
        let _metadata = lock_directories(Path::new(&self.paths.git_common_dir))?;
        let observed = inspect(&self.root)?;
        ensure!(
            observed.git_dir == self.paths.git_dir
                && observed.git_common_dir == self.paths.git_common_dir
                && observed.checkout_root == self.paths.checkout_root,
            "registered merge repository identity changed"
        );
        let source = self.branch_ref(&request.expected_source_branch)?;
        let destination = self.branch_ref(&request.destination_branch)?;
        ensure!(
            self.branch_tip(&request.expected_source_branch)? == request.expected_source_sha,
            "approved working branch moved before merge"
        );
        let tip = self.find_branch_tip(&request.destination_branch)?;
        if tip.as_deref() != Some(request.expected_destination_sha.as_str()) {
            return Ok(Publication::DestinationMoved(tip));
        }
        let mut child = self
            .command()
            .args([
                "update-ref",
                "-m",
                "OmniAgent verified delivery integration",
                "--stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let result = (|| {
            let mut input = child
                .stdin
                .take()
                .context("Git ref transaction requires stdin")?;
            let mut response = BufReader::new(
                child
                    .stdout
                    .take()
                    .context("Git ref transaction requires stdout")?,
            );
            send(&mut input, &mut response, "start\n", "start")?;
            send(
                &mut input,
                &mut response,
                &format!(
                    "option no-deref\nverify {source} {}\noption no-deref\nupdate {destination} {sha} {}\nprepare\n",
                    request.expected_source_sha, request.expected_destination_sha
                ),
                "prepare",
            )?;
            // Recheck worktree attachment while Git holds both expected ref locks.
            // A ref update has no accompanying worktree or index write.
            if let Some(checkout) = self.attached_checkout(&request.destination_branch)? {
                send(&mut input, &mut response, "abort\n", "abort")?;
                return Ok(Publication::DestinationCheckedOut(checkout));
            }
            send(&mut input, &mut response, "commit\n", "commit")?;
            Ok(Publication::Published)
        })();
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let tip = self.find_branch_tip(&request.destination_branch)?;
            if tip.as_deref() != Some(request.expected_destination_sha.as_str()) {
                return Ok(Publication::DestinationMoved(tip));
            }
            check(output, "publish the expected delivery refs")?;
        }
        result
    }

    fn objects(&self, head: &str) -> Result<IsolatedGit> {
        IsolatedGit::open(&self.paths, &self.root, head)
    }

    fn command(&self) -> Command {
        let mut command = git_command(&self.git, &self.root);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["-c", "core.hooksPath=NUL", "-c", "core.fsmonitor=false"]);
        command
    }

    fn read(&self, args: &[&str]) -> Result<Vec<u8>> {
        check(self.command().args(args).output()?, args[0])
    }
}

fn send(
    input: &mut impl Write,
    response: &mut impl BufRead,
    command: &str,
    operation: &str,
) -> Result<()> {
    input.write_all(command.as_bytes())?;
    input.flush()?;
    let mut reply = String::new();
    response.read_line(&mut reply)?;
    ensure!(
        reply.trim_end() == format!("{operation}: ok"),
        "Git ref transaction could not {operation}"
    );
    Ok(())
}

fn is_ancestor(view: &IsolatedGit, ancestor: &str, tip: &str) -> Result<bool> {
    validate_oid(ancestor)?;
    validate_oid(tip)?;
    let output = view
        .command()
        .args(["merge-base", "--is-ancestor", ancestor, tip])
        .output()?;
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(false);
    }
    check(output, "verify integration ancestry")?;
    Ok(true)
}

fn merge_tree(view: &IsolatedGit, destination: &str, source: &str) -> Result<Option<String>> {
    let output = view
        .command()
        .env("GIT_ATTR_SOURCE", destination)
        .args([
            "merge-tree",
            "--write-tree",
            "--no-messages",
            destination,
            source,
        ])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Ok(Some(text_oid(check(
        output,
        "prepare the approved integration tree",
    )?)?))
}

fn read_commit(view: &IsolatedGit, sha: &str) -> Result<Commit> {
    validate_oid(sha)?;
    ensure!(
        view.read(&["cat-file", "-t", sha])?.trim_ascii_end() == b"commit",
        "Git evidence must be a commit"
    );
    let raw = view.read(&["cat-file", "-p", sha])?;
    let mut tree = None;
    let mut parents = Vec::new();
    for line in raw
        .split(|byte| *byte == b'\n')
        .take_while(|line| !line.is_empty())
    {
        if let Some(bytes) = line.strip_prefix(b"tree ") {
            tree = Some(text_oid(bytes.to_vec())?);
        } else if let Some(bytes) = line.strip_prefix(b"parent ") {
            parents.push(text_oid(bytes.to_vec())?);
        }
    }
    Ok(Commit {
        tree: tree.context("Git commit is missing its tree")?,
        parents,
    })
}

fn text_oid(bytes: Vec<u8>) -> Result<String> {
    let text = String::from_utf8(bytes)?;
    let oid = text.trim_end();
    validate_oid(oid)?;
    Ok(oid.into())
}

pub(super) fn validate_oid(oid: &str) -> Result<()> {
    ensure!(
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a full Git object SHA is required"
    );
    Ok(())
}
