use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use uuid::Uuid;

use crate::{repository::ProjectCheckout, store::Store};

use super::Conversation;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationLibraryEntry {
    pub conversation: Conversation,
    pub checkout: ProjectCheckout,
    pub project_name: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub last_activity_at: Option<i64>,
    pub organization: ConversationLibraryOrganization,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationLibrarySection {
    pub id: String,
    pub project_id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationLibraryOrganization {
    pub pinned: bool,
    pub unread: bool,
    pub archived: bool,
    pub section: Option<ConversationLibrarySection>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConversationLibraryChange {
    Pin { pinned: bool },
    Read { unread: bool },
    Archive { archived: bool },
    Move { section_id: Option<String> },
    NewSection { name: String },
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConversationLibrarySectionChange {
    Rename { expected_name: String, name: String },
    Move { neighbor_id: String, before: bool },
}

impl Store {
    pub async fn conversation_library(
        &self,
        query: Option<String>,
        project_id: Option<String>,
    ) -> Result<Vec<ConversationLibraryEntry>> {
        self.conversation_library_view(query, project_id, false)
            .await
    }

    pub async fn conversation_library_view(
        &self,
        query: Option<String>,
        project_id: Option<String>,
        archived: bool,
    ) -> Result<Vec<ConversationLibraryEntry>> {
        let query = searchable(query.as_deref().unwrap_or_default().trim());
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT c.id, c.checkout_id, c.title, c.active_context_id,
                        project.id, project.root, checkout.root, activity.last_activity_at,
                        json_extract(session.scope, '$.provider'),
                        COALESCE(attempt.reported_model, json_extract(session.scope, '$.model')), run.status,
                        COALESCE(organization.pinned, 0), COALESCE(organization.unread, 0),
                        section.id, section.name
                 FROM conversations c
                 JOIN checkouts checkout ON checkout.id = c.checkout_id
                 JOIN projects project ON project.id = checkout.project_id
                 LEFT JOIN conversation_library_activity activity ON activity.conversation_id = c.id
                 LEFT JOIN active_agent_attempts active ON active.conversation_id = c.id
                 LEFT JOIN agent_attempts attempt ON attempt.run_id = COALESCE(active.run_id,
                     (SELECT prior.run_id FROM agent_attempts prior WHERE prior.conversation_id = c.id
                      ORDER BY prior.ordinal DESC LIMIT 1))
                 LEFT JOIN runs run ON run.id = attempt.run_id
                 LEFT JOIN agent_sessions session ON session.id = attempt.agent_session_id
                 LEFT JOIN conversation_library_organization organization ON organization.conversation_id = c.id
                 LEFT JOIN conversation_library_sections section
                    ON section.id = organization.section_id AND section.project_id = project.id
                 WHERE c.background = 0 AND (?1 IS NULL OR project.id = ?1) AND COALESCE(organization.archived, 0) = ?2
                 ORDER BY activity.last_activity_at DESC, activity.entry_order DESC, c.title COLLATE NOCASE, c.id",
            )?;
            let mut rows = statement.query(params![project_id, archived])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                let title: String = row.get(2)?;
                if !query.is_empty() && !searchable(&title).contains(&query) {
                    continue;
                }
                let root: String = row.get(5)?;
                let project_name = Path::new(&root).file_name().and_then(|name| name.to_str()).unwrap_or(&root).to_owned();
                let checkout_id: String = row.get(1)?;
                let project_id: String = row.get(4)?;
                let section = match row.get::<_, Option<String>>(13)? {
                    Some(id) => Some(ConversationLibrarySection { id, project_id: project_id.clone(), name: row.get(14)? }),
                    None => None,
                };
                result.push(ConversationLibraryEntry {
                    conversation: Conversation { id: row.get(0)?, checkout_id: checkout_id.clone(), title, context_id: row.get(3)? },
                    checkout: ProjectCheckout { project_id, repository_root: root, checkout_id, checkout_root: row.get(6)? },
                    project_name, agent: row.get(8)?, model: row.get(9)?, status: row.get(10)?, last_activity_at: row.get(7)?,
                    organization: ConversationLibraryOrganization { pinned: row.get(11)?, unread: row.get(12)?, archived, section },
                });
            }
            Ok(result)
        }).await
    }

    pub async fn conversation_library_sections(
        &self,
        project_id: Option<String>,
    ) -> Result<Vec<ConversationLibrarySection>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT id, project_id, name FROM conversation_library_sections
                 WHERE ?1 IS NULL OR project_id = ?1 ORDER BY project_id, display_order, id",
            )?;
            let sections = query
                .query_map([project_id], |row| {
                    Ok(ConversationLibrarySection {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        name: row.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(sections)
        })
        .await
    }

    pub async fn organize_conversation_library_section(
        &self,
        project_id: String,
        section_id: String,
        change: ConversationLibrarySectionChange,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            match change {
                ConversationLibrarySectionChange::Rename { expected_name, name } => {
                    let (name, name_key) = section_name(&name)?;
                    let exists = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM conversation_library_sections
                         WHERE project_id = ?1 AND name_key = ?2 AND id <> ?3)",
                        params![project_id, name_key, section_id], |row| row.get::<_, bool>(0),
                    )?;
                    ensure!(!exists, "A section named {name} already exists in this project");
                    let updated = tx.execute(
                        "UPDATE conversation_library_sections SET name = ?4, name_key = ?5
                         WHERE project_id = ?1 AND id = ?2 AND name = ?3",
                        params![project_id, section_id, expected_name, name, name_key],
                    )?;
                    ensure!(updated == 1, "This section changed or no longer belongs to the project. Refresh the library.");
                }
                ConversationLibrarySectionChange::Move { neighbor_id, before } => {
                    let ordered: Vec<(String, i64)> = tx.prepare(
                        "SELECT id, display_order FROM conversation_library_sections
                         WHERE project_id = ?1 ORDER BY display_order, id",
                    )?.query_map([&project_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                        .collect::<rusqlite::Result<_>>()?;
                    let index = ordered.iter().position(|(id, _)| *id == section_id)
                        .context("Section does not belong to this project")?;
                    let neighbor = if before { index.checked_sub(1) } else { Some(index + 1) };
                    let (_, neighbor_order) = neighbor.and_then(|index| ordered.get(index))
                        .filter(|(id, _)| *id == neighbor_id)
                        .context("Section order changed. Refresh the library and choose an adjacent section.")?;
                    tx.execute(
                        "UPDATE conversation_library_sections SET display_order = CASE id WHEN ?2 THEN ?4 ELSE ?5 END
                         WHERE project_id = ?1 AND id IN (?2, ?3)",
                        params![project_id, section_id, neighbor_id, neighbor_order, ordered[index].1],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn organize_conversation(
        &self,
        checkout_id: String,
        conversation_id: String,
        change: ConversationLibraryChange,
    ) -> Result<bool> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let project_id: String = tx.query_row(
                "SELECT checkout.project_id FROM conversations conversation
                 JOIN checkouts checkout ON checkout.id = conversation.checkout_id
                 WHERE conversation.id = ?1 AND conversation.checkout_id = ?2 AND conversation.background = 0",
                params![conversation_id, checkout_id], |row| row.get(0),
            ).optional()?.context("Conversation does not belong to this checkout")?;

            if !matches!(change, ConversationLibraryChange::Read { unread: false }) {
                tx.execute(
                    "INSERT INTO conversation_library_organization (conversation_id) VALUES (?1)
                     ON CONFLICT (conversation_id) DO NOTHING",
                    [&conversation_id],
                )?;
            }
            let updated = match change {
                ConversationLibraryChange::Pin { pinned } => tx.execute(
                    "UPDATE conversation_library_organization SET pinned = ?1 WHERE conversation_id = ?2",
                    params![pinned, conversation_id],
                )?,
                ConversationLibraryChange::Read { unread } => tx.execute(
                    "UPDATE conversation_library_organization SET unread = ?1 WHERE conversation_id = ?2 AND unread <> ?1",
                    params![unread, conversation_id],
                )?,
                ConversationLibraryChange::Archive { archived } => tx.execute(
                    "UPDATE conversation_library_organization SET archived = ?1 WHERE conversation_id = ?2",
                    params![archived, conversation_id],
                )?,
                ConversationLibraryChange::Move { section_id } => {
                    if let Some(id) = &section_id {
                        let exists = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM conversation_library_sections WHERE id = ?1 AND project_id = ?2)",
                            params![id, project_id], |row| row.get::<_, bool>(0),
                        )?;
                        ensure!(exists, "Section does not belong to this project");
                    }
                    tx.execute(
                        "UPDATE conversation_library_organization SET section_id = ?1 WHERE conversation_id = ?2",
                        params![section_id, conversation_id],
                    )?
                }
                ConversationLibraryChange::NewSection { name } => {
                    let (name, name_key) = section_name(&name)?;
                    let id = Uuid::new_v4().to_string();
                    let inserted = tx.execute(
                        "INSERT INTO conversation_library_sections (id, project_id, name, name_key, display_order)
                         SELECT ?1, ?2, ?3, ?4, COALESCE(MAX(display_order) + 1, 0)
                         FROM conversation_library_sections WHERE project_id = ?2
                         ON CONFLICT(project_id, name_key) DO NOTHING",
                        params![id, project_id, name, name_key],
                    )?;
                    ensure!(inserted == 1, "A section named {name} already exists in this project");
                    tx.execute(
                        "UPDATE conversation_library_organization SET section_id = ?1 WHERE conversation_id = ?2",
                        params![id, conversation_id],
                    )?
                }
            };
            tx.commit()?;
            Ok(updated != 0)
        }).await
    }
}

fn section_name(value: &str) -> Result<(&str, String)> {
    let name = value.trim();
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "Enter a valid section name"
    );
    Ok((name, name.nfc().flat_map(char::to_lowercase).collect()))
}

fn searchable(value: &str) -> String {
    value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests;
