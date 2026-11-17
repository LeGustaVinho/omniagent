use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::store::Store;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationLibraryMoveTarget {
    pub conversation_id: String,
    pub checkout_id: String,
    pub expected_section_id: Option<String>,
}

impl Store {
    pub async fn move_conversations_to_library_section(
        &self,
        project_id: String,
        section_id: Option<String>,
        selection: Vec<ConversationLibraryMoveTarget>,
    ) -> Result<usize> {
        ensure!(!selection.is_empty(), "Select conversations to move");
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(id) = &section_id {
                let belongs: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM conversation_library_sections WHERE id = ?1 AND project_id = ?2)",
                    params![id, project_id],
                    |row| row.get(0),
                )?;
                ensure!(belongs, "Section does not belong to this project");
            }
            let mut moved = 0;
            let mut selected = HashSet::with_capacity(selection.len());
            {
                let mut section = tx.prepare(
                    "SELECT organization.section_id FROM conversations conversation
                     JOIN checkouts checkout ON checkout.id = conversation.checkout_id
                     LEFT JOIN conversation_library_organization organization ON organization.conversation_id = conversation.id
                     WHERE conversation.id = ?1 AND conversation.checkout_id = ?2 AND checkout.project_id = ?3 AND conversation.background = 0",
                )?;
                let mut assign = tx.prepare(
                    "INSERT INTO conversation_library_organization (conversation_id, section_id) VALUES (?1, ?2)
                     ON CONFLICT (conversation_id) DO UPDATE SET section_id = excluded.section_id",
                )?;
                for target in &selection {
                    ensure!(selected.insert(&target.conversation_id), "A conversation is selected more than once");
                    let current: Option<String> = section.query_row(
                        params![target.conversation_id, target.checkout_id, project_id],
                        |row| row.get(0),
                    ).optional()?.context("A conversation no longer belongs to the selected worktree and project")?;
                    ensure!(
                        current == target.expected_section_id,
                        "A selected conversation moved to a different section. Refresh the selection and try again."
                    );
                    if current != section_id {
                        moved += assign.execute(params![target.conversation_id, section_id])?;
                    }
                }
            }
            tx.commit()?;
            Ok(moved)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use crate::{
        conversations::{Conversation, ConversationLibraryChange},
        repository::register,
    };

    use super::*;

    fn git(cwd: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git.exe")
            .current_dir(cwd)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "fixture Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn target(
        chat: &Conversation,
        expected_section_id: Option<&str>,
    ) -> ConversationLibraryMoveTarget {
        ConversationLibraryMoveTarget {
            conversation_id: chat.id.clone(),
            checkout_id: chat.checkout_id.clone(),
            expected_section_id: expected_section_id.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn moves_exact_same_project_worktrees_atomically_and_preserves_drafts_and_flags()
    -> Result<()> {
        use ConversationLibraryChange::{Archive, Move, NewSection, Pin, Read};

        let fixture = tempfile::tempdir()?;
        let database = fixture.path().join("profile.sqlite");
        let store = Store::open(database.clone())?;
        let root = fixture.path().join("project");
        let linked = fixture.path().join("other worktree");
        fs::create_dir(&root)?;
        git(&root, &["init", "-q"])?;
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "Initial",
            ],
        )?;
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().context("worktree path")?,
            ],
        )?;
        let first = register(&store, root).await?;
        let second = register(&store, linked).await?;
        let project = first.project_id;
        assert_eq!(second.project_id, project);
        assert_ne!(first.checkout_id, second.checkout_id);
        let left = store
            .create_conversation(first.checkout_id, "Primary chat".into())
            .await?;
        let right = store
            .create_conversation(second.checkout_id, "Linked chat".into())
            .await?;
        for change in [
            NewSection {
                name: "Review".into(),
            },
            Move { section_id: None },
            Pin { pinned: true },
            Read { unread: true },
        ] {
            store
                .organize_conversation(left.checkout_id.clone(), left.id.clone(), change)
                .await?;
        }
        store
            .organize_conversation(
                right.checkout_id.clone(),
                right.id.clone(),
                Archive { archived: true },
            )
            .await?;
        store
            .save_conversation_draft(right.id.clone(), "A pending review".into())
            .await?;
        let sections = store
            .conversation_library_sections(Some(project.clone()))
            .await?;
        let destination = &sections[0].id;

        let wrong_checkout = ConversationLibraryMoveTarget {
            checkout_id: left.checkout_id.clone(),
            ..target(&right, None)
        };
        for invalid in [
            wrong_checkout,
            target(&right, Some("stale-section")),
            target(&left, None),
        ] {
            assert!(
                store
                    .move_conversations_to_library_section(
                        project.clone(),
                        Some(destination.clone()),
                        vec![target(&left, None), invalid],
                    )
                    .await
                    .is_err()
            );
            let active = store
                .conversation_library(None, Some(project.clone()))
                .await?;
            assert!(active[0].organization.section.is_none());
        }
        assert_eq!(
            store
                .move_conversations_to_library_section(
                    project.clone(),
                    Some(destination.clone()),
                    vec![target(&left, None), target(&right, None)],
                )
                .await?,
            2
        );
        drop(store);

        let reopened = Store::open(database)?;
        let active = reopened
            .conversation_library(None, Some(project.clone()))
            .await?;
        let archived = reopened
            .conversation_library_view(None, Some(project.clone()), true)
            .await?;
        assert!(active[0].organization.pinned && active[0].organization.unread);
        assert_eq!(
            &active[0].organization.section.as_ref().unwrap().id,
            destination
        );
        assert_eq!(
            &archived[0].organization.section.as_ref().unwrap().id,
            destination
        );
        assert_eq!(
            reopened.conversation_draft(right.id.clone()).await?,
            "A pending review"
        );
        assert_eq!(
            reopened
                .move_conversations_to_library_section(
                    project,
                    None,
                    vec![
                        target(&left, Some(destination)),
                        target(&right, Some(destination))
                    ],
                )
                .await?,
            2
        );
        Ok(())
    }
}
