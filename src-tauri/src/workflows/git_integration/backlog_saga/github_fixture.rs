use std::{collections::BTreeMap, sync::Mutex};

use crate::{
    repository::github::{
        CheckEvidence, CiSnapshot, CiState, GitHubAccount, GitHubRepository, MergeOutcome,
        PublicRepository, PullRequest, PullRequestRequest, RemoteMergeProof, RepositorySlug,
        RequiredCheck,
    },
    workflows::github::{GithubDeliveryClient, GithubSettings},
};

use super::*;

pub(in crate::workflows::git_integration) const FAILED_CONTENT: &str =
    "integration rejected this head";
const REPOSITORY_ID: u64 = 11;
const ACCOUNT_ID: u64 = 22;
pub(in crate::workflows::git_integration) const MERGE_REQUEST: &str = "fixture-merge-request";

pub(in crate::workflows::git_integration) fn settings() -> GithubSettings {
    GithubSettings {
        account_id: ACCOUNT_ID,
        repository_id: REPOSITORY_ID,
        owner: "fixture".into(),
        repository: "backlog".into(),
        remote: "origin".into(),
        destination_branch: DESTINATION.into(),
        required_checks: vec![RequiredCheck {
            context: "required-ci".into(),
            app_id: Some(33),
        }],
        allow_automatic_merge: true,
        high_risk_paths: Some(vec![
            "first.txt".into(),
            "second.txt".into(),
            "last.txt".into(),
        ]),
    }
}

#[derive(Default)]
struct Remote {
    refs: BTreeMap<String, String>,
    requests: BTreeMap<u64, (String, PullRequest)>,
    merges: usize,
    ci_waited: bool,
    pending_merge: Option<u64>,
}

pub(in crate::workflows::git_integration) struct LocalGithub {
    root: PathBuf,
    slug: RepositorySlug,
    remote: Mutex<Remote>,
}

impl LocalGithub {
    pub(in crate::workflows::git_integration) fn new(root: &Path, tip: String) -> Self {
        let mut remote = Remote::default();
        remote.refs.insert(DESTINATION.into(), tip);
        Self {
            root: root.into(),
            slug: RepositorySlug {
                owner: "fixture".into(),
                name: "backlog".into(),
            },
            remote: Mutex::new(remote),
        }
    }

    pub(in crate::workflows::git_integration) fn merges(&self) -> usize {
        self.remote.lock().unwrap().merges
    }

    fn integrate(&self, pull: &PullRequest) -> Result<(String, String)> {
        let tree = git(
            &self.root,
            &["merge-tree", "--write-tree", &pull.base_sha, &pull.head_sha],
        )?;
        let commit = git(
            &self.root,
            &[
                "-c",
                "user.name=Fixture GitHub",
                "-c",
                "user.email=github@omniagent.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit-tree",
                &tree,
                "-p",
                &pull.base_sha,
                "-p",
                &pull.head_sha,
                "-m",
                "Fixture integration of exact approved parents",
            ],
        )?;
        Ok((tree, commit))
    }
}

impl GithubDeliveryClient for LocalGithub {
    async fn account(&self) -> Result<GitHubAccount> {
        Ok(GitHubAccount {
            id: ACCOUNT_ID,
            login: "fixture-operator".into(),
        })
    }

    async fn repository(&self, repo: &RepositorySlug) -> Result<GitHubRepository> {
        ensure!(repo == &self.slug);
        Ok(GitHubRepository {
            id: REPOSITORY_ID,
            slug: repo.clone(),
            default_branch: DESTINATION.into(),
        })
    }

    async fn validate_remote(
        &self,
        repo: &RepositorySlug,
        remote: &str,
        checkout: &Path,
    ) -> Result<()> {
        ensure!(repo == &self.slug && remote == "origin" && checkout.is_dir());
        Ok(())
    }

