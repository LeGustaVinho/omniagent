use std::{collections::BTreeMap, fs, path::Path, process::Command};

use anyhow::{Result, ensure};
use omniagent::{
    repository::{self, markdown_index::IndexedNote, markdown_links::MarkdownLinkResolution},
    store::Store,
};
use sha2::{Digest, Sha256};

const REFERENCE_PATH: &str = "notes/reference.md";
const DETAILS_PATH: &str = "notes/details.md";
const ALTERNATE_PATH: &str = "archive/Café.md";
const REFERENCE: &str = include_str!("../../tests/fixtures/obsidian-reference/reference.md");
const DETAILS: &str = include_str!("../../tests/fixtures/obsidian-reference/details.md");
const ALTERNATE: &str = concat!(
    "\u{feff}---\r\naliases: [Fallback evidence]\r\ntags: [contingência]\r\n---\r\n",
    "# Guia de contingência\r\n\r\n",
    "Ficha alternativa de verificação. ^validacao\r\n\r\n",
    "[[/notes/REFERENCE.md#Revisão paralela|Voltar ao índice]].\r\n",
);

#[tokio::test]
async fn rebuilds_the_reference_graph_and_search_independently_in_linked_worktrees() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let primary = fixture.path().join("primary");
    let linked = fixture.path().join("linked");
    fs::create_dir_all(primary.join("notes"))?;
    git(&primary, &["init", "--quiet"])?;
    fs::write(primary.join(".gitattributes"), "*.md -text\n")?;
    fs::write(primary.join(REFERENCE_PATH), REFERENCE)?;
    fs::write(primary.join(DETAILS_PATH), DETAILS)?;
    git(&primary, &["add", "."])?;
    git(
        &primary,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--no-gpg-sign",
            "--quiet",
            "-m",
            "Add the Phase 05 comparison notes",
        ],
    )?;
    git(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "reference-alternate",
            &linked.to_string_lossy(),
        ],
    )?;

    fs::create_dir_all(linked.join("archive"))?;
    fs::write(linked.join(ALTERNATE_PATH), ALTERNATE)?;
    fs::write(
        linked.join(REFERENCE_PATH),
        REFERENCE
            .replacen("Compatibility check 🚀", "Revisão paralela", 1)
            .replace("./details#Evidence", "Fallback evidence#^validacao")
            .replacen(
                "Measured on this fixture.",
                "Termômetro de contingência atualizado.",
                1,
            ),
    )?;

    let primary_files = [REFERENCE_PATH, DETAILS_PATH];
    let linked_files = [REFERENCE_PATH, DETAILS_PATH, ALTERNATE_PATH];
    let primary_hashes = source_hashes(&primary, &primary_files)?;
    let linked_hashes = source_hashes(&linked, &linked_files)?;
    let primary_status = git(
        &primary,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    let linked_status = git(&linked, &["status", "--porcelain", "--untracked-files=all"])?;
    assert_ne!(
        primary_hashes[REFERENCE_PATH],
        linked_hashes[REFERENCE_PATH]
    );

    let store = Store::open(fixture.path().join("profile.sqlite"))?;
    let main = repository::register(&store, primary.clone()).await?;
    let alternate = repository::register(&store, linked.clone()).await?;
    assert_eq!(main.project_id, alternate.project_id);
    assert_ne!(main.checkout_id, alternate.checkout_id);

    let primary_ids = note_ids(
        store
            .rebuild_repository_note_index(main.checkout_id.clone())
            .await?,
    );
    assert_eq!(
        primary_ids.keys().map(String::as_str).collect::<Vec<_>>(),
        [DETAILS_PATH, REFERENCE_PATH]
    );
    assert_eq!(
        graph_edges(&store, &main.checkout_id).await?,
        [(REFERENCE_PATH.to_owned(), DETAILS_PATH.to_owned())]
    );
    let relationships = store
        .repository_note_relationships(main.checkout_id.clone(), REFERENCE_PATH.into())
        .await?;
    assert_eq!(
        relationships.outgoing.len(),
        2,
        "the reference contains a link and an embed"
    );
    assert!(relationships.outgoing.iter().all(|entry| matches!(&entry.resolution,
        MarkdownLinkResolution::Resolved { location }
            if location.path == DETAILS_PATH && location.anchor.as_deref() == Some("Evidence")
    )));

    let alternate_ids = note_ids(
        store
            .rebuild_repository_note_index(alternate.checkout_id.clone())
            .await?,
    );
    assert_eq!(
        alternate_ids.keys().map(String::as_str).collect::<Vec<_>>(),
        [ALTERNATE_PATH, DETAILS_PATH, REFERENCE_PATH]
    );
    for path in primary_files {
        assert_ne!(
            primary_ids[path], alternate_ids[path],
            "the worktrees share a file ID for {path}"
        );
    }
    assert_eq!(
        graph_edges(&store, &alternate.checkout_id).await?,
        [
            (ALTERNATE_PATH.to_owned(), REFERENCE_PATH.to_owned()),
            (REFERENCE_PATH.to_owned(), ALTERNATE_PATH.to_owned()),
        ]
    );
    let relationships = store
        .repository_note_relationships(alternate.checkout_id.clone(), REFERENCE_PATH.into())
        .await?;
    assert_eq!(relationships.backlinks.len(), 1);
    assert_eq!(relationships.backlinks[0].path, ALTERNATE_PATH);
    assert_eq!(relationships.outgoing.len(), 2);
    assert!(relationships.outgoing.iter().all(|entry| matches!(&entry.resolution,
        MarkdownLinkResolution::Resolved { location }
            if location.path == ALTERNATE_PATH && location.anchor.as_deref() == Some("^validacao")
    )));

    let primary_checkout = &main.checkout_id;
    let alternate_checkout = &alternate.checkout_id;
    let expected_hits: [(&String, &str, &[&str]); 8] = [
        (primary_checkout, "compatibility check", &[REFERENCE_PATH]),
        (
            primary_checkout,
            "measured on this fixture",
            &[REFERENCE_PATH],
        ),
        (primary_checkout, "revisao paralela", &[]),
        (primary_checkout, "termometro de contingencia", &[]),
        // The linked note mentions this heading in its backlink. Both are valid text hits.
        (
            alternate_checkout,
            "revisao paralela",
            &[ALTERNATE_PATH, REFERENCE_PATH],
        ),
        (
            alternate_checkout,
            "termometro de contingencia",
            &[REFERENCE_PATH],
        ),
        (alternate_checkout, "compatibility check", &[]),
        (alternate_checkout, "measured on this fixture", &[]),
    ];
    for (checkout, query, expected) in expected_hits {
        let results = store
            .search_repository_note_index(checkout.clone(), query.into(), 10)
            .await?;
        let mut paths = results
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        assert_eq!(
            paths.as_slice(),
            expected,
            "search {query:?} in checkout {checkout}"
        );
    }

    assert_eq!(
        note_ids(
            store
                .rebuild_repository_note_index(main.checkout_id.clone())
                .await?
        ),
        primary_ids
    );
    assert_eq!(
        graph_edges(&store, &main.checkout_id).await?,
        [(REFERENCE_PATH.to_owned(), DETAILS_PATH.to_owned())]
    );
    assert_eq!(
        note_ids(store.repository_note_index(alternate.checkout_id).await?),
        alternate_ids
    );
    assert_eq!(source_hashes(&primary, &primary_files)?, primary_hashes);
    assert_eq!(source_hashes(&linked, &linked_files)?, linked_hashes);
    assert_eq!(
        git(
            &primary,
            &["status", "--porcelain", "--untracked-files=all"]
        )?,
        primary_status
    );
    assert_eq!(
        git(&linked, &["status", "--porcelain", "--untracked-files=all"])?,
        linked_status
    );
    Ok(())
}

fn note_ids(notes: Vec<IndexedNote>) -> BTreeMap<String, i64> {
    notes.into_iter().map(|note| (note.path, note.id)).collect()
}

async fn graph_edges(store: &Store, checkout: &str) -> Result<Vec<(String, String)>> {
    Ok(store
        .repository_note_edges(checkout.into())
        .await?
        .into_iter()
        .map(|edge| (edge.source_path, edge.target_path))
        .collect())
}

fn source_hashes<'a>(root: &Path, paths: &[&'a str]) -> Result<BTreeMap<&'a str, String>> {
    paths
        .iter()
        .map(|&path| {
            let bytes = fs::read(root.join(path))?;
            Ok((path, format!("{:x}", Sha256::digest(bytes))))
        })
        .collect()
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git.exe")
        .current_dir(root)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}
