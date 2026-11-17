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

#[tokio::test]
async fn organizes_by_project_and_retains_archived_history_across_reopen() -> Result<()> {
    use ConversationLibraryChange::{Archive, Move, NewSection, Pin, Read};

    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("app.sqlite");
    let store = Store::open(database.clone())?;
    let first = repo(&store, fixture.path().join("First")).await?;
    let second = repo(&store, fixture.path().join("Second")).await?;
    let original = store
        .create_conversation(first.checkout_id.clone(), "Launch notes".into())
        .await?;
    let destination = store
        .create_conversation(first.checkout_id.clone(), "Release review".into())
        .await?;
    let other = store
        .create_conversation(second.checkout_id.clone(), "Unrelated repository".into())
        .await?;

    assert!(
        !store
            .organize_conversation(
                first.checkout_id.clone(),
                original.id.clone(),
                Read { unread: false }
            )
            .await?
    );
    for change in [
        NewSection {
            name: "  Entregáveis  ".into(),
        },
        Pin { pinned: true },
        Read { unread: true },
    ] {
        assert!(
            store
                .organize_conversation(first.checkout_id.clone(), original.id.clone(), change)
                .await?
        );
    }
    let first_sections = store
        .conversation_library_sections(Some(first.project_id.clone()))
        .await?;
    assert_eq!(first_sections.len(), 1);
    let section = &first_sections[0];
    assert_eq!(section.name, "Entregáveis");
    assert_eq!(section.project_id, first.project_id);
    assert!(
        store
            .organize_conversation(
                first.checkout_id.clone(),
                destination.id.clone(),
                NewSection {
                    name: "ENTREGA\u{301}VEIS".into()
                }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .organize_conversation(
                second.checkout_id.clone(),
                other.id.clone(),
                Move {
                    section_id: Some(section.id.clone())
                }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .organize_conversation(
                second.checkout_id.clone(),
                original.id.clone(),
                Archive { archived: true }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .organize_conversation(
                second.checkout_id.clone(),
                other.id.clone(),
                NewSection {
                    name: section.name.clone()
                }
            )
            .await?
    );
    store
        .save_conversation_draft(original.id.clone(), "An unsent note".into())
        .await?;
    assert!(
        store
            .organize_conversation(
                first.checkout_id.clone(),
                original.id.clone(),
                Archive { archived: true }
            )
            .await?
    );
    drop(store);

    let reopened = Store::open(database)?;
    assert_eq!(
        reopened
            .conversation_library(None, Some(first.project_id.clone()))
            .await?
            .len(),
        1
    );
    let archives = reopened
        .conversation_library_view(None, Some(first.project_id.clone()), true)
        .await?;
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].conversation.id, original.id);
    let organization = &archives[0].organization;
    assert!(organization.pinned && organization.unread && organization.archived);
    assert_eq!(organization.section.as_ref().unwrap().id, section.id);
    assert_eq!(
        reopened.conversation_draft(original.id.clone()).await?,
        "An unsent note"
    );
    let source = reopened
        .conversation_reference_source(
            destination.id,
            original.id.clone(),
            Some(original.context_id.clone()),
        )
        .await?;
    assert_eq!(source.context_id, original.context_id);
    assert!(
        reopened
            .conversation_reference_source(other.id, original.id.clone(), None)
            .await
            .is_err()
    );

    for change in [
        Archive { archived: false },
        Pin { pinned: false },
        Read { unread: false },
        Move { section_id: None },
    ] {
        assert!(
            reopened
                .organize_conversation(first.checkout_id.clone(), original.id.clone(), change)
                .await?
        );
    }
    let restored = reopened
        .conversation_library(Some("Launch".into()), None)
        .await?;
    let organization = &restored[0].organization;
    assert!(
        !organization.pinned
            && !organization.unread
            && !organization.archived
            && organization.section.is_none()
    );
    assert_eq!(reopened.conversation_library_sections(None).await?.len(), 2);
    Ok(())
}