    async fn search_public_repositories(
        &self,
        query: &str,
        limit: u16,
    ) -> Result<Vec<PublicRepository>> {
        ensure!(!query.trim().is_empty() && limit > 0);
        let slug = RepositorySlug {
            owner: "fixture".into(),
            name: "reference".into(),
        };
        Ok(vec![PublicRepository {
            url: slug.url(),
            repository: GitHubRepository {
                id: 44,
                slug,
                default_branch: "main".into(),
            },
            description: Some("A fixture repository for pinned public research".into()),
            license: Some("MIT".into()),
            revision: self.branch_head(&self.slug, DESTINATION).await?,
        }])
    }

    async fn branch_head_optional(
        &self,
        repo: &RepositorySlug,
        branch: &str,
    ) -> Result<Option<String>> {
        ensure!(repo == &self.slug);
        Ok(self.remote.lock().unwrap().refs.get(branch).cloned())
    }

    async fn fetch_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        checkout: &Path,
    ) -> Result<()> {
        ensure!(self.branch_head(repo, branch).await? == head);
        git(checkout, &["cat-file", "-e", head])?;
        Ok(())
    }

    async fn publish_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        previous: Option<&str>,
        checkout: &Path,
    ) -> Result<()> {
        ensure!(repo == &self.slug);
        git(checkout, &["merge-base", "--is-ancestor", "HEAD", head])?;
        if let Some(previous) = previous {
            git(checkout, &["merge-base", "--is-ancestor", previous, head])?;
        }
        let mut remote = self.remote.lock().unwrap();
        ensure!(
            remote.refs.get(branch).map(String::as_str) == previous,
            "The stale publication must not overwrite another head"
        );
        remote.refs.insert(branch.into(), head.into());
        Ok(())
    }

    async fn find_pull_request(
        &self,
        repo: &RepositorySlug,
        head: &str,
        base: &str,
        task_key: &str,
    ) -> Result<Option<PullRequest>> {
        ensure!(repo == &self.slug);
        Ok(self
            .remote
            .lock()
            .unwrap()
            .requests
            .values()
            .find(|(task, pull)| {
                task == task_key && pull.head_branch == head && pull.base_branch == base
            })
            .map(|(_, pull)| pull.clone()))
    }

    async fn open_or_update_pull_request(
        &self,
        repo: &RepositorySlug,
        request: &PullRequestRequest,
    ) -> Result<PullRequest> {
        ensure!(repo == &self.slug);
        let mut remote = self.remote.lock().unwrap();
        let base = remote
            .refs
            .get(&request.base_branch)
            .context("Missing fixture destination")?
            .clone();
        ensure!(remote.refs.get(&request.head_branch) == Some(&request.head_sha));
        if let Some((task, pull)) = remote
            .requests
            .values_mut()
            .find(|(_, pull)| pull.head_branch == request.head_branch)
        {
            ensure!(*task == request.task_key && pull.open && !pull.merged);
            pull.head_sha.clone_from(&request.head_sha);
            pull.base_sha = base;
            return Ok(pull.clone());
        }
        let number = remote.requests.len() as u64 + 1;
        let pull = PullRequest {
            number,
            id: format!("fixture-pr-{number}"),
            repository_id: REPOSITORY_ID,
            head_branch: request.head_branch.clone(),
            head_sha: request.head_sha.clone(),
            base_branch: request.base_branch.clone(),
            base_sha: base,
            merge_commit_sha: None,
            merged: false,
            open: true,
            draft: false,
            url: format!("https://github.com/fixture/backlog/pull/{number}"),
        };
        remote
            .requests
            .insert(number, (request.task_key.clone(), pull.clone()));
        Ok(pull)
    }

    async fn pull_request(&self, repo: &RepositorySlug, number: u64) -> Result<PullRequest> {
        ensure!(repo == &self.slug);
        Ok(self
            .remote
            .lock()
            .unwrap()
            .requests
            .get(&number)
            .context("Missing fixture PR")?
            .1
            .clone())
    }

    async fn inspect_ci(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        required: &[RequiredCheck],
    ) -> Result<CiSnapshot> {
        ensure!(repo == &self.slug && self.pull_request(repo, pull.number).await? == *pull);
        {
            let mut remote = self.remote.lock().unwrap();
            if !remote.ci_waited {
                remote.ci_waited = true;
                return Ok(CiSnapshot {
                    pull_number: pull.number,
                    head_sha: pull.head_sha.clone(),
                    base_sha: pull.base_sha.clone(),
                    tested_sha: String::new(),
                    tested_tree: None,
                    state: CiState::Pending,
                    checks: Vec::new(),
                    required_checks: required.to_vec(),
                    reason: Some("Fixture CI is still running".into()),
                });
            }
        }
        // Keep the check-run identity stable between CI and the pre-merge revalidation.
        let reference = format!("refs/fixture/ci/{}/{}", pull.base_sha, pull.head_sha);
        let tested =
            git(&self.root, &["rev-parse", "--verify", &reference]).or_else(|_| -> Result<_> {
                let (_, tested) = self.integrate(pull)?;
                git(&self.root, &["update-ref", &reference, &tested])?;
                Ok(tested)
            })?;
        let tree = git(&self.root, &["rev-parse", &format!("{tested}^{{tree}}")])?;
        let failed = git(&self.root, &["show", &format!("{tested}:first.txt")])? == FAILED_CONTENT;
        let state = if failed {
            CiState::Failed
        } else {
            CiState::Passed
        };
        let check_id = u64::from_str_radix(&pull.head_sha[..8], 16)? + 1;
        Ok(CiSnapshot {
            pull_number: pull.number,
            head_sha: pull.head_sha.clone(),
            base_sha: pull.base_sha.clone(),
            tested_sha: tested,
            tested_tree: Some(tree),
            state,
            required_checks: required.to_vec(),
            checks: required
                .iter()
                .map(|check| CheckEvidence {
                    context: check.context.clone(),
                    app_id: check.app_id,
                    state,
                    source: "check_run".into(),
                    id: check_id,
                })
                .collect(),
            reason: failed.then(|| "The required check rejected the tested merge".into()),
        })
    }

    async fn merge_pull_request(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
    ) -> Result<MergeOutcome> {
        ensure!(repo == &self.slug && ci.state == CiState::Passed && ci.head_sha == pull.head_sha);
        let mut remote = self.remote.lock().unwrap();
        ensure!(
            remote.pending_merge.is_none(),
            "The app submitted the merge twice"
        );
        remote.pending_merge = Some(pull.number);
        Ok(MergeOutcome::Pending {
            request_id: Some(MERGE_REQUEST.into()),
        })
    }

    async fn poll_merge(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
        request: &str,
    ) -> Result<MergeOutcome> {
        ensure!(
            repo == &self.slug
                && request == MERGE_REQUEST
                && ci.state == CiState::Passed
                && ci.head_sha == pull.head_sha
        );
        let mut remote = self.remote.lock().unwrap();
        ensure!(
            remote.pending_merge.take() == Some(pull.number),
            "The app did not resume the exact pending merge"
        );
        ensure!(remote.refs.get(&pull.base_branch) == Some(&pull.base_sha));
        let (_, current) = remote
            .requests
            .get_mut(&pull.number)
            .context("Missing approved fixture PR")?;
        ensure!(*current == *pull);
        let (tree_sha, merge_sha) = self.integrate(pull)?;
        ensure!(ci.tested_tree.as_ref() == Some(&tree_sha));
        git(
            &self.root,
            &[
                "update-ref",
                &format!("refs/heads/{}", pull.base_branch),
                &merge_sha,
                &pull.base_sha,
            ],
        )?;
        current.merged = true;
        current.open = false;
        current.merge_commit_sha = Some(merge_sha.clone());
        remote
            .refs
            .insert(pull.base_branch.clone(), merge_sha.clone());
        remote.merges += 1;
        Ok(MergeOutcome::Verifiable(RemoteMergeProof {
            pull_number: pull.number,
            head_sha: pull.head_sha.clone(),
            base_sha: pull.base_sha.clone(),
            destination_sha: merge_sha.clone(),
            merge_sha,
            tree_sha,
        }))
    }

    async fn verify_merge(
        &self,
        _: &RepositorySlug,
        _: &PullRequest,
        _: &CiSnapshot,
    ) -> Result<Option<RemoteMergeProof>> {
        anyhow::bail!("The fixture records the native merge response immediately")
    }
}
