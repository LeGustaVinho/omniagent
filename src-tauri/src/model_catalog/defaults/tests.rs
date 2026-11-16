use std::{fs, path::Path, process::Command};

use super::*;
use crate::repository::{self, ProjectCheckout};

async fn register(store: &Store, root: &Path) -> Result<ProjectCheckout> {
    fs::create_dir(root)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()?
            .success()
    );
    repository::register(store, root.to_path_buf()).await
}

#[tokio::test]
async fn model_defaults_keep_project_purpose_and_exact_profile_separate_after_restart() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("defaults.sqlite");
    let store = Store::open(database.clone())?;
    let first = register(&store, &temp.path().join("first")).await?;
    let second = register(&store, &temp.path().join("second")).await?;
    let model = |provider, profile: &str| ModelSelection {
        provider,
        profile_id: profile.into(),
        model_id: "same-model-id".into(),
    };
    let writer = model(Provider::Codex, "desktop-direct:codex");
    let planner = model(Provider::Claude, "desktop-direct:claude");
    store
        .set_project_model_default(
            first.project_id.clone(),
            ModelDefaultPurpose::Planning,
            Some(planner.clone()),
        )
        .await?;
    store
        .set_project_model_default(
            first.project_id.clone(),
            ModelDefaultPurpose::Implementation,
            Some(writer.clone()),
        )
        .await?;
    store
        .set_execution_profile_default_model(
            writer.provider,
            writer.profile_id.clone(),
            Some(writer.clone()),
        )
        .await?;
    assert!(
        store
            .set_execution_profile_default_model(
                Provider::Claude,
                planner.profile_id.clone(),
                Some(writer.clone())
            )
            .await
            .is_err()
    );
    assert!(
        store
            .set_execution_profile_default_model(
                writer.provider,
                "another-profile".into(),
                Some(writer.clone())
            )
            .await
            .is_err()
    );
    assert!(
        store
            .set_project_model_default(
                "missing-project".into(),
                ModelDefaultPurpose::Conversation,
                Some(writer.clone())
            )
            .await
            .is_err()
    );

    drop(store);
    let store = Store::open(database)?;
    let expected = ProjectModelDefaults {
        conversation: None,
        planning: Some(planner),
        implementation: Some(writer.clone()),
    };
    assert_eq!(
        store.checkout_model_defaults(first.checkout_id).await?,
        expected
    );
    assert_eq!(
        store.project_model_defaults(second.project_id).await?,
        ProjectModelDefaults::default()
    );
    assert_eq!(
        store
            .execution_profile_default_model(writer.provider, writer.profile_id.clone())
            .await?,
        Some(writer.clone())
    );
    assert_eq!(
        store
            .execution_profile_default_model(writer.provider, "another-profile".into())
            .await?,
        None
    );
    let updated = store
        .set_project_model_default(first.project_id, ModelDefaultPurpose::Planning, None)
        .await?;
    assert_eq!(updated.planning, None);
    assert_eq!(updated.implementation, Some(writer));
    Ok(())
}
