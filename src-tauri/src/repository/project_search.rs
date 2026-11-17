use std::{
    env,
    ffi::OsStr,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    process::{self, TerminalOutcome, WindowsProcess, WindowsProcessControl},
    providers::configuration::plain_metadata,
    store::Store,
};

use super::{checkout_items::checkout_directory, checkout_root, verify_checkout};

#[cfg(test)]
mod tests;

const RIPGREP_SETTING: &str = "ripgrep_executable";
const MAX_QUERY_UNITS: usize = 4096;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(8);
const PROBE_OUTPUT_LIMIT: usize = 16 * 1024;
const SEARCH_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const RESULT_LIMIT: usize = 200;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepStatus {
    pub configured_executable: Option<PathBuf>,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub issue: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProjectSearch {
    Results {
        matches: Vec<ProjectSearchMatch>,
        truncated: bool,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ProjectSearchMatch {
    pub path: String,
    pub line: u32,
    pub excerpt: String,
}

impl Store {
    pub async fn ripgrep_status(&self) -> Result<RipgrepStatus> {
        let configured = self.ripgrep_executable().await?;
        Ok(tokio::task::spawn_blocking(move || inspect_ripgrep(configured)).await?)
    }

    pub async fn set_ripgrep_executable(
        &self,
        executable: Option<PathBuf>,
    ) -> Result<RipgrepStatus> {
        let status = tokio::task::spawn_blocking(move || inspect_ripgrep(executable)).await?;
        let saved = match &status.configured_executable {
            Some(_) => Some(
                status
                    .executable
                    .as_deref()
                    .with_context(|| {
                        status
                            .issue
                            .clone()
                            .unwrap_or_else(|| "The ripgrep executable is unavailable".into())
                    })?
                    .to_str()
                    .context("The ripgrep path is not UTF-8")?
                    .to_owned(),
            ),
            None => None,
        };
        self.execute({
            let saved = saved.clone();
            move |db| {
                if let Some(path) = saved {
                    db.execute(
                        "INSERT INTO workspace_settings (key, value) VALUES (?1, ?2)
                         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                        params![RIPGREP_SETTING, path],
                    )?;
                } else {
                    db.execute(
                        "DELETE FROM workspace_settings WHERE key = ?1",
                        [RIPGREP_SETTING],
                    )?;
                }
                Ok(())
            }
        })
        .await?;
        Ok(RipgrepStatus {
            configured_executable: saved.map(PathBuf::from),
            ..status
        })
    }

    pub async fn search_repository_files(
        &self,
        checkout_id: String,
        query: String,
    ) -> Result<ProjectSearch> {
        self.search_repository_files_in(checkout_id, query, String::new())
            .await
    }

    pub async fn search_repository_files_in(
        &self,
        checkout_id: String,
        query: String,
        relative_directory: String,
    ) -> Result<ProjectSearch> {
        validate_query(&query)?;
        let root = checkout_root(self, checkout_id.clone()).await?;
        let layout = verify_checkout(self, checkout_id, root).await?;
        let configured = self.ripgrep_executable().await?;
        tokio::task::spawn_blocking(move || {
            let status = inspect_ripgrep(configured);
            let Some(executable) = status.executable else {
                return Ok(ProjectSearch::Unavailable {
                    reason: status
                        .issue
                        .unwrap_or_else(|| "ripgrep is unavailable".into()),
                });
            };
            let directory = checkout_directory(&layout.root, &relative_directory)?;
            let target = Path::new(".").join(directory.strip_prefix(&layout.root)?);
            search(&executable, &layout.root, &query, target.as_os_str())
        })
        .await?
    }

    async fn ripgrep_executable(&self) -> Result<Option<PathBuf>> {
        self.execute(|db| {
            let path: Option<String> = db
                .query_row(
                    "SELECT value FROM workspace_settings WHERE key = ?1",
                    [RIPGREP_SETTING],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(path.map(PathBuf::from))
        })
        .await
    }
}

fn validate_query(query: &str) -> Result<()> {
    ensure!(
        !query.is_empty() && !query.contains(['\r', '\n', '\0']),
        "Enter a single line of text to search"
    );
    ensure!(
        query.encode_utf16().count() <= MAX_QUERY_UNITS,
        "The search text is too long"
    );
    Ok(())
}

fn inspect_ripgrep(configured_executable: Option<PathBuf>) -> RipgrepStatus {
    let candidate = match &configured_executable {
        Some(path) => verified_executable(path),
        None => discover_ripgrep().context("ripgrep is not on PATH."),
    };
    let qualified = candidate.and_then(|path| {
        let version = probe(&path)?;
        Ok((path, version))
    });
    match qualified {
        Ok((executable, version)) => RipgrepStatus {
            configured_executable,
            executable: Some(executable),
            version: Some(version),
            issue: None,
        },
        Err(error) => RipgrepStatus {
            configured_executable,
            executable: None,
            version: None,
            issue: Some(error.to_string()),
        },
    }
}

fn discover_ripgrep() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter(|path| path.is_absolute())
        .find_map(|directory| verified_executable(&directory.join("rg.exe")).ok())
}

fn verified_executable(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
        "Select an absolute path to the Windows ripgrep executable (.exe)"
    );
    let canonical = dunce::canonicalize(path)
        .context("The configured ripgrep executable could not be opened")?;
    ensure!(
        plain_metadata(&canonical)
            .ok()
            .flatten()
            .is_some_and(|metadata| metadata.is_file()),
        "The ripgrep executable must be a local file"
    );
    Ok(canonical)
}

fn probe(executable: &Path) -> Result<String> {
    let cwd = executable
        .parent()
        .context("The ripgrep executable has no parent directory")?;
    let capture = run_bounded(
        executable,
        cwd,
        &[OsStr::new("--no-config"), OsStr::new("--version")],
        PROBE_TIMEOUT,
        PROBE_OUTPUT_LIMIT,
    )
    .context("The ripgrep version check failed")?;
    ensure!(
        !capture.truncated && capture.outcome == TerminalOutcome::Exited(0),
        "The ripgrep version check failed"
    );
    String::from_utf8_lossy(&capture.stdout)
        .lines()
        .next()
        .filter(|version| version.starts_with("ripgrep ") && version.len() > "ripgrep ".len())
        .map(str::to_owned)
        .context("The selected executable did not report a ripgrep version")
}

fn search(executable: &Path, root: &Path, query: &str, target: &OsStr) -> Result<ProjectSearch> {
    let args = [
        "--no-config",
        "--no-follow",
        "--no-ignore-parent",
        "--no-messages",
        "--json",
        "--line-number",
        "--ignore-case",
        "--fixed-strings",
        "--glob",
        "!.git",
        "--glob",
        "!**/.git/**",
        "--",
        query,
    ]
    .map(OsStr::new);
    let args = [args.as_slice(), &[target]].concat();
    let capture = run_bounded(executable, root, &args, SEARCH_TIMEOUT, SEARCH_OUTPUT_LIMIT)?;
    ensure!(
        capture.truncated || matches!(capture.outcome, TerminalOutcome::Exited(0 | 1)),
        "ripgrep could not search this checkout"
    );
    let (matches, limited) = read_matches(root, &capture.stdout)?;
    Ok(ProjectSearch::Results {
        matches,
        truncated: capture.truncated || limited,
    })
}

struct Capture {
    outcome: TerminalOutcome,
    stdout: Vec<u8>,
    truncated: bool,
}

fn run_bounded(
    executable: &Path,
    cwd: &Path,
    args: &[&OsStr],
    timeout: Duration,
    max_stdout: usize,
) -> Result<Capture> {
    let WindowsProcess {
        stdin,
        stdout,
        stderr,
        control,
        ..
    } = process::launch(executable, cwd, args).context("Start ripgrep")?;
    drop(stdin);
    let (finished, completed) = mpsc::channel();
    thread::scope(|scope| {
        let timeout_control = &control;
        scope.spawn(move || {
            if matches!(
                completed.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let _ = timeout_control.cancel();
            }
        });
        let output = scope.spawn(|| bounded_pipe(stdout, max_stdout, &control));
        let errors = scope.spawn(|| bounded_pipe(stderr, PROBE_OUTPUT_LIMIT, &control));
        let outcome = control.wait();
        if outcome.is_err() {
            let _ = control.cancel();
        }
        let _ = finished.send(());
        let (stdout, limited_stdout) = output
            .join()
            .map_err(|_| anyhow::anyhow!("Read ripgrep output"))??;
        let (_, limited_stderr) = errors
            .join()
            .map_err(|_| anyhow::anyhow!("Read ripgrep errors"))??;
        let outcome = outcome?;
        let truncated =
            limited_stdout || limited_stderr || matches!(outcome, TerminalOutcome::Cancelled(_));
        Ok(Capture {
            outcome,
            stdout,
            truncated,
        })
    })
}

fn bounded_pipe(
    pipe: impl Read,
    limit: usize,
    control: &WindowsProcessControl,
) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let read = pipe.take(limit as u64 + 1).read_to_end(&mut output);
    let limited = output.len() > limit;
    if read.is_err() || limited {
        let _ = control.cancel();
    }
    read?;
    output.truncate(limit);
    Ok((output, limited))
}

#[derive(Deserialize)]
struct RipgrepFrame {
    #[serde(rename = "type")]
    kind: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct RipgrepMatch {
    path: Text,
    lines: Text,
    line_number: u32,
    submatches: Vec<Submatch>,
}

#[derive(Deserialize)]
struct Text {
    text: Option<String>,
}

#[derive(Deserialize)]
struct Submatch {
    start: usize,
}

fn read_matches(root: &Path, output: &[u8]) -> Result<(Vec<ProjectSearchMatch>, bool)> {
    let mut matches = Vec::new();
    for frame in output.split_inclusive(|byte| *byte == b'\n') {
        if !frame.ends_with(b"\n") {
            break;
        }
        let frame: RipgrepFrame =
            serde_json::from_slice(frame).context("Invalid ripgrep output")?;
        if frame.kind != "match" {
            continue;
        }
        let matched: RipgrepMatch =
            serde_json::from_value(frame.data).context("Invalid ripgrep match")?;
        let (Some(path), Some(text)) = (matched.path.text, matched.lines.text) else {
            continue;
        };
        let Some(path) = safe_path(root, &path) else {
            continue;
        };
        if matched.line_number == 0 {
            continue;
        }
        if matches.len() == RESULT_LIMIT {
            return Ok((ordered(matches), true));
        }
        matches.push(ProjectSearchMatch {
            path,
            line: matched.line_number,
            excerpt: excerpt(&text, matched.submatches.first().map(|entry| entry.start)),
        });
    }
    Ok((ordered(matches), false))
}

fn ordered(mut matches: Vec<ProjectSearchMatch>) -> Vec<ProjectSearchMatch> {
    matches.sort_by(|left, right| (&left.path, left.line).cmp(&(&right.path, right.line)));
    matches
}

fn safe_path(root: &Path, reported: &str) -> Option<String> {
    let relative = Path::new(reported);
    let relative = relative.strip_prefix(".").unwrap_or(relative);
    if relative.as_os_str().is_empty()
        || reported.contains(':')
        || !relative.components().all(
            |part| matches!(part, Component::Normal(part) if !part.eq_ignore_ascii_case(".git")),
        )
    {
        return None;
    }
    // Verify every component without following junctions or other reparse points, even when a command reports a path.
    plain_metadata(&root.join(relative))
        .ok()
        .flatten()
        .filter(|entry| entry.is_file())?;
    Some(relative.to_str()?.replace('\\', "/"))
}

fn excerpt(line: &str, first_match: Option<usize>) -> String {
    let line = line.trim_end_matches(['\r', '\n']);
    let before = first_match
        .and_then(|offset| line.get(..offset))
        .unwrap_or_default();
    let start = before
        .char_indices()
        .rev()
        .nth(80)
        .map_or(0, |(index, _)| index);
    let remaining = &line[start..];
    let end = remaining
        .char_indices()
        .nth(320)
        .map_or(remaining.len(), |(index, _)| index);
    let prefix = if start == 0 { "" } else { "…" };
    let suffix = if end == remaining.len() { "" } else { "…" };
    format!("{prefix}{}{suffix}", &remaining[..end])
}
