use std::{fs, path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result};
use omniagent::{
    repository::{
        self,
        markdown_index::IndexedNoteRelationships,
        markdown_index_watch::{IndexInvalidation, IndexRebuildReason},
        markdown_links::{MarkdownLinkLocation, MarkdownLinkResolution},
    },
    store::Store,
};
use tokio::sync::mpsc;

const DESK: &str = "docs/Desk.md";
const RELEASE: &str = "guide/Release.md";
const PUBLISHED: &str = "review/Published.markdown";
const CONTENDER: &str = "archive/Contender.md";

struct Checkout {
    store: Store,
    id: String,
    root: PathBuf,
    _directory: tempfile::TempDir,
}

impl Checkout {
    async fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("checkout");
        fs::create_dir(&root)?;
        for folder in ["docs", "guide", "review", "archive"] {
            fs::create_dir(root.join(folder))?;
        }
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let store = Store::open(directory.path().join("index.sqlite"))?;
        let id = repository::register(&store, root.clone())
            .await?
            .checkout_id;
        Ok(Self {
            store,
            id,
            root,
            _directory: directory,
        })
    }

    fn write(&self, path: &str, source: &str) -> Result<()> {
        fs::write(self.root.join(path), source)?;
        Ok(())
    }

    async fn apply(&self, invalidation: IndexInvalidation) -> Result<()> {
        let (checkout_id, paths) = match invalidation {
            IndexInvalidation::Paths { checkout_id, paths } => (checkout_id, Some(paths)),
            IndexInvalidation::Rebuild { checkout_id, .. } => (checkout_id, None),
        };
        assert_eq!(checkout_id, self.id);
        self.store
            .reconcile_repository_note_index(checkout_id, paths)
            .await?;
        Ok(())
    }

    async fn apply_native(
        &self,
        notifications: &mut mpsc::UnboundedReceiver<IndexInvalidation>,
        expected_paths: &[&str],
    ) -> Result<()> {
        let notice = tokio::time::timeout(Duration::from_secs(10), notifications.recv())
            .await?
            .context("native watch closed before sending the note changes")?;
        let IndexInvalidation::Paths { paths, .. } = &notice else {
            anyhow::bail!(
                "the specific note edit did not produce a path reconciliation: {notice:?}"
            );
        };
        assert_eq!(paths, expected_paths);
        self.apply(notice).await
    }

    async fn relationships(&self, path: &str) -> Result<IndexedNoteRelationships> {
        self.store
            .repository_note_relationships(self.id.clone(), path.into())
            .await
    }

    async fn assert_edges(&self, expected: &[(&str, &str)]) -> Result<()> {
        let edges = self.store.repository_note_edges(self.id.clone()).await?;
        let actual = edges
            .iter()
            .map(|edge| (edge.source_path.as_str(), edge.target_path.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        Ok(())
    }

    async fn assert_search(&self, token: &str, expected: &[&str]) -> Result<()> {
        let hits = self
            .store
            .search_repository_note_index(self.id.clone(), token.into(), 10)
            .await?;
        assert_eq!(
            hits.iter().map(|hit| hit.path.as_str()).collect::<Vec<_>>(),
            expected,
            "full-text term {token}"
        );
        Ok(())
    }
}

fn location(path: &str, heading: bool) -> MarkdownLinkLocation {
    MarkdownLinkLocation {
        path: path.into(),
        anchor: heading.then(|| "Ready".into()),
        line: heading.then_some(5),
    }
}

#[tokio::test]
async fn native_note_changes_reconcile_search_edges_and_backlinks() -> Result<()> {
    use MarkdownLinkResolution::{Ambiguous, Missing, Resolved};

    let checkout = Checkout::new().await?;
    checkout.write(
        DESK,
        "# Run desk\n\n[[Gate checklist#Ready|Review gate]]\n\n[[/guide/release|Original path]]\n",
    )?;
    let (publish, mut changes) = mpsc::unbounded_channel();
    let _watch = checkout
        .store
        .watch_repository_note_index(checkout.id.clone(), move |notice| {
            let _ = publish.send(notice);
        })
        .await?;
    checkout
        .store
        .reconcile_repository_note_index(checkout.id.clone(), None)
        .await?;
    let relationships = checkout.relationships(DESK).await?;
    assert_eq!(relationships.outgoing.len(), 2);
    assert!(
        relationships
            .outgoing
            .iter()
            .all(|outgoing| outgoing.resolution == Missing)
    );
    checkout.assert_edges(&[]).await?;

    let release_source = "---\naliases: [Gate checklist]\n---\n\n## Ready\n\nreleaseoriginsignal\n";
    checkout.write(RELEASE, release_source)?;
    checkout.apply_native(&mut changes, &[RELEASE]).await?;
    checkout
        .assert_search("releaseoriginsignal", &[RELEASE])
        .await?;
    checkout.assert_edges(&[(DESK, RELEASE)]).await?;
    let relationships = checkout.relationships(DESK).await?;
    assert_eq!(
        relationships.outgoing[0].resolution,
        Resolved {
            location: location(RELEASE, true)
        }
    );
    assert_eq!(
        relationships.outgoing[1].resolution,
        Resolved {
            location: location(RELEASE, false)
        }
    );
    let release = checkout.relationships(RELEASE).await?;
    assert_eq!(
        release
            .backlinks
            .iter()
            .map(|link| (link.path.as_str(), link.line, link.label.as_deref()))
            .collect::<Vec<_>>(),
        [
            (DESK, 3, Some("Review gate")),
            (DESK, 5, Some("Original path"))
        ]
    );

    checkout.write(
        CONTENDER,
        "---\naliases: [Gate checklist]\n---\n\n## Ready\n\nduplicatewatchsignal\n",
    )?;
    checkout.apply_native(&mut changes, &[CONTENDER]).await?;
    checkout
        .assert_search("duplicatewatchsignal", &[CONTENDER])
        .await?;
    assert_eq!(
        checkout.relationships(DESK).await?.outgoing[0].resolution,
        Ambiguous {
            candidates: vec![location(CONTENDER, true), location(RELEASE, true)]
        }
    );
    checkout.assert_edges(&[(DESK, RELEASE)]).await?;
    assert!(
        checkout
            .relationships(CONTENDER)
            .await?
            .backlinks
            .is_empty()
    );
    let release = checkout.relationships(RELEASE).await?;
    assert_eq!(release.backlinks.len(), 1);
    assert_eq!(release.backlinks[0].label.as_deref(), Some("Original path"));

    checkout.write(
        CONTENDER,
        "---\naliases: [Another guide]\n---\n\n## Ready\n\neditedwatchsignal\n",
    )?;
    checkout.apply_native(&mut changes, &[CONTENDER]).await?;
    checkout.assert_search("duplicatewatchsignal", &[]).await?;
    checkout
        .assert_search("editedwatchsignal", &[CONTENDER])
        .await?;
    assert_eq!(
        checkout.relationships(DESK).await?.outgoing[0].resolution,
        Resolved {
            location: location(RELEASE, true)
        }
    );
    assert_eq!(checkout.relationships(RELEASE).await?.backlinks.len(), 2);

    fs::rename(checkout.root.join(RELEASE), checkout.root.join(PUBLISHED))?;
    checkout
        .apply_native(&mut changes, &[RELEASE, PUBLISHED])
        .await?;
    checkout
        .assert_search("releaseoriginsignal", &[PUBLISHED])
        .await?;
    checkout.assert_edges(&[(DESK, PUBLISHED)]).await?;
    let relationships = checkout.relationships(DESK).await?;
    assert_eq!(
        relationships.outgoing[0].resolution,
        Resolved {
            location: location(PUBLISHED, true)
        }
    );
    assert_eq!(relationships.outgoing[1].resolution, Missing);
    assert!(checkout.relationships(RELEASE).await?.backlinks.is_empty());
    let published = checkout.relationships(PUBLISHED).await?;
    assert_eq!(published.backlinks.len(), 1);
    assert_eq!(published.backlinks[0].label.as_deref(), Some("Review gate"));
    assert_eq!(
        fs::read_to_string(checkout.root.join(PUBLISHED))?,
        release_source
    );

    fs::remove_file(checkout.root.join(PUBLISHED))?;
    checkout.apply_native(&mut changes, &[PUBLISHED]).await?;
    checkout.assert_search("releaseoriginsignal", &[]).await?;
    checkout.assert_edges(&[]).await?;
    assert!(
        checkout
            .relationships(DESK)
            .await?
            .outgoing
            .iter()
            .all(|outgoing| outgoing.resolution == Missing)
    );
    assert!(
        checkout
            .relationships(PUBLISHED)
            .await?
            .backlinks
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_full_rescan_after_a_watch_gap_reconciles_disjoint_existing_notes() -> Result<()> {
    use MarkdownLinkResolution::{Missing, Resolved};

    let checkout = Checkout::new().await?;
    checkout.write(DESK, "# Run desk\n\n[[Gap target#Ready]]\n")?;
    checkout.write(CONTENDER, "# Archive\n\noldgaptext\n")?;
    checkout
        .store
        .reconcile_repository_note_index(checkout.id.clone(), None)
        .await?;
    assert_eq!(
        checkout.relationships(DESK).await?.outgoing[0].resolution,
        Missing
    );

    // Both existing notes change on disk, but one path is lost during the simulated gap.
    checkout.write(
        DESK,
        "# Run desk\n\n[[Gap target#Ready|From the desk]]\n\nrenovatedsource\n",
    )?;
    let off_path = "---\naliases: [Gap target]\n---\n\n## Ready\n\nrecoveredgaptext\n";
    checkout.write(CONTENDER, off_path)?;
    checkout
        .apply(IndexInvalidation::Paths {
            checkout_id: checkout.id.clone(),
            paths: vec![DESK.into()],
        })
        .await?;
    checkout.assert_search("renovatedsource", &[DESK]).await?;
    checkout.assert_search("recoveredgaptext", &[]).await?;
    checkout.assert_search("oldgaptext", &[CONTENDER]).await?;
    assert_eq!(
        checkout.relationships(DESK).await?.outgoing[0].resolution,
        Missing
    );

    checkout
        .apply(IndexInvalidation::Rebuild {
            checkout_id: checkout.id.clone(),
            reason: IndexRebuildReason::WatchGap,
        })
        .await?;
    checkout.assert_search("oldgaptext", &[]).await?;
    checkout
        .assert_search("recoveredgaptext", &[CONTENDER])
        .await?;
    checkout.assert_edges(&[(DESK, CONTENDER)]).await?;
    assert_eq!(
        checkout.relationships(DESK).await?.outgoing[0].resolution,
        Resolved {
            location: location(CONTENDER, true)
        }
    );
    let destination = checkout.relationships(CONTENDER).await?;
    assert_eq!(destination.backlinks.len(), 1);
    assert_eq!(destination.backlinks[0].path, DESK);
    assert_eq!(
        destination.backlinks[0].label.as_deref(),
        Some("From the desk")
    );
    assert_eq!(fs::read_to_string(checkout.root.join(CONTENDER))?, off_path);
    Ok(())
}
