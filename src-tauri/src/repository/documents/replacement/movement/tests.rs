use super::*;
use crate::providers::configuration::open_locked_local_file;

#[test]
fn refuses_destination_created_after_the_selected_source_has_been_locked() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    let source = root.join("first.md");
    let destination = root.join("second.md");
    fs::write(&source, "reviewed")?;
    let identity = super::super::file_identity(&open_locked_local_file(&source)?)?;
    let result = move_exact_with(
        &root,
        &source,
        &destination,
        &hash(b"reviewed"),
        &identity,
        1024,
        || {
            fs::write(&destination, "written outside the application")?;
            Ok(())
        },
    )?;
    assert!(matches!(result, FileMove::Conflict(_)));
    assert_eq!(fs::read_to_string(source)?, "reviewed");
    assert_eq!(
        fs::read_to_string(destination)?,
        "written outside the application"
    );
    Ok(())
}

#[test]
fn a_new_file_with_identical_bytes_is_not_the_reviewed_source() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = dunce::canonicalize(directory.path())?;
    let source = root.join("first.md");
    let destination = root.join("second.md");
    fs::write(&source, "reviewed")?;
    let identity = super::super::file_identity(&open_locked_local_file(&source)?)?;
    fs::rename(&source, root.join("displaced.md"))?;
    fs::write(&source, "reviewed")?;
    assert!(matches!(
        move_exact(
            &root,
            &source,
            &destination,
            &hash(b"reviewed"),
            &identity,
            1024
        )?,
        FileMove::Conflict(_)
    ));
    assert!(source.is_file() && !destination.exists());
    Ok(())
}
