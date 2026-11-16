use super::*;
use crate::{
    conversations::tests::{confirm, prepare, setup},
    providers::SessionIntent,
};

fn texts(history: &ConversationHistory) -> Vec<&str> {
    history
        .messages
        .iter()
        .map(|message| message.text.as_str())
        .collect()
}

#[tokio::test]
async fn rewind_clear_and_fork_preserve_archives_and_choose_independent_session_seeds() -> Result<()>
{
    let (_dir, store, chat, scope) = setup().await?;
    std::fs::write(&scope.executable, b"fixture")?;
    let first = prepare(&store, &chat.id, scope.clone(), "First question").await?;
    confirm(&store, &first).await?;
    let second = prepare(&store, &chat.id, scope.clone(), "Second question").await?;
    let busy = store.conversation(chat.id.clone()).await?;
    assert!(
        store
            .fork_conversation(
                chat.id.clone(),
                busy.context_id.clone(),
                None,
                "Busy".into()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .rewind_conversation_history(chat.id.clone(), busy.context_id, None)
            .await
            .is_err()
    );
    confirm(&store, &second).await?;
    let full = store.conversation_history(chat.id.clone()).await?;
    let first_user = &full.messages[0];
    let first_answer = &full.messages[1];
    assert_eq!(
        texts(&full),
        ["First question", "answer", "Second question", "answer"]
    );
    assert!(store.conversation_history_revision(chat.id.clone(), second.context_id).await.is_err());

    let head_fork = store
        .fork_conversation(
            chat.id.clone(),
            full.context_id.clone(),
            None,
            "Head".into(),
        )
        .await?;
    let prefix_fork = store
        .fork_conversation(
            chat.id.clone(),
            full.context_id.clone(),
            Some(first_answer.revision_id.clone()),
            "Prefix".into(),
        )
        .await?;
    let edited = store
        .fork_conversation_with_message_edit(
            chat.id.clone(),
            full.context_id.clone(),
            first_user.revision_id.clone(),
            "Alternative".into(),
            "Revised question".into(),
        )
        .await?;
    assert!(
        store
            .fork_conversation_with_message_edit(
                chat.id.clone(),
                full.context_id.clone(),
                first_answer.revision_id.clone(),
                "Assistant edit".into(),
                "Forbidden".into()
            )
            .await
            .is_err()
    );
    assert_eq!(
        texts(&store.conversation_history(head_fork.id.clone()).await?),
        texts(&full)
    );
    assert_eq!(
        texts(&store.conversation_history(prefix_fork.id.clone()).await?),
        ["First question", "answer"]
    );
    assert_eq!(
        texts(&store.conversation_history(edited.id.clone()).await?),
        ["Revised question"]
    );
    let edited_source = store
        .inspect_conversation_history(edited.id.clone())
        .await?
        .fork
        .context("fork lineage missing")?;
    assert_eq!(
        edited_source.source_context_id.as_deref(),
        Some(full.context_id.as_str())
    );
    assert_eq!(
        edited_source.source_revision_id.as_deref(),
        Some(first_user.revision_id.as_str())
    );
    assert_eq!(
        edited_source
            .source
            .as_ref()
            .map(|source| source.id.as_str()),
        Some(chat.id.as_str())
    );
    assert!(
        store
            .conversation_sessions(head_fork.id.clone())
            .await?
            .is_empty()
    );

    let rewind = store
        .rewind_conversation_history(
            chat.id.clone(),
            full.context_id.clone(),
            Some(first_answer.revision_id.clone()),
        )
        .await?;
    assert_ne!(rewind, full.context_id);
    assert_eq!(
        texts(&store.conversation_history(chat.id.clone()).await?),
        ["First question", "answer"]
    );
    assert!(
        store
            .rewind_conversation_history(chat.id.clone(), full.context_id.clone(), None)
            .await
            .is_err()
    );
    let cleared = store
        .rewind_conversation_history(chat.id.clone(), rewind.clone(), None)
        .await?;
    assert!(
        store
            .conversation_history(chat.id.clone())
            .await?
            .messages
            .is_empty()
    );
    let archive_fork = store
        .fork_retained_conversation(
            chat.id.clone(),
            cleared.clone(),
            full.context_id.clone(),
            first_user.revision_id.clone(),
            "Archive".into(),
        )
        .await?;
    assert_eq!(
        texts(&store.conversation_history(archive_fork.id.clone()).await?),
        ["First question"]
    );
    assert!(
        store
            .fork_retained_conversation(
                chat.id.clone(),
                cleared.clone(),
                edited.context_id,
                first_user.revision_id.clone(),
                "Foreign".into()
            )
            .await
            .is_err()
    );

    drop(store);
    let store = Store::open(scope.checkout.join("store.sqlite"))?;
    let inspect = store.inspect_conversation_history(chat.id.clone()).await?;
    assert_eq!(
        inspect
            .previous
            .iter()
            .map(|entry| (
                entry.action.as_str(),
                entry.context_id.as_str(),
                entry.message_count
            ))
            .collect::<Vec<_>>(),
        [
            ("clear", rewind.as_str(), 2),
            ("rewind", full.context_id.as_str(), 4)
        ]
    );
    assert_eq!(
        texts(
            &store
                .conversation_history_revision(chat.id.clone(), full.context_id.clone())
                .await?
        ),
        texts(&full)
    );
    assert!(
        store
            .conversation_history_revision(archive_fork.id.clone(), full.context_id)
            .await
            .is_err()
    );

    for (conversation, input) in [
        (
            head_fork.id,
            vec![
                "First question",
                "answer",
                "Second question",
                "answer",
                "Continue",
            ],
        ),
        (prefix_fork.id, vec!["First question", "answer", "Continue"]),
        (edited.id, vec!["Revised question", "Continue"]),
        (archive_fork.id, vec!["First question", "Continue"]),
        (chat.id, vec!["Continue"]),
    ] {
        let next = prepare(&store, &conversation, scope.clone(), "Continue").await?;
        assert!(matches!(next.intent, SessionIntent::Start));
        assert_eq!(
            next.input
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            input
        );
    }
    Ok(())
}
