use std::{fs, path::PathBuf, process::Command};

use anyhow::{Result, ensure};

use crate::{
    conversations::SessionScope,
    providers::{Provider, SessionMode},
    repository::register,
};

use super::*;

async fn repo(store: &Store, path: PathBuf) -> Result<ProjectCheckout> {
    fs::create_dir(&path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(&path)
            .status()?
            .success(),
        "initialize conversation library fixture"
    );
    register(store, path).await
}

#[tokio::test]
async fn searches_projects_in_activity_order_and_reopens_renamed_conversations_with_run_metadata()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("app.sqlite");
    let store = Store::open(database.clone())?;
    let first = repo(&store, fixture.path().join("First")).await?;
    let second = repo(&store, fixture.path().join("Second")).await?;
    let older = store
        .create_conversation(first.checkout_id.clone(), "Café launch".into())
        .await?;
    let latest = store
        .create_conversation(second.checkout_id.clone(), "Release checklist".into())
        .await?;
    let previous = store.conversation_library(None, None).await?;
    assert_eq!(
        previous
            .iter()
            .map(|item| &item.conversation.id)
            .collect::<Vec<_>>(),
        [&latest.id, &older.id]
    );
    assert!(previous.iter().all(|item| item.last_activity_at.is_some()));
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "library-fixture".into(),
        account_binding: None,
        checkout: PathBuf::from(&first.checkout_root),
        executable: PathBuf::from(&first.checkout_root).join("codex.exe"),
        cli_version: "1.0.0".into(),
        runner: "fixture".into(),
        permission_revision: "library".into(),
        mode: SessionMode::ReadOnly,
        model: Some("selected-model".into()),
        effort: None,
    };
    let attempt = store
        .prepare_agent_attempt(
            older.id.clone(),
            older.context_id,
            scope,
            "Retain a transcript".into(),
        )
        .await?;
    store.mark_agent_running(attempt.run.id.clone()).await?;
    store
        .execute(move |db| {
            db.execute(
                "UPDATE agent_attempts SET reported_model = 'observed-model' WHERE run_id = ?1",
                [attempt.run.id],
            )?;
            Ok(())
        })
        .await?;
    let found = store
        .conversation_library(Some("CAFE".into()), None)
        .await?;
    assert_eq!(found.len(), 1);
    assert_eq!(
        (&found[0].checkout, found[0].project_name.as_str()),
        (&first, "First")
    );
    assert_eq!(found[0].agent.as_deref(), Some("codex"));
    assert_eq!(found[0].model.as_deref(), Some("observed-model"));
    assert_eq!(found[0].status.as_deref(), Some("running"));
    assert!(
        store
            .conversation_library(Some("Cafe".into()), Some(second.project_id.clone()))
            .await?
            .is_empty()
    );
    let old = store
        .rename_conversation(older.id, "Revisão de produção".into())
        .await?;
    store
        .save_conversation_draft(old.id.clone(), "Unsent review".into())
        .await?;
    let hidden = store
        .create_conversation(second.checkout_id.clone(), "Hidden worker".into())
        .await?;
    store
        .execute(move |db| {
            db.execute(
                "UPDATE conversations SET background = 1 WHERE id = ?1",
                [hidden.id],
            )?;
            Ok(())
        })
        .await?;
    drop(store);

    let reopened = Store::open(database)?;
    let all = reopened.conversation_library(None, None).await?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].conversation.id, old.id);
    assert_eq!(all[0].conversation.title, "Revisão de produção");
    assert_eq!(
        reopened.conversation_draft(old.id.clone()).await?,
        "Unsent review"
    );
    assert_eq!(
        reopened.conversation_history(old.id).await?.messages[0].text,
        "Retain a transcript"
    );
    Ok(())
}
