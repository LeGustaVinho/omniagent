use std::{fs, process::Command};

use anyhow::Result;
use notify::event::{AccessKind, Flag, RenameMode};
use tokio::sync::mpsc;

use crate::repository;

use super::*;

#[test]
fn classifies_note_and_structural_changes_without_opening_event_paths() {
    let root = Path::new(r"C:\registered\checkout");
    let note = root.join("docs/Note.MARKDOWN");
    let case_rename = root.join("docs/note.markdown");
    for kind in [
        EventKind::Create(CreateKind::Any),
        EventKind::Modify(ModifyKind::Any),
        EventKind::Remove(RemoveKind::Any),
    ] {
        assert_eq!(
            classify_event(root, Ok(Event::new(kind).add_path(note.clone()))),
            Some(Hint::Paths(vec!["docs/Note.MARKDOWN".into()]))
        );
    }
    let rename = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
        .add_path(note.clone())
        .add_path(case_rename);
    assert_eq!(
        classify_event(root, Ok(rename)),
        Some(Hint::Paths(vec![
            "docs/Note.MARKDOWN".into(),
            "docs/note.markdown".into()
        ]))
    );
    assert_eq!(
        classify_event(
            root,
            Ok(Event::new(EventKind::Access(AccessKind::Read)).add_path(note))
        ),
        None
    );

    let directory = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
        .add_path(root.join("docs/nested"));
    assert_eq!(
        classify_event(root, Ok(directory)),
        Some(Hint::Rebuild(IndexRebuildReason::StructuralChange))
    );
    let directory_like_note =
        Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("docs/folder.md"));
    assert_eq!(
        classify_event(root, Ok(directory_like_note)),
        Some(Hint::Rebuild(IndexRebuildReason::StructuralChange))
    );
    for path in ["docs/.gitignore", ".git", ".git/info/exclude"] {
        let discovery = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(root.join(path));
        assert_eq!(
            classify_event(root, Ok(discovery)),
            Some(Hint::Rebuild(IndexRebuildReason::DiscoveryChanged))
        );
    }
    for path in [".git/index", "docs/.omniagent-markdown-fixture.pending"] {
        let irrelevant = Event::new(EventKind::Create(CreateKind::Any)).add_path(root.join(path));
        assert_eq!(classify_event(root, Ok(irrelevant)), None);
    }
    let legitimate_hidden_note = Event::new(EventKind::Create(CreateKind::Any))
        .add_path(root.join("docs/.omniagent-markdown-guide.md"));
    assert_eq!(
        classify_event(root, Ok(legitimate_hidden_note)),
        Some(Hint::Paths(vec![
            "docs/.omniagent-markdown-guide.md".into()
        ]))
    );
}

#[test]
fn treats_gaps_errors_and_unusable_paths_as_full_rebuilds() {
    let root = Path::new(r"C:\registered\checkout");
    let lost = Event::new(EventKind::Any)
        .set_flag(Flag::Rescan)
        .add_path(root.join("Note.md"));
    assert_eq!(
        classify_event(root, Ok(lost)),
        Some(Hint::Rebuild(IndexRebuildReason::WatchGap))
    );
    assert_eq!(
        classify_event(root, Ok(Event::new(EventKind::Modify(ModifyKind::Any)))),
        Some(Hint::Rebuild(IndexRebuildReason::WatchGap))
    );
    assert_eq!(
        classify_event(root, Err(notify::Error::generic("fixture IO error"))),
        Some(Hint::Rebuild(IndexRebuildReason::WatchError))
    );
    for unexpected in [
        PathBuf::from(r"C:\registered\another\Note.md"),
        root.join("docs/../../Note.md"),
        root.join("Note.md:stream"),
    ] {
        let event = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(unexpected);
        assert_eq!(
            classify_event(root, Ok(event)),
            Some(Hint::Rebuild(IndexRebuildReason::UnexpectedPath))
        );
    }

    let mut batch = PendingChanges::default();
    for index in 0..=MAX_PENDING_PATHS {
        batch.record(Hint::Paths(vec![format!("notes/{index}.md")]));
    }
    assert_eq!(batch.rebuild, Some(IndexRebuildReason::TooManyChanges));
    assert!(batch.paths.is_empty());
}

