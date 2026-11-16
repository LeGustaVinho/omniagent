use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::{
    protection,
    providers::{Provider, ProviderEvent, ProviderEventData},
    runs::RunStatus,
    store::Store,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTreeLink {
    pub run_id: String,
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTreeAttempt {
    #[serde(flatten)]
    pub link: AgentTreeLink,
    pub ordinal: u32,
    pub previous_run_id: Option<String>,
    pub status: RunStatus,
    pub provider: Provider,
    pub model: Option<String>,
    pub reported_model: Option<String>,
    pub prompt_delivery: Option<String>,
    pub context_recorded: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTreeJob {
    pub id: String,
    pub parent: AgentTreeLink,
    pub queue_state: String,
    pub queued_recovery: bool,
    pub attempts: Vec<AgentTreeAttempt>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeChildObservation {
    pub run_id: String,
    pub event: ProviderEvent,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationAgentTree {
    pub parent: Option<AgentTreeLink>,
    pub attempts: Vec<AgentTreeAttempt>,
    pub managed: Vec<AgentTreeJob>,
    pub native_observations: Vec<NativeChildObservation>,
}

const SUBTREE: &str = "WITH RECURSIVE subtree(run_id) AS (
    SELECT ?1
    UNION
    SELECT child.run_id FROM subtree parent
    JOIN jobs j ON j.origin_run_id = parent.run_id AND j.project_id = ?2
    JOIN job_delegation_owners managed ON managed.job_id = j.id
    JOIN job_attempts child ON child.job_id = j.id
    JOIN agent_attempts agent ON agent.run_id = child.run_id
    WHERE NOT EXISTS(SELECT 1 FROM job_side_requests side WHERE side.job_id = j.id)
)";

const ATTEMPT_FIELDS: &str = "a.run_id, a.conversation_id, coalesce(jr.ordinal, a.ordinal),
    coalesce(jr.previous_run_id, a.previous_run_id), r.status,
    json_quote(json_extract(s.scope, '$.provider')), json_extract(s.scope, '$.model'), a.reported_model,
    (SELECT d.delivery FROM dispatches d WHERE d.snapshot_id = snapshot.id AND d.ordinal = 1),
    snapshot.id IS NOT NULL";

impl Store {
    pub async fn conversation_agent_tree(
        &self,
        conversation: String,
        run: String,
    ) -> Result<ConversationAgentTree> {
        self.execute(move |db| {
            let (project, job_id) = db.query_row(
                "SELECT c.project_id, j.job_id FROM agent_attempts a JOIN runs r ON r.id = a.run_id
                 JOIN checkouts c ON c.id = r.checkout_id
                 LEFT JOIN job_attempts j ON j.run_id = a.run_id
                 WHERE a.conversation_id = ?1 AND a.run_id = ?2",
                params![conversation, run], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            ).optional()?.context("the agent attempt is outside this conversation")?;
            let parent = if let Some(job) = &job_id {
                db.query_row(
                    "SELECT a.run_id, a.conversation_id FROM jobs j
                     JOIN job_delegation_owners managed ON managed.job_id = j.id
                     JOIN agent_attempts a ON a.run_id = j.origin_run_id
                     JOIN runs r ON r.id = a.run_id
                     JOIN checkouts c ON c.id = r.checkout_id AND c.project_id = j.project_id
                     WHERE j.id = ?1 AND j.project_id = ?2
                       AND NOT EXISTS(SELECT 1 FROM job_side_requests side WHERE side.job_id = j.id)",
                    params![job, project], |row| Ok(AgentTreeLink { run_id: row.get(0)?, conversation_id: row.get(1)? }),
                ).optional()?
            } else { None };
            Ok(ConversationAgentTree {
                parent,
                attempts: attempt_history(db, &project, &conversation, job_id.as_deref())?,
                managed: managed_children(db, &project, &run)?,
                native_observations: native_observations(db, &project, &run)?,
            })
        }).await
    }
}

fn attempt_history(
    db: &Connection,
    project: &str,
    conversation: &str,
    job: Option<&str>,
) -> Result<Vec<AgentTreeAttempt>> {
    let mut stmt = db.prepare(&format!(
        "SELECT {ATTEMPT_FIELDS} FROM agent_attempts a
         JOIN runs r ON r.id = a.run_id JOIN checkouts c ON c.id = r.checkout_id
         JOIN agent_sessions s ON s.id = a.agent_session_id
         LEFT JOIN job_attempts jr ON jr.run_id = a.run_id
         LEFT JOIN context_snapshots snapshot ON snapshot.run_id = a.run_id
         WHERE c.project_id = ?1 AND ((?3 IS NOT NULL AND jr.job_id = ?3)
             OR (?3 IS NULL AND a.conversation_id = ?2 AND jr.job_id IS NULL))
         ORDER BY coalesce(jr.ordinal, a.ordinal)"
    ))?;
    let mut rows = stmt.query(params![project, conversation, job])?;
    let mut attempts = Vec::new();
    while let Some(row) = rows.next()? {
        attempts.push(decode_attempt(row, 0)?);
    }
    Ok(attempts)
}

fn managed_children(db: &Connection, project: &str, root_run: &str) -> Result<Vec<AgentTreeJob>> {
    let mut stmt = db.prepare(&format!(
        "{SUBTREE}
        SELECT j.id, j.origin_run_id, p.conversation_id, j.queue_state,
            EXISTS(SELECT 1 FROM job_recoveries recovery WHERE recovery.job_id = j.id), {ATTEMPT_FIELDS}
        FROM subtree parent JOIN jobs j ON j.origin_run_id = parent.run_id AND j.project_id = ?2
        JOIN job_delegation_owners managed ON managed.job_id = j.id
        JOIN agent_attempts p ON p.run_id = parent.run_id
        LEFT JOIN job_attempts jr ON jr.job_id = j.id
        LEFT JOIN agent_attempts a ON a.run_id = jr.run_id
        LEFT JOIN runs r ON r.id = a.run_id
        LEFT JOIN agent_sessions s ON s.id = a.agent_session_id
        LEFT JOIN context_snapshots snapshot ON snapshot.run_id = a.run_id
        WHERE NOT EXISTS(SELECT 1 FROM job_commands cmd WHERE cmd.job_id = j.id)
          AND NOT EXISTS(SELECT 1 FROM job_side_requests side WHERE side.job_id = j.id)
        ORDER BY j.sequence, jr.ordinal"
    ))?;
    let mut rows = stmt.query(params![root_run, project])?;
    let mut managed: Vec<AgentTreeJob> = Vec::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if managed.last().is_none_or(|job| job.id != id) {
            managed.push(AgentTreeJob {
                id,
                parent: AgentTreeLink {
                    run_id: row.get(1)?,
                    conversation_id: row.get(2)?,
                },
                queue_state: row.get(3)?,
                queued_recovery: row.get(4)?,
                attempts: Vec::new(),
            });
        }
        if row.get::<_, Option<String>>(5)?.is_some() {
            managed
                .last_mut()
                .expect("the current job was recorded")
                .attempts
                .push(decode_attempt(row, 5)?);
        }
    }
    Ok(managed)
}

fn native_observations(
    db: &Connection,
    project: &str,
    root_run: &str,
) -> Result<Vec<NativeChildObservation>> {
    let mut stmt = db.prepare(&format!(
        "{SUBTREE}
        SELECT e.run_id, e.payload FROM subtree a JOIN agent_events e ON e.run_id = a.run_id
        JOIN native_subagent_observations n ON n.event_seq = e.seq
        WHERE e.source = 'provider' ORDER BY e.seq"
    ))?;
    let mut rows = stmt.query(params![root_run, project])?;
    let mut observations = Vec::new();
    while let Some(row) = rows.next()? {
        let event: ProviderEvent =
            serde_json::from_slice(&protection::unprotect(row.get_ref(1)?.as_blob()?)?)?;
        if matches!(event.data, ProviderEventData::ChildActivity { .. }) {
            observations.push(NativeChildObservation {
                run_id: row.get(0)?,
                event,
            });
        }
    }
    Ok(observations)
}

fn decode_attempt(row: &Row<'_>, start: usize) -> Result<AgentTreeAttempt> {
    Ok(AgentTreeAttempt {
        link: AgentTreeLink {
            run_id: row.get(start)?,
            conversation_id: row.get(start + 1)?,
        },
        ordinal: row.get(start + 2)?,
        previous_run_id: row.get(start + 3)?,
        status: RunStatus::from_sql(row.get_ref(start + 4)?.as_str()?)?,
        provider: serde_json::from_str(row.get_ref(start + 5)?.as_str()?)?,
        model: row.get(start + 6)?,
        reported_model: row.get(start + 7)?,
        prompt_delivery: row.get(start + 8)?,
        context_recorded: row.get(start + 9)?,
    })
}
