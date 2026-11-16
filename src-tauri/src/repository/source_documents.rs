use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    markdown_recovery::{
        RepositoryMarkdownDraft, RepositoryMarkdownInvalidation, RepositoryMarkdownWatch,
    },
    providers::configuration::plain_metadata,
    store::Store,
};

use super::{
    RepositoryMarkdown, checkout_root,
    documents::{DocumentKind, RepositoryDocumentOpen, RepositoryDocumentSave, relative_name},
    git_path, git_query, verify_checkout,
};

const SOURCE: DocumentKind = DocumentKind::Source;

impl Store {
    pub async fn list_repository_source_files(&self, checkout: String) -> Result<Vec<String>> {
        let root = checkout_root(self, checkout.clone()).await?;
        let layout = verify_checkout(self, checkout, root).await?;
        tokio::task::spawn_blocking(move || list_files(&layout.root)).await?
    }

    pub async fn open_repository_source_file(
        &self,
        checkout: String,
        path: PathBuf,
    ) -> Result<RepositoryDocumentOpen> {
        self.open_checkout_document(checkout, path, SOURCE).await
    }

    pub async fn save_repository_source_file(
        &self,
        checkout: String,
        path: PathBuf,
        expected: String,
        text: String,
    ) -> Result<RepositoryDocumentSave> {
        self.save_checkout_document(checkout, path, expected, text, SOURCE)
            .await
    }

    pub async fn repository_source_revision(
        &self,
        checkout: String,
        revision: String,
    ) -> Result<Option<RepositoryMarkdown>> {
        self.checkout_document_revision(checkout, revision, SOURCE)
            .await
    }

    pub async fn repository_source_draft_paths(&self, checkout: String) -> Result<Vec<String>> {
        self.checkout_file_draft_paths(checkout, SOURCE).await
    }

    pub async fn repository_source_draft(
        &self,
        checkout: String,
        path: String,
    ) -> Result<Option<RepositoryMarkdownDraft>> {
        self.checkout_file_draft(checkout, path, SOURCE).await
    }

    pub async fn save_repository_source_draft(
        &self,
        checkout: String,
        base: String,
        expected: Option<String>,
        text: String,
    ) -> Result<RepositoryMarkdownDraft> {
        self.save_checkout_file_draft(checkout, base, expected, text, SOURCE)
            .await
    }

    pub async fn discard_repository_source_draft(
        &self,
        checkout: String,
        path: String,
        expected: String,
    ) -> Result<bool> {
        self.discard_checkout_file_draft(checkout, path, expected, SOURCE)
            .await
    }

    pub async fn watch_repository_source(
        &self,
        checkout: String,
        base: String,
        invalidate: impl Fn(RepositoryMarkdownInvalidation) + Send + 'static,
    ) -> Result<RepositoryMarkdownWatch> {
        self.watch_checkout_file(checkout, base, SOURCE, invalidate)
            .await
    }
}

fn list_files(root: &Path) -> Result<Vec<String>> {
    let names = git_query(
        &git_path()?,
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--deduplicate",
        ],
    )?;
    let mut paths: Vec<_> = names
        .split_terminator('\0')
        .filter_map(|name| {
            let path = root.join(name);
            let relative = relative_name(root, &path).ok()?;
            plain_metadata(&path)
                .ok()
                .flatten()
                .filter(|entry| entry.is_file())?;
            Some(relative)
        })
        .collect();
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}
