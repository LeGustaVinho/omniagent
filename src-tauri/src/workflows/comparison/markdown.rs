use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    conversations::SessionScope,
    model_catalog::ModelOption,
    repository::{RepositoryMarkdownRevision, load_markdown, workflow_markdown_revision},
    store::Store,
};

pub struct ComparisonMarkdownSource {
    pub revision_id: String,
    pub relative_path: String,
    pub sha256: String,
}

impl Store {
    pub async fn start_model_comparison_from_markdown(
        &self,
        project: String,
        checkout: String,
        source: ComparisonMarkdownSource,
        prompt: String,
        profiles: Vec<(SessionScope, Option<ModelOption>)>,
        storage: PathBuf,
    ) -> Result<super::ModelComparison> {
        let revision = self
            .execute({
                let (project, checkout) = (project.clone(), checkout.clone());
                move |db| {
                    let revision =
                        workflow_markdown_revision(db, &project, &checkout, &source.revision_id)?
                            .context(
                            "The saved Markdown revision is unavailable in this project checkout",
                        )?;
                    ensure!(
                        revision.relative_path == source.relative_path
                            && revision.sha256 == source.sha256,
                        "The selected path or checksum does not match the saved Markdown revision"
                    );
                    Ok(revision)
                }
            })
            .await?;
        let commit = revision
            .base_commit
            .clone()
            .context("The saved Markdown revision needs a Git base commit")?;
        let mut prepared = self
            .prepare_model_comparison(project, checkout, commit, prompt, profiles, storage)
            .await?;
        prepared.source_document = Some(revision);
        self.enqueue_prepared_comparison(prepared).await
    }
}

pub(super) fn record_source(
    db: &Connection,
    group: &str,
    checkout: &str,
    commit: &str,
    source: &RepositoryMarkdownRevision,
) -> Result<()> {
    ensure!(
        source.checkout_id == checkout && source.base_commit.as_deref() == Some(commit),
        "The comparison and saved Markdown revision have different source scopes"
    );
    let saved = load_markdown(db, checkout, &source.id)?
        .filter(|document| &document.revision == source)
        .context("The saved Markdown revision changed or is no longer available")?;
    saved.verify_checksum()?;
    db.execute(
        "INSERT INTO model_comparison_markdown_sources (group_id, checkout_id, revision_id, path, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![group, checkout, source.id, source.relative_path, source.sha256],
    )?;
    Ok(())
}

pub(super) fn load_source(
    db: &Connection,
    group: &str,
) -> Result<Option<RepositoryMarkdownRevision>> {
    let source = db.query_row(
        "SELECT source.revision_id, source.checkout_id, source.path, source.sha256, g.commit_oid, g.source_checkout_id
         FROM model_comparison_markdown_sources source
         JOIN model_comparison_groups g ON g.id = source.group_id
         WHERE source.group_id = ?1",
        [group],
        |row| Ok((RepositoryMarkdownRevision {
            id: row.get(0)?, checkout_id: row.get(1)?, relative_path: row.get(2)?, sha256: row.get(3)?, base_commit: Some(row.get(4)?),
        }, row.get::<_, String>(5)?)),
    ).optional()?;
    source
        .map(|(document, checkout)| {
            ensure!(
                document.checkout_id == checkout,
                "The saved Markdown revision belongs to another comparison checkout"
            );
            Ok(document)
        })
        .transpose()
}
