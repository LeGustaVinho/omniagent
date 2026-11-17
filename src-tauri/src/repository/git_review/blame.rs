use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

use crate::store::Store;

use super::{GitFileSide, LiveReview, check, content, validate_oid, validate_path};

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum GitLineBlame {
    Committed {
        head: String,
        commit: String,
        author: String,
        authored_at: i64,
        summary: String,
        head_line: usize,
        original_line: usize,
    },
    Uncommitted {
        reason: &'static str,
    },
    Unavailable {
        reason: String,
    },
}

impl Store {
    pub async fn git_checkout_line_blame(
        &self,
        checkout: String,
        path: String,
        line: usize,
        editor_text: String,
    ) -> Result<GitLineBlame> {
        validate_path(&path)?;
        ensure!(line > 0, "Select a valid editor line.");
        ensure!(
            editor_text.len() as u64 <= content::MAX_DIFF_SIDE_BYTES && !editor_text.contains('\0'),
            "This document exceeds the Git line authorship text limit."
        );
        let scope = self.git_review_scope(checkout).await?;
        tokio::task::spawn_blocking(move || {
            let review = LiveReview::open(&scope)?;
            let result = read_line_blame(&review, &path, line, &editor_text)?;
            review.verify()?;
            Ok(result)
        })
        .await?
    }
}

fn read_line_blame(
    review: &LiveReview,
    path: &str,
    line: usize,
    editor_text: &str,
) -> Result<GitLineBlame> {
    let editor_lines = document_lines(editor_text);
    let trailing_line = editor_lines.is_empty() || editor_text.ends_with('\n');
    ensure!(
        line <= editor_lines.len() + usize::from(trailing_line),
        "Select a valid editor line."
    );
    let Some(head) = &review.revision.head else {
        return Ok(GitLineBlame::Uncommitted {
            reason: "This checkout has no commits yet.",
        });
    };
    let committed = match content::read_tree(&review.view, head, path)? {
        GitFileSide::Text { content } => content,
        GitFileSide::Missing => {
            return Ok(GitLineBlame::Uncommitted {
                reason: "This file has no version in the checkout HEAD.",
            });
        }
        GitFileSide::Unavailable { reason } => return Ok(GitLineBlame::Unavailable { reason }),
    };
    let committed_lines = document_lines(&committed);
    if committed_lines.iter().any(|line| line.contains('\r')) {
        return Ok(GitLineBlame::Unavailable { reason: "This revision uses carriage-return line breaks which cannot be matched to Git line numbers.".into() });
    }
    let Some(head_line) = matching_line(&committed_lines, &editor_lines, line) else {
        return Ok(GitLineBlame::Uncommitted {
            reason: "This editor line has no matching line in the checkout HEAD.",
        });
    };
    let range = format!("{head_line},{head_line}");
    let output = review
        .view
        .command()
        .env_remove("GIT_WORK_TREE")
        .args([
            "--bare",
            "--literal-pathspecs",
            "blame",
            "--line-porcelain",
            "--encoding=UTF-8",
            "--no-textconv",
            "-L",
            &range,
            head,
            "--",
            path,
        ])
        .output()?;
    parse_blame(
        &String::from_utf8(check(output, "read line authorship")?)?,
        head,
        head_line,
    )
}

fn document_lines(text: &str) -> Vec<&str> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .collect()
}

fn matching_line(committed: &[&str], editor: &[&str], line: usize) -> Option<usize> {
    // The editor normalizes BOM and CRLF. Match its line text to the pinned blob before asking Git for authorship.
    let index = line.checked_sub(1)?;
    editor.get(index)?;
    if editor == committed {
        return Some(line);
    }
    capture_diff_slices_deadline(
        Algorithm::Myers,
        committed,
        editor,
        Some(Instant::now() + Duration::from_millis(350)),
    )
    .into_iter()
    .find_map(|operation| match operation {
        DiffOp::Equal {
            old_index,
            new_index,
            len,
        } if (new_index..new_index + len).contains(&index) => {
            Some(old_index + index - new_index + 1)
        }
        _ => None,
    })
}

