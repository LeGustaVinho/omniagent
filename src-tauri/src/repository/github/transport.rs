use std::{
    env,
    future::Future,
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::OnceCell;

use super::{GithubCli, RepositorySlug, validate_branch, validate_sha};
use crate::repository::{git_command, git_path, inspect, isolated_git::IsolatedGit};

const CREATE_NO_WINDOW: u32 = 0x08000000;
const API_TIMEOUT: Duration = Duration::from_secs(45);
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: u64 = 16 * 1024 * 1024;

pub(crate) trait GithubTransport: Send + Sync {
    fn request(
        &self,
        method: &str,
        endpoint: &str,
        payload: Option<Value>,
    ) -> impl Future<Output = Result<ApiResponse>> + Send;
}

pub(crate) struct ApiResponse {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) next_page: bool,
}

impl ApiResponse {
    pub(super) fn decode<T: DeserializeOwned>(self) -> Result<T> {
        self.success()?;
        serde_json::from_value(self.body)
            .map_err(|_| anyhow::anyhow!("GitHub returned an unexpected response"))
    }

    pub(super) fn success(&self) -> Result<()> {
        ensure!(
            (200..300).contains(&self.status),
            "GitHub API request returned HTTP {}",
            self.status
        );
        Ok(())
    }
}

pub(crate) struct GhTransport {
    executable: PathBuf,
    token: OnceCell<String>,
}

impl GhTransport {
    pub(super) fn discover() -> Result<Self> {
        let executable = env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .filter(|path| path.is_absolute())
            .map(|path| path.join("gh.exe"))
            .find(|path| path.is_file())
            .context("GitHub CLI (gh.exe) was not found on PATH")?;
        Ok(Self {
            executable,
            token: OnceCell::new(),
        })
    }

    async fn token(&self) -> Result<&str> {
        let token = self
            .token
            .get_or_try_init(|| async {
                let executable = self.executable.clone();
                tokio::task::spawn_blocking(move || -> Result<String> {
                    let mut command = gh_command(&executable);
                    command.args(["auth", "token", "--hostname", "github.com"]);
                    let output = run(command, None, API_TIMEOUT)?;
                    ensure!(
                        output.status.success(),
                        "Sign in to GitHub.com with gh before using the GitHub adapter"
                    );
                    let token = String::from_utf8(output.stdout).map_err(|_| {
                        anyhow::anyhow!("GitHub CLI returned an invalid credential")
                    })?;
                    let token = token.trim();
                    ensure!(
                        !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()),
                        "GitHub CLI returned an invalid credential"
                    );
                    Ok(token.to_owned())
                })
                .await?
            })
            .await?;
        Ok(token)
    }
}

impl GithubTransport for GhTransport {
    async fn request(
        &self,
        method: &str,
        endpoint: &str,
        payload: Option<Value>,
    ) -> Result<ApiResponse> {
        let token = self.token().await?.to_owned();
        let executable = self.executable.clone();
        let method = method.to_owned();
        let endpoint = endpoint.to_owned();
        tokio::task::spawn_blocking(move || {
            let input = payload
                .map(|payload| serde_json::to_vec(&payload))
                .transpose()?;
            let mut command = gh_command(&executable);
            command.env("GH_TOKEN", token).args([
                "api",
                "--hostname",
                "github.com",
                "--include",
                "--method",
                &method,
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "X-GitHub-Api-Version: 2026-03-10",
            ]);
            if input.is_some() {
                command.args(["--input", "-"]);
            }
            command.arg("--").arg(endpoint);
            let output = run(command, input.as_deref(), API_TIMEOUT)?;
            parse_api(output)
        })
        .await?
    }
}

impl<T: GithubTransport> GithubCli<T> {
    pub(crate) async fn validate_remote(
        &self,
        slug: &RepositorySlug,
        remote: &str,
        checkout: &Path,
    ) -> Result<()> {
        slug.validate()?;
        ensure!(
            !remote.is_empty()
                && !remote.starts_with('-')
                && remote.len() <= 128
                && remote
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
            "A configured local Git remote name is required"
        );
        let slug = slug.clone();
        let remote = remote.to_owned();
        let checkout = checkout.to_owned();
        tokio::task::spawn_blocking(move || {
            ensure!(checkout.is_absolute(), "An absolute managed checkout path is required");
            let git = git_path()?;
            for operation in [None, Some("--push")] {
                let mut command = git_command(&git, &checkout);
                command.args(["remote", "get-url", "--all"]);
                command.args(operation).arg(&remote);
                let output = run(command, None, API_TIMEOUT)?;
                ensure!(output.status.success(), "The configured Git remote is unavailable in this checkout");
                let stdout = String::from_utf8(output.stdout).map_err(|_| anyhow::anyhow!("The Git remote has an invalid URL"))?;
                let mut urls = stdout.lines();
                let first = urls.next().context("The Git remote does not have a URL")?;
                ensure!(urls.next().is_none() && RepositorySlug::from_remote_url(first)?.same(&slug),
                    "The checkout's fetch and push URLs must resolve to the configured GitHub repository");
            }
            Ok(())
        }).await?
    }
}

