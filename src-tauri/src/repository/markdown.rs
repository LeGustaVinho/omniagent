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
    providers::configuration::{
        expand_local_dos_path, open_locked_local_file, plain_metadata, strip_local_dos_prefix,
    },
    store::Store,
};

use super::{
    base_commit, checkout_root,
    documents::{DocumentKind, RawDocument},
    verify_checkout,
};

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
        bytes: Vec<u8>,
        before_save: Option<RepositoryMarkdownRevision>,
    ) -> Result<RepositoryMarkdownRevision> {
        self.execute(move |db| {
            let identity = serde_json::to_vec(&(&checkout_id, &path, &sha256, &base_commit))?;
            let revision = RepositoryMarkdownRevision {
                id: format!("{}-{:x}", kind.editor_prefix(), Sha256::digest(identity)),
                checkout_id,
                relative_path: path,
                sha256,
                base_commit,
            };
            let transaction = db.transaction()?;
            let existing: bool = transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM repository_markdown_revisions WHERE checkout_id = ?1 AND id = ?2)",
                params![revision.checkout_id, revision.id], |row| row.get(0),
            )?;
            if !existing {
                insert_revision(&transaction, &revision, &protection::protect(&bytes)?)?;
            }
            if let Some(before) = before_save.as_ref().filter(|before| before.sha256 != revision.sha256) {
                super::documents::history::append_save(&transaction, before, &revision)?;
            }
            transaction.commit()?;
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
    ensure!(
        !selected
            .components()
            .any(|part| matches!(part, Component::ParentDir)),
        "select a Markdown file inside the current checkout"
    );
    let selected = expand_local_dos_path(selected)
        .context("select a Markdown file inside the current checkout")?;
    let path = strip_local_dos_prefix(&selected, root)
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

pub(crate) fn load_markdown(
    db: &Connection,
    checkout: &str,
    revision: &str,
) -> Result<Option<RepositoryMarkdown>> {
    load_document_payload(db, checkout, revision)?
        .map(|(revision, bytes)| {
            let markdown = if DocumentKind::Source.contains_revision(&revision.id) {
                RawDocument::from_bytes(DocumentKind::Source, &bytes)?.raw_text()
            } else {
                String::from_utf8(bytes)?
            };
            Ok(RepositoryMarkdown { revision, markdown })
        })
        .transpose()
}

pub(super) fn load_document_payload(
    db: &Connection,
    checkout: &str,
    revision: &str,
) -> Result<Option<(RepositoryMarkdownRevision, Vec<u8>)>> {
    let row = db.query_row(
        "SELECT id, checkout_id, path, sha256, base_commit, body FROM repository_markdown_revisions
         WHERE checkout_id = ?1 AND id = ?2",
        params![checkout, revision], |row| Ok((decode_revision(row)?, row.get::<_, Vec<u8>>(5)?)),
    ).optional()?;
    row.map(|(revision, body)| Ok((revision, protection::unprotect(&body)?)))
        .transpose()
}

pub(crate) fn workflow_markdown_revision(
    db: &Connection,
    project: &str,
    checkout: &str,
    revision: &str,
) -> Result<Option<RepositoryMarkdownRevision>> {
    if !DocumentKind::Markdown.contains_revision(revision) {
        return Ok(None);
    }
    Ok(db
        .query_row(
            "SELECT m.id, m.checkout_id, m.path, m.sha256, m.base_commit
             FROM repository_markdown_revisions m JOIN checkouts c ON c.id = m.checkout_id
             WHERE c.project_id = ?1 AND c.id = ?2 AND m.id = ?3",
            params![project, checkout, revision],
            decode_revision,
        )
        .optional()?)
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::windows::ffi::{OsStrExt, OsStringExt},
        process::Command,
    };

    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    use super::*;

    #[test]
    fn captures_short_dos_paths_without_following_junctions_or_leaving_checkout() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let directory = dunce::canonicalize(temporary.path())?;
        let root = directory.join("Repository with a very long Windows checkout name");
        fs::create_dir(&root)?;
        let initialized = Command::new(super::super::git_path()?)
            .args(["init", "--quiet"])
            .current_dir(&root)
            .output()?;
        ensure!(
            initialized.status.success(),
            "could not initialize the fixture checkout"
        );
        let selected = root.join("Release Overview.md");
        fs::write(&selected, "# Pinned release note\n")?;
        let short = short_path(&selected)?;
        // NTFS can disable allocation of new DOS names for the volume.
        assert_eq!(dunce::simplified(&expand_local_dos_path(&short)?), selected);
        drop(open_locked_local_file(&short)?);
        let (document, _) = capture(&root, &short, "checkout".into(), 4096)?;
        assert_eq!(document.revision.relative_path, "Release Overview.md");
        assert_eq!(document.markdown, "# Pinned release note\n");
        let mut with_nul = short.into_os_string();
        with_nul.push("\0other.md");
        assert!(capture(&root, Path::new(&with_nul), "checkout".into(), 4096).is_err());

        let outside = directory.join("Outside note directory");
        fs::create_dir(&outside)?;
        let outside_note = outside.join("Outside review.md");
        fs::write(&outside_note, "outside")?;
        let junction = root.join("Other checkout junction");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:OMNIAGENT_MARKDOWN_TEST_LINK -Target $env:OMNIAGENT_MARKDOWN_TEST_TARGET -ErrorAction Stop | Out-Null"])
            .env("OMNIAGENT_MARKDOWN_TEST_LINK", &junction)
            .env("OMNIAGENT_MARKDOWN_TEST_TARGET", &outside)
            .output()?;
        ensure!(
            output.status.success(),
            "could not create the fixture junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let linked = junction.join("Outside review.md");
        assert_eq!(dunce::simplified(&expand_local_dos_path(&linked)?), linked);
        assert!(open_locked_local_file(&linked).is_err());
        assert!(capture(&root, &linked, "checkout".into(), 4096).is_err());
        assert!(capture(&root, &short_path(&outside_note)?, "checkout".into(), 4096).is_err());
        fs::remove_dir(junction)?;
        Ok(())
    }

    fn short_path(path: &Path) -> Result<PathBuf> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut buffer = vec![0u16; 32_768];
        let length =
            unsafe { GetShortPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32) }
                as usize;
        ensure!(
            length > 0 && length < buffer.len(),
            "could not get the fixture DOS filename"
        );
        Ok(std::ffi::OsString::from_wide(&buffer[..length]).into())
    }
}