fn parse_blame(output: &str, head: &str, head_line: usize) -> Result<GitLineBlame> {
    let mut lines = output.lines();
    let mut header = lines
        .next()
        .context("Git returned no line authorship.")?
        .split_ascii_whitespace();
    let commit = header.next().context("Git returned no authoring commit.")?;
    validate_oid(commit)?;
    let original_line = header
        .next()
        .context("Git returned no original line number.")?
        .parse::<usize>()?;
    let final_line = header
        .next()
        .context("Git returned no final line number.")?
        .parse::<usize>()?;
    ensure!(
        original_line > 0 && final_line == head_line,
        "Git returned authorship for a different line."
    );
    let mut author = None;
    let mut authored_at = None;
    let mut summary = String::new();
    for line in lines.take_while(|line| !line.starts_with('\t')) {
        if let Some(value) = line.strip_prefix("author ") {
            author = Some(
                value
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(300)
                    .collect(),
            );
        } else if let Some(value) = line.strip_prefix("author-time ") {
            authored_at = Some(value.parse::<i64>()?);
        } else if let Some(value) = line.strip_prefix("summary ") {
            summary = value
                .chars()
                .filter(|ch| !ch.is_control())
                .take(600)
                .collect();
        }
    }
    Ok(GitLineBlame::Committed {
        head: head.to_owned(),
        commit: commit.to_owned(),
        original_line,
        head_line,
        summary,
        author: author.context("Git returned no line author.")?,
        authored_at: authored_at.context("Git returned no author timestamp.")?,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use crate::repository::{base_commit, git_path, git_query, register};

    use super::*;

    fn git(directory: &Path, args: &[&str]) -> Result<()> {
        git_query(&git_path()?, directory, args).map(|_| ())
    }

    fn commit(directory: &Path, path: &str, author: &str, subject: &str) -> Result<String> {
        git(
            directory,
            &[
                "-c",
                "core.autocrlf=false",
                "--literal-pathspecs",
                "add",
                "--",
                path,
            ],
        )?;
        git(
            directory,
            &[
                "-c",
                &format!("user.name={author}"),
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--quiet",
                "-m",
                subject,
            ],
        )?;
        base_commit(directory)?.context("fixture has no commit")
    }

    #[tokio::test]
    async fn uses_each_checkout_head_and_keeps_authorship_only_for_matching_editor_lines()
    -> Result<()> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("root");
        let linked = fixture.path().join("linked checkout");
        let path = "ação [draft].txt";
        fs::create_dir(&root)?;
        git(&root, &["init", "--quiet"])?;
        fs::write(root.join(path), "\u{feff}first\r\nsecond\r\n")?;
        let first = commit(&root, path, "Fixture Author", "Initial text")?;
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked.to_str().unwrap(),
            ],
        )?;
        fs::write(linked.join(path), "\u{feff}first\r\nupdated\r\n")?;
        let second = commit(&linked, path, "Linked Author", "Update the linked copy")?;
        let store = Store::open(fixture.path().join("profile.sqlite"))?;
        let primary = register(&store, root).await?;
        let worktree = register(&store, linked.clone()).await?;
        let blame = store
            .git_checkout_line_blame(
                primary.checkout_id,
                path.into(),
                2,
                "first\nsecond\n".into(),
            )
            .await?;
        assert!(
            matches!(blame, GitLineBlame::Committed { ref author, ref commit, ref head,
            head_line: 2, original_line: 2, .. } if author == "Fixture Author" && commit == &first && head == &first),
            "{blame:?}"
        );

        let dirty = "new editor line\nfirst\nupdated\n";
        let blame = store
            .git_checkout_line_blame(worktree.checkout_id.clone(), path.into(), 3, dirty.into())
            .await?;
        assert!(
            matches!(blame, GitLineBlame::Committed { ref author, ref commit, ref head, ref summary,
            head_line: 2, original_line: 2, .. } if author == "Linked Author" && commit == &second && head == &second
            && summary == "Update the linked copy"),
            "{blame:?}"
        );
        for (line, text) in [(1, dirty), (4, dirty), (2, "changed\nworking tree edit\n")] {
            let blame = store
                .git_checkout_line_blame(
                    worktree.checkout_id.clone(),
                    path.into(),
                    line,
                    text.into(),
                )
                .await?;
            assert!(
                matches!(blame, GitLineBlame::Uncommitted { .. }),
                "{blame:?}"
            );
        }
        fs::write(linked.join("untracked.md"), "Local line")?;
        let blame = store
            .git_checkout_line_blame(
                worktree.checkout_id.clone(),
                "untracked.md".into(),
                1,
                "Local line".into(),
            )
            .await?;
        assert!(
            matches!(blame, GitLineBlame::Uncommitted { reason } if reason.contains("no version"))
        );
        assert!(
            store
                .git_checkout_line_blame(
                    worktree.checkout_id,
                    "../outside.ts".into(),
                    1,
                    "Outside".into()
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn verifies_the_porcelain_line_number_and_handles_a_padded_virtual_line() {
        let output = "0000000000000000000000000000000000000000 1 6 1\nauthor Git Author\nauthor-time 200\nsummary Commit\n\tdata\n";
        assert!(parse_blame(output, "ignored", 5).is_err());
        assert_eq!(matching_line(&["first"], &["first"], 2), None);
        assert_eq!(
            matching_line(&["first", "second"], &["insert", "first", "second"], 2),
            Some(1)
        );
    }
}