impl GithubCli {
    pub(crate) async fn publish_branch(
        &self,
        slug: &RepositorySlug,
        branch: &str,
        verified_sha: &str,
        prior: Option<&str>,
        checkout: &Path,
    ) -> Result<()> {
        validate_branch(branch)?;
        validate_sha(verified_sha)?;
        if let Some(prior) = prior {
            validate_sha(prior)?;
        }
        let remote = self.branch_head_optional(slug, branch).await?;
        let already_published = remote.as_deref() == Some(verified_sha);
        ensure!(
            already_published || remote.as_deref() == prior,
            "The remote GitHub branch changed outside its recorded publication"
        );
        let reference = format!("refs/heads/{branch}");
        let url = format!("{}.git", slug.url());
        let sha = verified_sha.to_owned();
        let prior = prior.map(str::to_owned);
        self.git_network(checkout, verified_sha, move |git, mut command| {
            ensure!(exact_commit(git, &sha)?, "The verified commit is missing from the managed checkout");
            if let Some(prior) = &prior {
                let mut ancestry = git.command();
                ancestry.args(["merge-base", "--is-ancestor", prior, &sha]);
                ensure!(run(ancestry, None, API_TIMEOUT)?.status.success(),
                    "GitHub branch publication must be a fast-forward from the preceding authorized revision");
            }
            if already_published {
                return Ok(());
            }
            command.args(["push", "--porcelain", "--no-verify"])
                .arg(format!("--force-with-lease={reference}:{}", prior.as_deref().unwrap_or("")))
                .arg(&url).arg(format!("{sha}:{reference}"));
            ensure!(run(command, None, GIT_TIMEOUT)?.status.success(), "GitHub branch publication failed or lost its atomic lease");
            Ok(())
        }).await?;
        ensure!(
            self.branch_head(slug, branch).await? == verified_sha,
            "The published GitHub branch is not at the verified revision"
        );
        Ok(())
    }

    pub(crate) async fn fetch_branch(
        &self,
        slug: &RepositorySlug,
        branch: &str,
        expected_sha: &str,
        checkout: &Path,
    ) -> Result<()> {
        validate_branch(branch)?;
        validate_sha(expected_sha)?;
        ensure!(
            self.branch_head(slug, branch).await? == expected_sha,
            "The GitHub destination branch advanced before fetching its objects"
        );
        let reference = format!("refs/heads/{branch}");
        let url = format!("{}.git", slug.url());
        let sha = expected_sha.to_owned();
        self.git_network(checkout, expected_sha, move |git, mut command| {
            command
                .args([
                    "fetch",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--no-write-fetch-head",
                    "--no-auto-maintenance",
                ])
                .arg(&url)
                .arg(&reference);
            ensure!(
                run(command, None, GIT_TIMEOUT)?.status.success(),
                "Could not fetch the verified GitHub destination objects"
            );
            ensure!(
                exact_commit(git, &sha)?,
                "The fetched GitHub destination revision is missing from Git's object store"
            );
            Ok(())
        })
        .await?;
        ensure!(
            self.branch_head(slug, branch).await? == expected_sha,
            "The GitHub destination branch advanced while fetching its objects"
        );
        Ok(())
    }

