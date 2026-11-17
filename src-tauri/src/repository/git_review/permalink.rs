use std::fmt::Write;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::store::Store;

use super::{
    GitFileSide, LiveReview, content, history::require_commit, passive, validate_oid, validate_path,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPermalinkLines {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

impl Store {
    pub async fn git_file_permalink(
        &self,
        checkout: String,
        path: String,
        sha: Option<String>,
        lines: Option<GitPermalinkLines>,
    ) -> Result<String> {
        validate_path(&path)?;
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let sha = sha.or_else(|| review.revision.head.clone()).context("the selected checkout has no committed revision")?;
            validate_oid(&sha)?;
            require_commit(&review.view, &sha)?;
            let remote = passive(&scope.root, &["config", "--local", "--get", "remote.origin.url"])
                .context("the checkout has no origin remote")?;
            let remote = String::from_utf8(remote)?;
            let (host, repository) = remote_repository(remote.trim()).context("copying a permalink requires a GitHub.com or GitLab.com origin")?;
            if let Some(lines) = &lines {
                let GitFileSide::Text { content } = content::read_tree(&review.view, &sha, &path)? else {
                    anyhow::bail!("the selected lines are not available as text in that commit");
                };
                ensure!(lines.start_line >= 1 && lines.end_line >= lines.start_line, "select a valid line range");
                let length = lines.end_line - lines.start_line + 1;
                let committed = content.lines().skip(lines.start_line - 1).take(length).collect::<Vec<_>>();
                ensure!(committed.len() == length && committed.join("\n") == lines.text.replace("\r\n", "\n"),
                    "these lines differ from the committed file; copy a local line reference or choose the committed revision");
            } else {
                let tree = review.view.read(&["--literal-pathspecs", "ls-tree", "-z", &sha, "--", &path])?;
                let found = String::from_utf8(tree)?.split_terminator('\0').any(|entry| {
                    entry.split_once('\t').is_some_and(|(info, item)| item == path && info.split_ascii_whitespace().nth(1) == Some("blob"))
                });
                ensure!(found, "this file does not exist in the selected commit");
            }
            review.verify()?;
            Ok(permalink(host, &repository, &sha, &path, lines.as_ref()))
        }).await?
    }
}

fn remote_repository(remote: &str) -> Option<(&'static str, String)> {
    let (host, path) = if let Some((scheme, address)) = remote.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh") {
            return None;
        }
        let (authority, path) = address.split_once('/')?;
        let host_port = authority.rsplit('@').next()?;
        let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
        if !port.is_empty()
            && !matches!(
                (scheme, port),
                ("ssh", "22" | "443") | ("https", "443") | ("http", "80")
            )
        {
            return None;
        }
        (host, path)
    } else {
        let (authority, path) = remote.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let host = match host.to_ascii_lowercase().as_str() {
        "github.com" | "ssh.github.com" => "github.com",
        "gitlab.com" | "altssh.gitlab.com" => "gitlab.com",
        _ => return None,
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments = path.split('/').collect::<Vec<_>>();
    if segments.len() < 2
        || host == "github.com" && segments.len() != 2
        || segments.iter().any(|name| {
            name.is_empty()
                || matches!(*name, "." | "..")
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
    {
        return None;
    }
    Some((host, path.to_owned()))
}

fn permalink(
    host: &str,
    repository: &str,
    sha: &str,
    path: &str,
    lines: Option<&GitPermalinkLines>,
) -> String {
    let route = if host == "github.com" {
        "blob"
    } else {
        "-/blob"
    };
    let mut output = format!("https://{host}/{repository}/{route}/{sha}/");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
    if let Some(lines) = lines {
        if host == "github.com"
            && path.rsplit_once('.').is_some_and(|(_, ext)| {
                ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
            })
        {
            output.push_str("?plain=1");
        }
        let _ = write!(output, "#L{}", lines.start_line);
        if lines.end_line != lines.start_line {
            let separator = if host == "github.com" { "-L" } else { "-" };
            let _ = write!(output, "{separator}{}", lines.end_line);
        }
    }
    output
}
