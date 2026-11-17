use super::*;

#[test]
fn installation_survives_reopen_and_only_its_seal_authenticates_the_manifest() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("installed");
    let first = InstallationIdentity::load_or_create(&root)?;
    let seal = first.seal_manifest(b"approved manifest")?;
    let file = root.join(IDENTITY_FILE);
    let original = fs::read(&file)?;
    let upgraded = InstallationIdentity::load_or_create(&root)?;
    assert_eq!(upgraded.id(), first.id());
    assert_eq!(fs::read(&file)?, original);
    upgraded.verify_manifest(b"approved manifest", &seal)?;
    assert!(
        upgraded
            .verify_manifest(b"modified manifest", &seal)
            .is_err()
    );
    let replacement = InstallationIdentity::load_or_create(&directory.path().join("second"))?;
    assert_ne!(replacement.id(), first.id());
    assert!(
        replacement
            .verify_manifest(b"approved manifest", &seal)
            .is_err()
    );
    Ok(())
}

#[test]
fn a_foreign_profile_identity_and_unreadable_dpapi_are_rejected_without_creating_a_replacement()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = InstallationIdentity::load_or_create(directory.path())?;
    let mut foreign = identity.record.clone();
    foreign.user_sid = if foreign.user_sid == "S-1-5-18" {
        "S-1-5-19"
    } else {
        "S-1-5-18"
    }
    .into();
    let file = identity.app_data().join(IDENTITY_FILE);
    let sealed = protection::protect(&serde_json::to_vec(&foreign)?)?;
    atomic_file::replace(&file, &sealed)?;
    let failure = InstallationIdentity::load_or_create(directory.path()).unwrap_err();
    assert!(failure.to_string().contains("another Windows profile"));
    assert_eq!(fs::read(&file)?, sealed);
    let invalid = b"not a DPAPI blob";
    atomic_file::replace(&file, invalid)?;
    let failure = InstallationIdentity::load_or_create(directory.path()).unwrap_err();
    assert!(
        failure
            .to_string()
            .contains("cannot be opened by this Windows profile")
    );
    assert_eq!(fs::read(file)?, invalid);
    Ok(())
}
