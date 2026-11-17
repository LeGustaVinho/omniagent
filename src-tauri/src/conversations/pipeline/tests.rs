use std::{fs, path::PathBuf, process::Command};

use crate::{
    repository::{ProjectCheckout, register},
    workflows::{DeliveryRoles, builtin_delivery_graph},
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
        "initialize conversation pipeline fixture"
    );
    register(store, path).await
}

pub(in crate::conversations) async fn save_workflow(
    store: &Store,
    project: String,
    name: &str,
    publish: bool,
) -> Result<WorkflowRevision> {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.name = name.into();
    let workflow = store
        .save_workflow_revision(project.clone(), "test-pipeline".into(), graph)
        .await?;
    if publish {
        let revision = workflow.clone();
        store
            .execute(move |db| {
                db.execute(
                    "INSERT INTO workflow_published_heads VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, workflow_id) DO UPDATE SET revision = excluded.revision",
                    params![project, revision.id, revision.revision],
                )?;
                Ok(())
            })
            .await?;
    }
    Ok(workflow)
}

#[tokio::test]
async fn existing_chat_upgrades_without_changing_the_shared_sqlite_migration_version() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("app.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = repo(&store, fixture.path().join("Repository")).await?;
    let legacy = store
        .create_conversation(checkout.checkout_id, "Legacy conversation".into())
        .await?;
    let legacy_version: u32 = store.execute(|db| {
        db.execute_batch("DROP TABLE conversation_pipeline_turns; ALTER TABLE conversations DROP COLUMN pipeline;")?;
        Ok(db.pragma_query_value(None, "user_version", |row| row.get(0))?)
    }).await?;
    drop(store);

    let store = Store::open(database.clone())?;
    let conversation_id = legacy.id.clone();
    store
        .execute(move |db| {
            let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
            assert_eq!(version, legacy_version);
            assert_eq!(
                load_conversation_pipeline(db, &conversation_id)?,
                ConversationPipeline::Direct
            );
            Ok(())
        })
        .await?;
    assert_eq!(store.conversation(legacy.id.clone()).await?, legacy);
    assert_eq!(
        Store::open(database)?
            .conversation(legacy.id.clone())
            .await?,
        legacy
    );
    Ok(())
}

#[tokio::test]
async fn published_pipeline_selection_is_project_scoped_and_forks_pin_the_original_revision()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("app.sqlite");
    let store = Store::open(database.clone())?;
    let first = repo(&store, fixture.path().join("First")).await?;
    let second = repo(&store, fixture.path().join("Second")).await?;
    let published =
        save_workflow(&store, first.project_id.clone(), "Published pipeline", true).await?;
    let private =
        save_workflow(&store, first.project_id.clone(), "Private pipeline", false).await?;
    let options = store
        .conversation_pipeline_options(first.checkout_id.clone())
        .await?;
    assert_eq!(
        options
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["Direct conversation", "Published pipeline"]
    );
    assert_eq!(
        store
            .conversation_pipeline_options(second.checkout_id.clone())
            .await?
            .len(),
        1
    );

    assert!(
        store
            .create_conversation_with_pipeline(
                first.checkout_id.clone(),
                "Private choice".into(),
                ConversationPipelineChoice::Published { workflow: private },
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_conversation_with_pipeline(
                second.checkout_id,
                "Cross-project choice".into(),
                ConversationPipelineChoice::Published {
                    workflow: published.clone()
                },
            )
            .await
            .is_err()
    );

    let chat = store
        .create_conversation_with_pipeline(
            first.checkout_id.clone(),
            "Pipeline conversation".into(),
            ConversationPipelineChoice::Published {
                workflow: published.clone(),
            },
        )
        .await?;
    assert!(chat.require_direct_turn().is_err());
    assert_eq!(
        chat.pipeline,
        Some(ConversationPipeline::Published {
            workflow: published,
            name: "Published pipeline".into(),
        })
    );
    save_workflow(&store, first.project_id, "New published revision", true).await?;
    drop(store);

    let reopened = Store::open(database)?;
    assert_eq!(reopened.conversation(chat.id.clone()).await?, chat);
    let fork = reopened
        .fork_conversation(
            chat.id.clone(),
            chat.context_id.clone(),
            None,
            "Fork".into(),
        )
        .await?;
    assert_eq!(fork.pipeline, chat.pipeline);
    let list = reopened
        .list_conversations(first.checkout_id.clone())
        .await?;
    assert!(list.contains(&chat) && list.contains(&fork));
    assert!(reopened
        .create_conversation_with_pipeline(
            first.checkout_id.clone(),
            "Delivery".into(),
            ConversationPipelineChoice::RepositoryDelivery,
        )
        .await.is_err());
    let legacy_id = chat.id.clone();
    reopened.execute(move |db| {
        db.execute(
            "UPDATE conversations SET pipeline = ?2 WHERE id = ?1",
            params![legacy_id, r#"{"kind":"repository_delivery","workflow":{"id":"old","revision":"old"}}"#],
        )?;
        Ok(())
    }).await?;
    reopened.conversation(chat.id.clone()).await?.require_direct_turn()?;
    let direct = reopened
        .create_conversation_with_pipeline(
            first.checkout_id,
            "Direct".into(),
            ConversationPipelineChoice::Direct,
        )
        .await?;
    assert_eq!(direct.pipeline, Some(ConversationPipeline::Direct));
    direct.require_direct_turn()?;
    Ok(())
}
