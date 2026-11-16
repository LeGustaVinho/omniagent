use std::path::Path;

use anyhow::Result;
use rusqlite::params;
use serde::Serialize;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

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
}

impl Store {
    pub async fn conversation_library(
        &self,
        query: Option<String>,
        project_id: Option<String>,
    ) -> Result<Vec<ConversationLibraryEntry>> {
        let query = searchable(query.as_deref().unwrap_or_default().trim());
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT c.id, c.checkout_id, c.title, c.active_context_id,
                        project.id, project.root, checkout.root, activity.last_activity_at,
                        json_extract(session.scope, '$.provider'),
                        COALESCE(attempt.reported_model, json_extract(session.scope, '$.model')), run.status
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
                 WHERE c.background = 0 AND (?1 IS NULL OR project.id = ?1)
                 ORDER BY activity.last_activity_at DESC, activity.entry_order DESC, c.title COLLATE NOCASE, c.id",
            )?;
            let mut rows = statement.query(params![project_id])?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                let title: String = row.get(2)?;
                if !query.is_empty() && !searchable(&title).contains(&query) {
                    continue;
                }
                let root: String = row.get(5)?;
                let project_name = Path::new(&root).file_name().and_then(|name| name.to_str()).unwrap_or(&root).to_owned();
                let checkout_id: String = row.get(1)?;
                result.push(ConversationLibraryEntry {
                    conversation: Conversation { id: row.get(0)?, checkout_id: checkout_id.clone(), title, context_id: row.get(3)? },
                    checkout: ProjectCheckout { project_id: row.get(4)?, repository_root: root, checkout_id, checkout_root: row.get(6)? },
                    project_name, agent: row.get(8)?, model: row.get(9)?, status: row.get(10)?, last_activity_at: row.get(7)?,
                });
            }
            Ok(result)
        }).await
    }
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
