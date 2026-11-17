use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;
use tauri::State;

use crate::{diagnostics, runs::RunStatus, store::Store};

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedSavedAttempt {
    run_id: String,
    checkout_id: String,
    status: RunStatus,
    ordinal: u32,
    created_at: i64,
}

#[tauri::command]
pub async fn related_saved_attempts(
    store: State<'_, Store>,
    project_id: String,
    run_id: String,
    conversation_id: Option<String>,
) -> Result<Vec<RelatedSavedAttempt>, String> {
    store
        .execute(move |db| related(db, &project_id, &run_id, conversation_id.as_deref()))
        .await
        .map_err(|error| diagnostics::preview(&format!("{error:#}")))
}

fn related(
    db: &Connection,
    project: &str,
    run: &str,
    conversation: Option<&str>,
) -> Result<Vec<RelatedSavedAttempt>> {
    let sql = if conversation.is_some() {
        "SELECT r.id, r.checkout_id, r.status, a.ordinal, r.created_at
         FROM agent_attempts a JOIN runs r ON r.id = a.run_id JOIN checkouts c ON c.id = r.checkout_id
         WHERE c.project_id = ?2 AND a.conversation_id = ?3 AND r.kind = 'agent'
           AND r.status IN ('succeeded', 'failed', 'cancelled', 'interrupted')
           AND EXISTS(SELECT 1 FROM agent_events e WHERE e.run_id = r.id)
           AND EXISTS(SELECT 1 FROM agent_attempts anchor JOIN runs source ON source.id = anchor.run_id
             JOIN checkouts own ON own.id = source.checkout_id
             WHERE anchor.run_id = ?1 AND anchor.conversation_id = ?3 AND source.kind = 'agent'
               AND own.project_id = ?2 AND source.status IN ('succeeded', 'failed', 'cancelled', 'interrupted'))
         ORDER BY (r.id = ?1) DESC, a.ordinal DESC"
    } else {
        "SELECT r.id, r.checkout_id, r.status, a.ordinal, r.created_at
         FROM job_attempts a JOIN runs r ON r.id = a.run_id JOIN jobs j ON j.id = a.job_id
         JOIN checkouts c ON c.id = r.checkout_id
         JOIN job_command_outputs o ON o.run_id = r.id
         JOIN job_attempts anchor ON anchor.job_id = a.job_id
         JOIN runs source ON source.id = anchor.run_id
         JOIN checkouts own ON own.id = source.checkout_id
         WHERE anchor.run_id = ?1 AND j.project_id = ?2 AND ?3 IS NULL
           AND c.project_id = ?2 AND own.project_id = ?2
           AND r.kind = 'command' AND source.kind = 'command'
           AND r.status IN ('succeeded', 'failed', 'cancelled', 'interrupted')
           AND source.status IN ('succeeded', 'failed', 'cancelled', 'interrupted')
         ORDER BY (r.id = ?1) DESC, a.ordinal DESC"
    };
    let mut statement = db.prepare(sql)?;
    let rows = statement.query_map(params![run, project, conversation], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get::<_, String>(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    rows.map(|row| {
        let (run_id, checkout_id, status, ordinal, created_at) = row?;
        Ok(RelatedSavedAttempt {
            run_id,
            checkout_id,
            status: RunStatus::from_sql(&status)?,
            ordinal,
            created_at,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_saved_attempts_only_in_the_anchored_conversation_or_command_job() -> Result<()> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE checkouts(id TEXT, project_id TEXT);
             CREATE TABLE runs(id TEXT, checkout_id TEXT, status TEXT, kind TEXT, created_at INTEGER);
             CREATE TABLE agent_attempts(run_id TEXT, conversation_id TEXT, ordinal INTEGER);
             CREATE TABLE agent_events(run_id TEXT);
             CREATE TABLE jobs(id TEXT, project_id TEXT);
             CREATE TABLE job_attempts(job_id TEXT, run_id TEXT, ordinal INTEGER);
             CREATE TABLE job_command_outputs(run_id TEXT);
             INSERT INTO checkouts VALUES ('root','project'), ('linked','project'), ('foreign','another');
             INSERT INTO runs VALUES ('current','root','succeeded','agent',10), ('previous','linked','failed','agent',9),
               ('foreign','foreign','succeeded','agent',9), ('active','root','running','agent',12),
               ('different-conversation','root','succeeded','agent',8), ('without-evidence','root','failed','agent',7),
               ('command-current','root','succeeded','command',11), ('command-previous','linked','failed','command',6),
               ('other-command','root','succeeded','command',7), ('command-active','root','running','command',12),
               ('command-no-output','root','interrupted','command',13), ('command-foreign','foreign','failed','command',11);
             INSERT INTO agent_attempts VALUES ('current','conversation',5), ('previous','conversation',4),
               ('foreign','conversation',3), ('active','conversation',6), ('without-evidence','conversation',2),
               ('different-conversation','other',1), ('command-current','conversation',7);
             INSERT INTO agent_events VALUES ('current'), ('previous'), ('foreign'), ('active'),
               ('different-conversation'), ('command-current');
             INSERT INTO jobs VALUES ('job','project'), ('other-job','project');
             INSERT INTO job_attempts VALUES ('job','command-current',2), ('job','command-previous',1),
               ('other-job','other-command',1), ('job','command-active',3), ('job','command-no-output',4), ('job','command-foreign',5);
             INSERT INTO job_command_outputs VALUES ('command-current'), ('command-previous'), ('other-command'), ('command-active'), ('command-foreign');",
        )?;
        let records = related(&db, "project", "current", Some("conversation"))?;
        assert_eq!(
            records
                .iter()
                .map(|attempt| attempt.run_id.as_str())
                .collect::<Vec<_>>(),
            ["current", "previous"]
        );
        assert_eq!(records[1].checkout_id, "linked");
        assert_eq!(records[1].status, RunStatus::Failed);
        assert!(related(&db, "project", "current", Some("other"))?.is_empty());
        assert!(related(&db, "another", "current", Some("conversation"))?.is_empty());
        assert!(related(&db, "project", "active", Some("conversation"))?.is_empty());
        let command = related(&db, "project", "command-current", None)?;
        assert_eq!(
            command
                .iter()
                .map(|attempt| attempt.run_id.as_str())
                .collect::<Vec<_>>(),
            ["command-current", "command-previous"]
        );
        assert!(related(&db, "another", "command-current", None)?.is_empty());
        assert!(related(&db, "project", "command-active", None)?.is_empty());
        Ok(())
    }
}
