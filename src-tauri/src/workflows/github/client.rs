use std::{future::Future, path::Path};

use anyhow::{Context, Result};

use crate::repository::github::{
    CiSnapshot, GitHubAccount, GitHubRepository, GithubCli, MergeOutcome, PublicRepository,
    PullRequest, PullRequestRequest, RemoteMergeProof, RepositorySlug, RequiredCheck,
};

pub(crate) trait GithubDeliveryClient: Sync {
    fn account(&self) -> impl Future<Output = Result<GitHubAccount>> + Send;
    fn repository(
        &self,
        repo: &RepositorySlug,
    ) -> impl Future<Output = Result<GitHubRepository>> + Send;
    fn validate_remote(
        &self,
        repo: &RepositorySlug,
        remote: &str,
        checkout: &Path,
    ) -> impl Future<Output = Result<()>> + Send;
    fn search_public_repositories(
        &self,
        query: &str,
        limit: u16,
    ) -> impl Future<Output = Result<Vec<PublicRepository>>> + Send;
    fn branch_head_optional(
        &self,
        repo: &RepositorySlug,
        branch: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send;
    fn branch_head(
        &self,
        repo: &RepositorySlug,
        branch: &str,
    ) -> impl Future<Output = Result<String>> + Send {
        async move {
            self.branch_head_optional(repo, branch)
                .await?
                .context("The requested remote GitHub branch is missing")
        }
    }
    fn fetch_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        checkout: &Path,
    ) -> impl Future<Output = Result<()>> + Send;
    fn publish_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        previous_head: Option<&str>,
        checkout: &Path,
    ) -> impl Future<Output = Result<()>> + Send;
    fn find_pull_request(
        &self,
        repo: &RepositorySlug,
        head: &str,
        base: &str,
        task_key: &str,
    ) -> impl Future<Output = Result<Option<PullRequest>>> + Send;
    fn open_or_update_pull_request(
        &self,
        repo: &RepositorySlug,
        request: &PullRequestRequest,
    ) -> impl Future<Output = Result<PullRequest>> + Send;
    fn pull_request(
        &self,
        repo: &RepositorySlug,
        number: u64,
    ) -> impl Future<Output = Result<PullRequest>> + Send;
    fn inspect_ci(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        required: &[RequiredCheck],
    ) -> impl Future<Output = Result<CiSnapshot>> + Send;
    fn merge_pull_request(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
    ) -> impl Future<Output = Result<MergeOutcome>> + Send;
    fn verify_merge(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
    ) -> impl Future<Output = Result<Option<RemoteMergeProof>>> + Send;
    fn poll_merge(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
        request: &str,
    ) -> impl Future<Output = Result<MergeOutcome>> + Send;
}

impl GithubDeliveryClient for GithubCli {
    async fn account(&self) -> Result<GitHubAccount> {
        self.account().await
    }

    async fn repository(&self, repo: &RepositorySlug) -> Result<GitHubRepository> {
        self.repository(repo).await
    }

    async fn validate_remote(
        &self,
        repo: &RepositorySlug,
        remote: &str,
        checkout: &Path,
    ) -> Result<()> {
        self.validate_remote(repo, remote, checkout).await
    }

    async fn search_public_repositories(
        &self,
        query: &str,
        limit: u16,
    ) -> Result<Vec<PublicRepository>> {
        self.search_public_repositories(query, limit).await
    }

    async fn branch_head_optional(
        &self,
        repo: &RepositorySlug,
        branch: &str,
    ) -> Result<Option<String>> {
        self.branch_head_optional(repo, branch).await
    }

    async fn fetch_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        checkout: &Path,
    ) -> Result<()> {
        self.fetch_branch(repo, branch, head, checkout).await
    }

    async fn publish_branch(
        &self,
        repo: &RepositorySlug,
        branch: &str,
        head: &str,
        previous_head: Option<&str>,
        checkout: &Path,
    ) -> Result<()> {
        self.publish_branch(repo, branch, head, previous_head, checkout)
            .await
    }

    async fn find_pull_request(
        &self,
        repo: &RepositorySlug,
        head: &str,
        base: &str,
        task_key: &str,
    ) -> Result<Option<PullRequest>> {
        self.find_pull_request(repo, head, base, task_key).await
    }

    async fn open_or_update_pull_request(
        &self,
        repo: &RepositorySlug,
        request: &PullRequestRequest,
    ) -> Result<PullRequest> {
        self.open_or_update_pull_request(repo, request).await
    }

    async fn pull_request(&self, repo: &RepositorySlug, number: u64) -> Result<PullRequest> {
        self.pull_request(repo, number).await
    }

    async fn inspect_ci(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        required: &[RequiredCheck],
    ) -> Result<CiSnapshot> {
        self.inspect_ci(repo, pull, required).await
    }

    async fn merge_pull_request(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
    ) -> Result<MergeOutcome> {
        self.merge_pull_request(repo, pull, ci).await
    }

    async fn verify_merge(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
    ) -> Result<Option<RemoteMergeProof>> {
        self.verify_merge(repo, pull, ci).await
    }

    async fn poll_merge(
        &self,
        repo: &RepositorySlug,
        pull: &PullRequest,
        ci: &CiSnapshot,
        request: &str,
    ) -> Result<MergeOutcome> {
        self.poll_merge(repo, pull, ci, request).await
    }
}
