use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod ci;
mod merge;
mod transport;

pub use ci::{CheckEvidence, CiSnapshot, CiState, RequiredCheck};
pub(crate) use merge::{MergeOutcome, RemoteMergeProof};
#[cfg(test)]
pub(crate) use transport::ApiResponse;
pub(crate) use transport::{GhTransport, GithubTransport};

const PAGE_LIMIT: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositorySlug {
    pub owner: String,
    pub name: String,
}

impl RepositorySlug {
    pub(crate) fn from_remote_url(url: &str) -> Result<Self> {
        let path = [
            "https://github.com/",
            "git@github.com:",
            "ssh://git@github.com/",
        ]
        .into_iter()
        .find_map(|prefix| url.strip_prefix(prefix))
        .context("Use a GitHub.com HTTPS or SSH repository remote")?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path
            .split_once('/')
            .context("The GitHub remote needs an owner and repository")?;
        let slug = Self {
            owner: owner.into(),
            name: name.into(),
        };
        slug.validate()?;
        Ok(slug)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.owner.is_empty()
                && self.owner.len() <= 39
                && self
                    .owner
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !self.name.is_empty()
                && self.name.len() <= 100
                && !matches!(self.name.as_str(), "." | "..")
                && self
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
            "Invalid GitHub.com owner or repository name"
        );
        Ok(())
    }

    pub(crate) fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }

    fn same(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }

    fn endpoint(&self, suffix: &str) -> Result<String> {
        self.validate()?;
        Ok(format!("repos/{}/{}{suffix}", self.owner, self.name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubAccount {
    pub id: u64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepository {
    pub id: u64,
    pub slug: RepositorySlug,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicRepository {
    pub repository: GitHubRepository,
    pub url: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub id: String,
    pub repository_id: u64,
    pub head_branch: String,
    pub head_sha: String,
    pub base_branch: String,
    pub base_sha: String,
    pub merge_commit_sha: Option<String>,
    pub merged: bool,
    pub open: bool,
    pub draft: bool,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PullRequestRequest {
    pub head_branch: String,
    pub head_sha: String,
    pub base_branch: String,
    pub task_key: String,
    pub title: String,
    pub body: String,
}

#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}

#[derive(Deserialize)]
struct ApiRepository {
    id: u64,
    name: String,
    owner: ApiOwner,
    default_branch: String,
    private: bool,
    visibility: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct License {
    spdx_id: Option<String>,
    name: Option<String>,
}

impl ApiRepository {
    fn identity(&self) -> Result<GitHubRepository> {
        let slug = RepositorySlug {
            owner: self.owner.login.clone(),
            name: self.name.clone(),
        };
        slug.validate()?;
        ensure!(
            self.id != 0,
            "GitHub returned an invalid repository identity"
        );
        validate_branch(&self.default_branch)?;
        Ok(GitHubRepository {
            id: self.id,
            slug,
            default_branch: self.default_branch.clone(),
        })
    }

    fn public(&self) -> bool {
        !self.private
            && self
                .visibility
                .as_deref()
                .is_none_or(|value| value == "public")
    }
}

#[derive(Deserialize)]
struct ApiPullRepository {
    id: u64,
    name: String,
    owner: ApiOwner,
}

impl ApiPullRepository {
    fn matches(&self, slug: &RepositorySlug) -> bool {
        self.id != 0
            && self.name.eq_ignore_ascii_case(&slug.name)
            && self.owner.login.eq_ignore_ascii_case(&slug.owner)
    }
}

#[derive(Deserialize)]
struct ApiPullRef {
    #[serde(rename = "ref")]
    branch: String,
    sha: String,
    repo: Option<ApiPullRepository>,
}

#[derive(Deserialize)]
struct ApiPullRequest {
    number: u64,
    node_id: String,
    title: String,
    body: Option<String>,
    state: String,
    head: ApiPullRef,
    base: ApiPullRef,
    merge_commit_sha: Option<String>,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    draft: bool,
    mergeable: Option<bool>,
    stack: Option<Value>,
}

impl ApiPullRequest {
    fn publishable(&self, slug: &RepositorySlug) -> Result<PullRequest> {
        ensure!(
            self.stack.is_none(),
            "Stacked GitHub pull requests are outside this single-pull delivery"
        );
        let head = self
            .head
            .repo
            .as_ref()
            .context("The pull request head repository was removed")?;
        let base = self
            .base
            .repo
            .as_ref()
            .context("The pull request base repository was removed")?;
        ensure!(
            head.matches(slug) && base.matches(slug) && head.id == base.id,
            "The pull request does not use branches of the pinned GitHub.com repository"
        );
        validate_branch(&self.head.branch)?;
        validate_branch(&self.base.branch)?;
        validate_sha(&self.head.sha)?;
        validate_sha(&self.base.sha)?;
        if let Some(sha) = self.merge_commit_sha.as_deref() {
            validate_sha(sha)?;
        }
        ensure!(
            self.number != 0
                && !self.node_id.is_empty()
                && matches!(self.state.as_str(), "open" | "closed"),
            "GitHub returned an invalid pull request identity"
        );
        Ok(PullRequest {
            number: self.number,
            id: self.node_id.clone(),
            repository_id: base.id,
            head_branch: self.head.branch.clone(),
            head_sha: self.head.sha.clone(),
            base_branch: self.base.branch.clone(),
            base_sha: self.base.sha.clone(),
            merge_commit_sha: self.merge_commit_sha.clone(),
            merged: self.merged,
            open: self.state == "open",
            draft: self.draft,
            url: format!("{}/pull/{}", slug.url(), self.number),
        })
    }
}

pub(crate) struct GithubCli<T = GhTransport> {
    transport: T,
}

impl GithubCli {
    pub(crate) fn discover() -> Result<Self> {
        Ok(Self {
            transport: GhTransport::discover()?,
        })
    }
}

impl<T: GithubTransport> GithubCli<T> {
    #[cfg(test)]
    pub(crate) fn with_transport(transport: T) -> Self {
        Self { transport }
    }

    async fn get<D: DeserializeOwned>(&self, endpoint: &str) -> Result<D> {
        self.transport
            .request("GET", endpoint, None)
            .await?
            .decode()
    }

    async fn pages(&self, endpoint: &str) -> Result<Vec<Value>> {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let mut pages = Vec::new();
        for page in 1..=PAGE_LIMIT {
            let response = self
                .transport
                .request(
                    "GET",
                    &format!("{endpoint}{separator}per_page=100&page={page}"),
                    None,
                )
                .await?;
            let next = response.next_page;
            pages.push(response.decode()?);
            if !next {
                return Ok(pages);
            }
        }
        anyhow::bail!("The GitHub response exceeds the supported pagination limit")
    }

    pub(crate) async fn account(&self) -> Result<GitHubAccount> {
        let account: GitHubAccount = self.get("user").await?;
        ensure!(
            account.id != 0 && !account.login.trim().is_empty(),
            "GitHub returned an invalid account identity"
        );
        Ok(account)
    }

    async fn fetch_repository(&self, slug: &RepositorySlug) -> Result<ApiRepository> {
        let repository: ApiRepository = self.get(&slug.endpoint("")?).await?;
        ensure!(
            repository.identity()?.slug.same(slug),
            "The requested GitHub repository was renamed or redirected"
        );
        Ok(repository)
    }

    pub(crate) async fn repository(&self, slug: &RepositorySlug) -> Result<GitHubRepository> {
        self.fetch_repository(slug).await?.identity()
    }

    pub(crate) async fn search_public_repositories(
        &self,
        query: &str,
        limit: u16,
    ) -> Result<Vec<PublicRepository>> {
        #[derive(Deserialize)]
        struct Search {
            items: Vec<ApiRepository>,
        }
        ensure!(
            !query.trim().is_empty() && query.len() <= 256 && !query.contains(['\r', '\n', '\0']),
            "The public GitHub search needs a query of up to 256 bytes"
        );
        ensure!(
            (1..=30).contains(&limit),
            "The public GitHub search supports 1 to 30 results"
        );
        let query = encoded(&format!("{} is:public", query.trim()));
        let search: Search = self
            .get(&format!("search/repositories?q={query}&per_page={limit}"))
            .await?;
        let mut found = Vec::new();
        for item in search.items.into_iter().take(usize::from(limit)) {
            if !item.public() {
                continue;
            }
            let reference = item.identity()?;
            let before = self.fetch_repository(&reference.slug).await?;
            if !before.public() || before.id != reference.id {
                continue;
            }
            let repository = before.identity()?;
            let Some(revision) = self
                .branch_head_optional(&repository.slug, &repository.default_branch)
                .await?
            else {
                continue;
            };
            let license = self.license_at(&repository.slug, &revision).await?;
            found.push(PublicRepository {
                url: repository.slug.url(),
                repository,
                description: before.description,
                license,
                revision,
            });
        }
        Ok(found)
    }

    async fn license_at(&self, slug: &RepositorySlug, revision: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct File {
            license: Option<License>,
        }
        validate_sha(revision)?;
        let response = self
            .transport
            .request(
                "GET",
                &slug.endpoint(&format!("/license?ref={revision}"))?,
                None,
            )
            .await?;
        if response.status == 404 {
            return Ok(None);
        }
        let file: File = response.decode()?;
        Ok(file.license.and_then(|license| {
            license
                .spdx_id
                .filter(|id| id != "NOASSERTION")
                .or(license.name)
        }))
    }

    pub(crate) async fn branch_head_optional(
        &self,
        slug: &RepositorySlug,
        branch: &str,
    ) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Branch {
            name: String,
            commit: Sha,
        }
        validate_branch(branch)?;
        let response = self
            .transport
            .request(
                "GET",
                &slug.endpoint(&format!("/branches/{}", encoded(branch)))?,
                None,
            )
            .await?;
        if response.status == 404 {
            return Ok(None);
        }
        let result: Branch = response.decode()?;
        ensure!(result.name == branch, "GitHub returned a different branch");
        validate_sha(&result.commit.sha)?;
        Ok(Some(result.commit.sha))
    }

    pub(crate) async fn branch_head(&self, slug: &RepositorySlug, branch: &str) -> Result<String> {
        self.branch_head_optional(slug, branch)
            .await?
            .context("The requested remote GitHub branch is missing")
    }

    async fn read_pull(&self, slug: &RepositorySlug, number: u64) -> Result<ApiPullRequest> {
        ensure!(number != 0, "A GitHub pull request number is required");
        let pull: ApiPullRequest = self
            .get(&slug.endpoint(&format!("/pulls/{number}"))?)
            .await?;
        ensure!(
            pull.number == number,
            "GitHub returned a different pull request"
        );
        pull.publishable(slug)?;
        Ok(pull)
    }

    pub(crate) async fn pull_request(
        &self,
        slug: &RepositorySlug,
        number: u64,
    ) -> Result<PullRequest> {
        self.read_pull(slug, number).await?.publishable(slug)
    }

    async fn matching_pull(
        &self,
        slug: &RepositorySlug,
        head: &str,
        base: &str,
        marker: &str,
    ) -> Result<Option<ApiPullRequest>> {
        validate_branch(head)?;
        validate_branch(base)?;
        ensure!(
            head != base,
            "The pull request needs distinct head and destination branches"
        );
        let query = format!(
            "/pulls?state=all&head={}",
            encoded(&format!("{}:{head}", slug.owner))
        );
        let mut matched = None;
        for page in self.pages(&slug.endpoint(&query)?).await? {
            for item in serde_json::from_value::<Vec<ApiPullRequest>>(page)? {
                if item.head.branch != head {
                    continue;
                }
                if !item
                    .head
                    .repo
                    .as_ref()
                    .is_some_and(|repo| repo.matches(slug))
                {
                    continue;
                }
                let belongs = item.base.branch == base && has_marker(item.body.as_deref(), marker);
                ensure!(
                    belongs || item.state != "open",
                    "An open pull request already uses this branch for different work"
                );
                if belongs {
                    item.publishable(slug)?;
                    ensure!(
                        matched.replace(item.number).is_none(),
                        "More than one pull request matches the task identity and refs"
                    );
                }
            }
        }
        match matched {
            Some(number) => {
                let pull = self.read_pull(slug, number).await?;
                ensure!(
                    pull.head.branch == head
                        && pull.base.branch == base
                        && has_marker(pull.body.as_deref(), marker),
                    "The pull request changed during reconciliation"
                );
                Ok(Some(pull))
            }
            None => Ok(None),
        }
    }

    pub(crate) async fn find_pull_request(
        &self,
        slug: &RepositorySlug,
        head: &str,
        base: &str,
        task_key: &str,
    ) -> Result<Option<PullRequest>> {
        let marker = task_marker(task_key)?;
        self.matching_pull(slug, head, base, &marker)
            .await?
            .map(|pull| pull.publishable(slug))
            .transpose()
    }

    pub(crate) async fn open_or_update_pull_request(
        &self,
        slug: &RepositorySlug,
        request: &PullRequestRequest,
    ) -> Result<PullRequest> {
        validate_sha(&request.head_sha)?;
        ensure!(
            !request.title.trim().is_empty()
                && request.title.len() <= 256
                && request.body.len() <= 60 * 1024
                && !request.body.contains("<!-- omniagent-task:"),
            "Invalid GitHub pull request title or body"
        );
        let marker = task_marker(&request.task_key)?;
        let prior = self
            .matching_pull(slug, &request.head_branch, &request.base_branch, &marker)
            .await?;
        let actual = self.branch_head(slug, &request.head_branch).await?;
        ensure!(
            actual == request.head_sha,
            "The remote head changed before pull request publication"
        );
        let body = format!("{}\n\n{marker}", request.body.trim_end());
        let number = match prior {
            Some(pull) => {
                ensure!(
                    pull.head.sha == request.head_sha,
                    "The pull request has not observed the published head"
                );
                if pull.merged {
                    return pull.publishable(slug);
                }
                ensure!(
                    pull.state == "open",
                    "The matching GitHub pull request was closed"
                );
                if pull.title != request.title || pull.body.as_deref() != Some(body.as_str()) {
                    self.transport
                        .request(
                            "PATCH",
                            &slug.endpoint(&format!("/pulls/{}", pull.number))?,
                            Some(json!({"title":request.title,"body":body})),
                        )
                        .await?
                        .success()?;
                }
                pull.number
            }
            None => {
                let response = self.transport.request("POST", &slug.endpoint("/pulls")?,
                    Some(json!({"head":request.head_branch,"base":request.base_branch,"title":request.title,"body":body}))).await?;
                let created: ApiPullRequest = response.decode()?;
                created.publishable(slug)?.number
            }
        };
        let saved = self.read_pull(slug, number).await?;
        ensure!(
            saved.head.branch == request.head_branch
                && saved.head.sha == request.head_sha
                && saved.base.branch == request.base_branch
                && has_marker(saved.body.as_deref(), &marker),
            "The saved pull request does not match the authorized task and refs"
        );
        saved.publishable(slug)
    }
}

#[derive(Deserialize)]
struct Sha {
    sha: String,
}

#[derive(Deserialize)]
struct GitCommit {
    sha: String,
    tree: Sha,
    parents: Vec<Sha>,
}

impl<T: GithubTransport> GithubCli<T> {
    async fn commit(&self, slug: &RepositorySlug, sha: &str) -> Result<GitCommit> {
        validate_sha(sha)?;
        let commit: GitCommit = self
            .get(&slug.endpoint(&format!("/git/commits/{sha}"))?)
            .await?;
        ensure!(commit.sha == sha, "GitHub returned a different commit");
        validate_sha(&commit.tree.sha)?;
        for parent in &commit.parents {
            validate_sha(&parent.sha)?;
        }
        Ok(commit)
    }
}

fn task_marker(task: &str) -> Result<String> {
    ensure!(
        !task.trim().is_empty() && task.len() <= 256,
        "A stable task identity is required for the pull request"
    );
    Ok(format!(
        "<!-- omniagent-task:{:x} -->",
        Sha256::digest(task.as_bytes())
    ))
}

fn has_marker(body: Option<&str>, marker: &str) -> bool {
    body.is_some_and(|body| body.lines().filter(|line| line.trim() == marker).count() == 1)
}

fn validate_sha(sha: &str) -> Result<()> {
    ensure!(
        sha.len() == 40
            && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            && sha.bytes().any(|byte| byte != b'0'),
        "A full GitHub.com commit SHA is required"
    );
    Ok(())
}

pub(crate) fn validate_branch(branch: &str) -> Result<()> {
    ensure!(
        !branch.is_empty()
            && branch.len() <= 1024
            && !branch.starts_with('-')
            && !branch.starts_with("refs/")
            && !branch.ends_with('.')
            && !branch.contains("..")
            && !branch.contains("@{")
            && branch != "@"
            && branch
                .split('/')
                .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
            && !branch
                .bytes()
                .any(|byte| byte <= 32 || byte == 127 || b"~^:?*[\\".contains(&byte)),
        "A valid short GitHub branch name is required"
    );
    Ok(())
}

fn encoded(value: &str) -> String {
    use std::fmt::Write;
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

#[cfg(test)]
mod tests;