#[tokio::test]
async fn watcher_debounces_a_batch_and_promotes_watch_gaps() -> Result<()> {
    let pending = Arc::new(Mutex::new(PendingChanges::default()));
    let (signal, changes) = watch::channel(());
    let (deliver, mut output) = mpsc::unbounded_channel();
    let worker = tokio::spawn(dispatch_changes(
        "checkout".into(),
        pending.clone(),
        changes,
        WATCH_HEALTH_INTERVAL,
        move |hint| {
            let _ = deliver.send(hint);
        },
    ));
    let record = |hint| {
        pending.lock().unwrap().record(hint);
        signal.send_replace(());
    };
    record(Hint::Paths(vec!["docs/B.md".into()]));
    record(Hint::Paths(vec!["docs/A.md".into(), "docs/B.md".into()]));
    assert_eq!(
        next_change(&mut output).await?,
        IndexInvalidation::Paths {
            checkout_id: "checkout".into(),
            paths: vec!["docs/A.md".into(), "docs/B.md".into()],
        }
    );
    record(Hint::Paths(vec!["docs/C.md".into()]));
    let gap = Event::new(EventKind::Any).set_flag(Flag::Rescan);
    record(classify_event(Path::new(r"C:\registered\checkout"), Ok(gap)).unwrap());
    record(Hint::Paths(vec!["docs/D.md".into()]));
    assert_eq!(
        next_change(&mut output).await?,
        IndexInvalidation::Rebuild {
            checkout_id: "checkout".into(),
            reason: IndexRebuildReason::WatchGap,
        }
    );
    drop(signal);
    worker.await?;
    Ok(())
}

#[tokio::test]
async fn periodic_reconciliation_covers_silence_without_waiting_for_native_events() -> Result<()> {
    let pending = Arc::new(Mutex::new(PendingChanges::default()));
    let (signal, changes) = watch::channel(());
    let (deliver, mut output) = mpsc::unbounded_channel();
    let worker = tokio::spawn(dispatch_changes(
        "checkout".into(),
        pending,
        changes,
        Duration::from_millis(20),
        move |hint| {
            let _ = deliver.send(hint);
        },
    ));
    assert_eq!(
        next_change(&mut output).await?,
        IndexInvalidation::Rebuild {
            checkout_id: "checkout".into(),
            reason: IndexRebuildReason::WatchHealthCheck,
        }
    );
    drop(signal);
    worker.await?;
    Ok(())
}

#[tokio::test]
async fn watches_creation_modification_rename_and_removal_in_the_registered_checkout() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("checkout");
    fs::create_dir_all(root.join("docs"))?;
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()?
            .success()
    );
    let store = Store::open(fixture.path().join("profile.sqlite"))?;
    let checkout = repository::register(&store, root.clone())
        .await?
        .checkout_id;
    let (deliver, mut changes) = mpsc::unbounded_channel();
    let _watcher = store
        .watch_repository_note_index(checkout.clone(), move |hint| {
            let _ = deliver.send(hint);
        })
        .await?;
    assert!(
        store
            .watch_repository_note_index("unknown-checkout".into(), |_| {})
            .await
            .is_err()
    );

    let paths = |paths: &[&str]| IndexInvalidation::Paths {
        checkout_id: checkout.clone(),
        paths: paths.iter().map(|path| (*path).into()).collect(),
    };
    let rebuild = |reason| IndexInvalidation::Rebuild {
        checkout_id: checkout.clone(),
        reason,
    };
    let before = "docs/Before.md";
    let after = "docs/After.markdown";
    fs::write(root.join(before), "Initial Markdown")?;
    assert_eq!(next_change(&mut changes).await?, paths(&[before]));
    fs::write(root.join(before), "Edited Markdown")?;
    assert_eq!(next_change(&mut changes).await?, paths(&[before]));
    fs::rename(root.join(before), root.join(after))?;
    assert_eq!(next_change(&mut changes).await?, paths(&[after, before]));
    fs::remove_file(root.join(after))?;
    assert_eq!(next_change(&mut changes).await?, paths(&[after]));

    let imported = fixture.path().join("imported");
    fs::create_dir(&imported)?;
    fs::write(imported.join("Included.md"), "Moved directory note")?;
    fs::rename(imported, root.join("docs/imported"))?;
    assert_eq!(
        next_change(&mut changes).await?,
        rebuild(IndexRebuildReason::StructuralChange)
    );
    fs::write(root.join(".gitignore"), "local-notes/\n")?;
    assert_eq!(
        next_change(&mut changes).await?,
        rebuild(IndexRebuildReason::DiscoveryChanged)
    );
    Ok(())
}

async fn next_change(
    output: &mut mpsc::UnboundedReceiver<IndexInvalidation>,
) -> Result<IndexInvalidation> {
    tokio::time::timeout(Duration::from_secs(10), output.recv())
        .await?
        .context("watcher closed without sending an invalidation")
}
