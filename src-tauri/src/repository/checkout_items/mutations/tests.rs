use super::*;

fn edit(source: &str, directory: bool, destination: &str) -> CheckoutItemEdit {
    CheckoutItemEdit {
        source: source.into(),
        directory,
        destination: Some(destination.into()),
    }
}

#[test]
fn copies_folders_and_moves_files_without_replacing_existing_entries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    fs::create_dir_all(root.join("src/empty"))?;
    fs::write(root.join("src/a.bin"), [0, 128, 255])?;
    let copied = apply_items(
        &root,
        CheckoutItemOperation::Copy,
        vec![edit("src", true, "copy")],
        &[],
    )?;
    assert!(copied.error.is_none(), "{:?}", copied.error);
    assert_eq!(copied.completed.len(), 1);
    assert!(root.join("copy/empty").is_dir());
    assert_eq!(fs::read(root.join("copy/a.bin"))?, [0, 128, 255]);

    let moved = apply_items(
        &root,
        CheckoutItemOperation::Move,
        vec![edit("src/a.bin", false, "copy/renamed.bin")],
        &[],
    )?;
    assert!(moved.error.is_none(), "{:?}", moved.error);
    assert!(!root.join("src/a.bin").exists());
    assert_eq!(fs::read(root.join("copy/renamed.bin"))?, [0, 128, 255]);
    let casing = apply_items(
        &root,
        CheckoutItemOperation::Move,
        vec![edit("copy/renamed.bin", false, "copy/RENAMED.bin")],
        &[],
    )?;
    assert!(casing.error.is_none(), "{:?}", casing.error);
    assert!(
        fs::read_dir(root.join("copy"))?
            .any(|entry| entry.is_ok_and(|item| item.file_name() == "RENAMED.bin"))
    );

    assert!(
        apply_items(
            &root,
            CheckoutItemOperation::Copy,
            vec![edit("copy/RENAMED.bin", false, "copy/a.bin")],
            &[]
        )
        .is_err()
    );
    assert_eq!(fs::read(root.join("copy/a.bin"))?, [0, 128, 255]);
    Ok(())
}

#[test]
fn validates_the_whole_batch_before_moving_any_items() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    fs::create_dir(root.join("docs"))?;
    fs::write(root.join("docs/plan.md"), "document")?;
    fs::write(root.join("src.rs"), "source")?;
    assert!(
        apply_items(
            &root,
            CheckoutItemOperation::Move,
            vec![
                edit("src.rs", false, "renamed.rs"),
                edit("docs", true, "notes")
            ],
            &["docs/plan.md".into()]
        )
        .is_err()
    );
    assert!(root.join("src.rs").exists());
    assert!(!root.join("renamed.rs").exists());
    assert!(
        apply_items(
            &root,
            CheckoutItemOperation::Copy,
            vec![edit("docs", true, "docs/child")],
            &[]
        )
        .is_err()
    );
    assert!(
        apply_items(
            &root,
            CheckoutItemOperation::Move,
            vec![edit("docs", true, "../escape")],
            &[]
        )
        .is_err()
    );
    assert!(
        apply_items(
            &root,
            CheckoutItemOperation::Move,
            vec![
                edit("docs", true, "notes"),
                edit("docs/plan.md", false, "plan.md")
            ],
            &[]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn reports_completed_items_when_a_later_copy_cannot_be_published() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    fs::write(root.join("first"), "first")?;
    fs::create_dir_all(root.join("nested/.git"))?;
    let result = apply_items(
        &root,
        CheckoutItemOperation::Copy,
        vec![
            edit("first", false, "first-copy"),
            edit("nested", true, "nested-copy"),
        ],
        &[],
    )?;
    assert_eq!(result.completed.len(), 1);
    assert!(
        result
            .error
            .is_some_and(|error| error.contains("nested Git checkout"))
    );
    assert_eq!(fs::read_to_string(root.join("first-copy"))?, "first");
    assert!(!root.join("nested-copy").exists());
    assert_eq!(fs::read_dir(&root)?.count(), 3);
    Ok(())
}

#[test]
fn imports_external_files_into_the_selected_checkout_folder() -> Result<()> {
    let checkout = tempfile::tempdir()?;
    let root = dunce::canonicalize(checkout.path())?;
    fs::create_dir(root.join("media"))?;
    let external = tempfile::tempdir()?;
    let source = dunce::canonicalize(external.path())?.join("image.bin");
    fs::write(&source, [1, 4, 9])?;
    let plans = plan_imports(&root, "media", vec![source.clone()], false)?;
    assert_eq!(
        plans[0].edit.destination.as_deref(),
        Some("media/image.bin")
    );
    let result = apply_planned(CheckoutItemOperation::Copy, plans);
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(fs::read(root.join("media/image.bin"))?, [1, 4, 9]);
    assert_eq!(fs::read(&source)?, [1, 4, 9]);
    assert!(plan_imports(&root, "media", vec![source], false).is_err());
    Ok(())
}
