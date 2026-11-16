use std::{
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    protection,
    providers::configuration::{open_locked_local_file, plain_metadata},
    store::Store,
};

use super::{base_commit, checkout_root, documents::DocumentKind, verify_checkout};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryMarkdownRevision {
    pub id: String,
    pub checkout_id: String,
    pub relative_path: String,
    pub sha256: String,
    pub base_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryMarkdown {
    pub revision: RepositoryMarkdownRevision,
    pub markdown: String,
}

impl RepositoryMarkdown {
    pub(crate) fn append_to_prompt(&self, prompt: &mut String) -> Result<()> {
        let content = serde_json::to_string(self)?;
        prompt.push_str("\n\nRepository Markdown source\n");
        prompt.push_str(&content);
        Ok(())
    }
}

impl Store {
    pub async fn capture_repository_markdown(
        &self,
        checkout: String,
        selected: PathBuf,
    ) -> Result<RepositoryMarkdown> {
        let root = checkout_root(self, checkout.clone()).await?;
        let layout = verify_checkout(self, checkout.clone(), root).await?;
        let max = self
            .checkout_attachment_limits(checkout.clone())
            .await?
            .bytes_per_file;
        let (document, body) =
            tokio::task::spawn_blocking(move || capture(&layout.root, &selected, checkout, max))
                .await??;
        self.execute(move |db| {
            let revision = &document.revision;
            insert_revision(db, revision, &body)?;
            Ok(document)
        })
        .await
    }

    pub async fn repository_markdown_revisions(
        &self,
        checkout: String,
    ) -> Result<Vec<RepositoryMarkdownRevision>> {
        self.execute(move |db| {
            Ok(db.prepare(
                "SELECT id, checkout_id, path, sha256, base_commit FROM repository_markdown_revisions
                 WHERE checkout_id = ?1 AND substr(id, 1, 7) != 'source-' ORDER BY captured_at DESC, path, id",
            )?.query_map([checkout], decode_revision)?.collect::<rusqlite::Result<_>>()?)
        }).await
    }

    pub async fn repository_markdown(
        &self,
        checkout: String,
        revision: String,
    ) -> Result<Option<RepositoryMarkdown>> {
        self.checkout_document_revision(checkout, revision, DocumentKind::Markdown)
            .await
    }

    pub(super) async fn checkout_document_revision(
        &self,
        checkout: String,
        revision: String,
        kind: DocumentKind,
    ) -> Result<Option<RepositoryMarkdown>> {
        if !kind.contains_revision(&revision) {
            return Ok(None);
        }
        self.execute(move |db| load_markdown(db, &checkout, &revision))
            .await
    }

    pub(super) async fn remember_editor_document(
        &self,
        kind: DocumentKind,
        checkout_id: String,
        path: String,
        sha256: String,
        base_commit: Option<String>,
        markdown: String,
    ) -> Result<RepositoryMarkdownRevision> {
        self.execute(move |db| {
            let identity = serde_json::to_vec(&(&checkout_id, &path, &sha256, &base_commit))?;
            let id = format!("{}-{:x}", kind.editor_prefix(), Sha256::digest(identity));
            let existing = db
                .query_row(
                    "SELECT id, checkout_id, path, sha256, base_commit
                 FROM repository_markdown_revisions
                 WHERE checkout_id = ?1 AND id = ?2",
                    params![checkout_id, id],
                    decode_revision,
                )
                .optional()?;
            if let Some(revision) = existing {
                return Ok(revision);
            }
            let revision = RepositoryMarkdownRevision {
                id,
                checkout_id,
                relative_path: path,
                sha256,
                base_commit,
            };
            insert_revision(db, &revision, &protection::protect(markdown.as_bytes())?)?;
            Ok(revision)
        })
        .await
    }

    pub(crate) async fn dispatch_repository_markdown(
        &self,
        run: String,
    ) -> Result<Option<RepositoryMarkdown>> {
        self.execute(move |db| {
            let binding: Option<(String, String)> = db.query_row(
                "SELECT source.checkout_id, source.revision_id FROM job_attempts attempt
                 JOIN job_repository_markdown source ON source.job_id = attempt.job_id WHERE attempt.run_id = ?1",
                [run], |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            binding.map(|(checkout, revision)| {
                load_markdown(db, &checkout, &revision)?.context("the pinned Markdown revision is unavailable")
            }).transpose()
        }).await
    }
}

fn capture(
    root: &Path,
    selected: &Path,
    checkout: String,
    max: u64,
) -> Result<(RepositoryMarkdown, Vec<u8>)> {
    let path = dunce::simplified(selected)
        .strip_prefix(dunce::simplified(root))
        .context("select a Markdown file inside the current checkout")?;
    let relative = path.to_str().context("the Markdown path is not UTF-8")?;
    ensure!(
        !relative.contains(':')
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "select a Markdown file inside the current checkout"
    );
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    ensure!(
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown"),
        "select a Markdown file"
    );

    let source = root.join(path);
    ensure!(
        plain_metadata(&source)?.is_some_and(|entry| entry.is_file()),
        "the Markdown source must be a regular file"
    );
    let file = open_locked_local_file(&source)?;
    let mut content = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut content)?;
    ensure!(
        content.len() as u64 <= max,
        "the Markdown source exceeds the project file limit of {max} bytes"
    );
    let sha256 = format!("{:x}", Sha256::digest(&content));
    let markdown = String::from_utf8(content).context("the Markdown source must be UTF-8")?;
    ensure!(!markdown.contains('\0'), "the Markdown source must be text");
    let body = protection::protect(markdown.as_bytes())?;
    let revision = RepositoryMarkdownRevision {
        id: Uuid::new_v4().to_string(),
        checkout_id: checkout,
        relative_path: relative.replace('\\', "/"),
        sha256,
        base_commit: base_commit(root)?,
    };
    Ok((RepositoryMarkdown { revision, markdown }, body))
}

fn decode_revision(row: &Row<'_>) -> rusqlite::Result<RepositoryMarkdownRevision> {
    Ok(RepositoryMarkdownRevision {
        id: row.get(0)?,
        checkout_id: row.get(1)?,
        relative_path: row.get(2)?,
        sha256: row.get(3)?,
        base_commit: row.get(4)?,
    })
}

fn insert_revision(
    db: &Connection,
    revision: &RepositoryMarkdownRevision,
    body: &[u8],
) -> Result<()> {
    db.execute(
        "INSERT INTO repository_markdown_revisions (checkout_id, id, path, sha256, base_commit, body)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![revision.checkout_id, revision.id, revision.relative_path, revision.sha256, revision.base_commit, body],
    )?;
    Ok(())
}

fn load_markdown(
    db: &Connection,
    checkout: &str,
    revision: &str,
) -> Result<Option<RepositoryMarkdown>> {
    let row = db.query_row(
        "SELECT id, checkout_id, path, sha256, base_commit, body FROM repository_markdown_revisions
         WHERE checkout_id = ?1 AND id = ?2",
        params![checkout, revision], |row| Ok((decode_revision(row)?, row.get::<_, Vec<u8>>(5)?)),
    ).optional()?;
    row.map(|(revision, body)| {
        let markdown = String::from_utf8(protection::unprotect(&body)?)?;
        Ok(RepositoryMarkdown { revision, markdown })
    })
    .transpose()
}

pub(crate) fn bind_job_markdown(db: &Connection, job: &str, revision: &str) -> Result<()> {
    ensure!(
        DocumentKind::Markdown.contains_revision(revision),
        "jobs require a Markdown revision"
    );
    let changed = db
        .execute(
            "INSERT INTO job_repository_markdown (job_id, checkout_id, revision_id)
         SELECT id, checkout_id, ?2 FROM jobs WHERE id = ?1",
            params![job, revision],
        )
        .context("the Markdown revision must belong to the job's checkout")?;
    ensure!(
        changed == 1,
        "the Markdown revision requires a pipeline job"
    );
    Ok(())
}