    async fn git_network(
        &self,
        checkout: &Path,
        head: &str,
        operation: impl FnOnce(&IsolatedGit, Command) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let token = self.transport.token().await?.to_owned();
        let executable = self.transport.executable.clone();
        let checkout = checkout.to_owned();
        let head = head.to_owned();
        tokio::task::spawn_blocking(move || {
            ensure!(
                checkout.is_absolute(),
                "An absolute managed checkout path is required"
            );
            let git = IsolatedGit::open(&inspect(&checkout)?, &checkout, &head)?;
            let mut command = git.command();
            gh_environment(&mut command);
            command
                .env("GH_TOKEN", token)
                .env("OMNIAGENT_GH_EXECUTABLE", executable)
                .env("GIT_ALLOW_PROTOCOL", "https")
                .env("GIT_LFS_SKIP_PUSH", "1")
                .env_remove("GIT_ASKPASS")
                .env_remove("SSH_ASKPASS")
                .env_remove("GIT_SSL_NO_VERIFY")
                .env_remove("GIT_TRACE")
                .env_remove("GIT_TRACE_CURL")
                .env_remove("GIT_TRACE_PACKET")
                .env_remove("GIT_CURL_VERBOSE")
                .env_remove("GIT_TRACE2")
                .env_remove("GIT_TRACE2_EVENT")
                .env_remove("GIT_TRACE2_PERF")
                .args([
                    "-c",
                    "credential.helper=",
                    "-c",
                    r#"credential.helper=!"$OMNIAGENT_GH_EXECUTABLE" auth git-credential"#,
                    "-c",
                    "http.followRedirects=false",
                    "-c",
                    "http.sslVerify=true",
                ]);
            operation(&git, command)
        })
        .await?
    }
}

fn exact_commit(git: &IsolatedGit, sha: &str) -> Result<bool> {
    let mut command = git.command();
    command.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{sha}^{{commit}}"),
    ]);
    let output = run(command, None, API_TIMEOUT)?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == sha)
}

fn gh_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.creation_flags(CREATE_NO_WINDOW);
    // Do not infer the host or repository from an arbitrary checkout.
    command.current_dir(env::temp_dir());
    gh_environment(&mut command);
    command
}

fn gh_environment(command: &mut Command) {
    for key in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_REPO",
        "GH_DEBUG",
        "DEBUG",
        "GH_FORCE_TTY",
        "GH_PAGER",
        "PAGER",
        "GH_SPINNER_DISABLED",
    ] {
        command.env_remove(key);
    }
    command
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1");
}

struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run(mut command: Command, input: Option<&[u8]>, timeout: Duration) -> Result<ProcessOutput> {
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| anyhow::anyhow!("Could not start the GitHub or Git command"))?;
    let deadline = Instant::now() + timeout;
    let stdout = child
        .stdout
        .take()
        .context("Could not read GitHub command output")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let output = stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(output);
    });
    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .context("Could not write GitHub command input")?;
        let input = input.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("The GitHub or Git command timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let bytes = receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .context("Could not finish reading GitHub command output")??;
    ensure!(
        bytes.len() as u64 <= OUTPUT_LIMIT,
        "The GitHub command exceeded its output limit"
    );
    Ok(ProcessOutput {
        status,
        stdout: bytes,
    })
}

fn parse_api(output: ProcessOutput) -> Result<ApiResponse> {
    let bytes = &output.stdout;
    let (split, separator) = [b"\r\n\r\n".as_slice(), b"\n\n".as_slice()]
        .into_iter()
        .filter_map(|separator| {
            bytes
                .windows(separator.len())
                .position(|window| window == separator)
                .map(|split| (split, separator))
        })
        .min_by_key(|(split, _)| *split)
        .context("GitHub CLI did not return an HTTP response")?;
    let headers = std::str::from_utf8(&bytes[..split])
        .map_err(|_| anyhow::anyhow!("GitHub returned invalid HTTP headers"))?;
    let mut lines = headers.lines();
    let mut status_line = lines.next().unwrap_or_default().split_whitespace();
    ensure!(
        status_line
            .next()
            .is_some_and(|prefix| prefix.starts_with("HTTP/")),
        "GitHub returned an invalid HTTP status"
    );
    let status = status_line
        .next()
        .and_then(|number| number.parse().ok())
        .context("GitHub returned an invalid HTTP status")?;
    ensure!(
        !(200..300).contains(&status) || output.status.success(),
        "GitHub CLI could not complete the API request"
    );
    let next_page = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("link") && value.contains("rel=\"next\"")
        })
    });
    let body = bytes[split + separator.len()..].trim_ascii();
    let body = if body.is_empty() || (!(200..300).contains(&status) && status != 409) {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .map_err(|_| anyhow::anyhow!("GitHub returned an invalid JSON response"))?
    };
    Ok(ApiResponse {
        status,
        body,
        next_page,
    })
}
