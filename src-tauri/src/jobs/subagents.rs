use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::{
    protection,
    providers::{Provider, ProviderEvent, ProviderEventData},
    store::Store,
};

use super::{AdmissionBlocker, Job, JobKind, history::structured_job_run};

const DEFAULT_LIMIT: u32 = 3;
const MAX_LIMIT: u32 = 32;
const STRICT_CONTROL_REQUIRED: &str =
    "Strict total requires a profile with verified native delegation controls";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum SubagentLimitMode {
    #[default]
    Managed,
    Strict,
}

impl SubagentLimitMode {
    fn key(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Strict => "strict",
        }
    }

    fn from_key(value: &str) -> Result<Self> {
        match value {
            "managed" => Ok(Self::Managed),
            "strict" => Ok(Self::Strict),
            _ => anyhow::bail!("Unknown subagent limit mode"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ProjectSubagentPolicy {
    pub mode: SubagentLimitMode,
    pub maximum: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct SubagentCounts {
    pub active: u32,
    pub queued: u32,
    pub historical: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct NativeSubagentCounts {
    pub provider: Provider,
    pub reported: SubagentCounts,
    pub unknown_state: u32,
    pub unidentified_events: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConversationSubagentQuota {
    pub project: ProjectSubagentPolicy,
    pub mode_override: Option<SubagentLimitMode>,
    pub requested: u32,
    pub authorized: u32,
    pub managed: SubagentCounts,
    pub native: Vec<NativeSubagentCounts>,
}

struct EffectiveQuota {
    project: ProjectSubagentPolicy,
    mode_override: Option<SubagentLimitMode>,
    requested: u32,
}

impl EffectiveQuota {
    fn mode(&self) -> SubagentLimitMode {
        self.mode_override.unwrap_or(self.project.mode)
    }

    fn authorized(&self) -> u32 {
        self.requested.min(self.project.maximum)
    }
}

fn project_policy(db: &Connection, project: &str) -> Result<ProjectSubagentPolicy> {
    let (mode, maximum): (Option<String>, Option<u32>) = db.query_row(
        "SELECT p.mode, p.maximum FROM projects j
         LEFT JOIN project_subagent_policies p ON p.project_id = j.id WHERE j.id = ?1",
        [project],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(ProjectSubagentPolicy {
        mode: mode
            .as_deref()
            .map(SubagentLimitMode::from_key)
            .transpose()?
            .unwrap_or_default(),
        maximum: maximum.unwrap_or(DEFAULT_LIMIT),
    })
}

fn effective_quota(db: &Connection, conversation: &str) -> Result<EffectiveQuota> {
    let (project, mode, requested): (String, Option<String>, Option<u32>) = db.query_row(
        "SELECT w.project_id, q.mode, q.requested
         FROM conversations c JOIN checkouts w ON w.id = c.checkout_id
         LEFT JOIN conversation_subagent_quotas q ON q.conversation_id = c.id WHERE c.id = ?1",
        [conversation],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(EffectiveQuota {
        project: project_policy(db, &project)?,
        mode_override: mode
            .as_deref()
            .map(SubagentLimitMode::from_key)
            .transpose()?,
        requested: requested.unwrap_or(DEFAULT_LIMIT),
    })
}

fn quota_snapshot(db: &Connection, conversation: &str) -> Result<ConversationSubagentQuota> {
    let quota = effective_quota(db, conversation)?;
    let managed = db.query_row(
        "SELECT count(*) FILTER (WHERE state = 'active'), count(*) FILTER (WHERE state = 'queued'),
                count(*) FILTER (WHERE state = 'historical')
         FROM managed_subagent_states WHERE conversation_id = ?1",
        [conversation],
        |row| {
            Ok(SubagentCounts {
                active: row.get(0)?,
                queued: row.get(1)?,
                historical: row.get(2)?,
            })
        },
    )?;
    let mut query = db.prepare(
        "WITH latest AS (
             SELECT session_id, child_key, state,
                    row_number() OVER (
                        PARTITION BY session_id, child_key IS NOT NULL, coalesce(child_key, event_seq)
                        ORDER BY state IS NOT NULL DESC, event_seq DESC
                    ) AS ordinal
             FROM native_subagent_observations WHERE conversation_id = ?1
         )
         SELECT json_quote(json_extract(s.scope, '$.provider')),
                count(*) FILTER (WHERE child_key IS NOT NULL AND state = 'active'),
                count(*) FILTER (WHERE child_key IS NOT NULL AND state = 'queued'),
                count(*) FILTER (WHERE child_key IS NOT NULL AND state = 'historical'),
                count(*) FILTER (WHERE child_key IS NOT NULL AND coalesce(state, 'unknown') = 'unknown'),
                count(*) FILTER (WHERE child_key IS NULL)
         FROM latest n JOIN agent_sessions s ON s.id = n.session_id
         WHERE ordinal = 1 GROUP BY 1 ORDER BY 1",
    )?;
    let rows = query.query_map([conversation], |row| {
        Ok((
            row.get::<_, String>(0)?,
            SubagentCounts {
                active: row.get(1)?,
                queued: row.get(2)?,
                historical: row.get(3)?,
            },
            row.get(4)?,
            row.get(5)?,
        ))
    })?;
    let native = rows
        .map(|row| {
            let (provider, reported, unknown_state, unidentified_events) = row?;
            Ok(NativeSubagentCounts {
                provider: serde_json::from_str(&provider)?,
                reported,
                unknown_state,
                unidentified_events,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ConversationSubagentQuota {
        project: quota.project,
        mode_override: quota.mode_override,
        requested: quota.requested,
        authorized: quota.authorized(),
        managed,
        native,
    })
}

impl Store {
    pub async fn project_subagent_policy(&self, project: String) -> Result<ProjectSubagentPolicy> {
        self.execute(move |db| project_policy(db, &project)).await
    }

    pub async fn set_project_subagent_policy(
        &self,
        project: String,
        policy: ProjectSubagentPolicy,
    ) -> Result<()> {
        ensure!(
            policy.maximum <= MAX_LIMIT,
            "Subagent limit must be between 0 and {MAX_LIMIT}"
        );
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_subagent_policies(project_id, mode, maximum) VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode, maximum = excluded.maximum",
                params![project, policy.mode.key(), policy.maximum],
            )?;
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn conversation_subagent_quota(
        &self,
        conversation: String,
    ) -> Result<ConversationSubagentQuota> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            quota_snapshot(&tx, &conversation)
        })
        .await
    }

    pub async fn set_conversation_subagent_quota(
        &self,
        conversation: String,
        mode_override: Option<SubagentLimitMode>,
        requested: u32,
    ) -> Result<ConversationSubagentQuota> {
        ensure!(
            requested <= MAX_LIMIT,
            "Subagent limit must be between 0 and {MAX_LIMIT}"
        );
        let state = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO conversation_subagent_quotas(conversation_id, mode, requested) VALUES (?1, ?2, ?3)
                 ON CONFLICT(conversation_id) DO UPDATE SET mode = excluded.mode, requested = excluded.requested",
                params![conversation, mode_override.map(SubagentLimitMode::key), requested],
            )?;
            let snapshot = quota_snapshot(&tx, &conversation)?;
            tx.commit()?;
            Ok(snapshot)
        }).await?;
        self.job_signal.send_replace(());
        Ok(state)
    }
}

fn delegation_owner(db: &Connection, run: &str) -> Result<String> {
    if let Some(source) = structured_job_run(db, run)? {
        let root: Option<String> = db
            .query_row(
                "SELECT d.conversation_id FROM job_attempts a
             JOIN job_delegation_owners d ON d.job_id = a.job_id WHERE a.run_id = ?1",
                [&source],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(root) = root {
            return Ok(root);
        }
    }
    Ok(db.query_row(
        "SELECT conversation_id FROM agent_attempts WHERE run_id = ?1",
        [run],
        |row| row.get(0),
    )?)
}

pub(super) fn record_delegation_owner(db: &Connection, job: &str, run: &str) -> Result<()> {
    let owner = delegation_owner(db, run)?;
    db.execute(
        "INSERT INTO job_delegation_owners(job_id, conversation_id) VALUES (?1, ?2)",
        params![job, owner],
    )?;
    Ok(())
}

pub(super) fn job_quota_blocker(db: &Connection, job: &Job) -> Result<Option<AdmissionBlocker>> {
    if job.kind != JobKind::Agent {
        return Ok(None);
    }
    let root: Option<String> = db
        .query_row(
            "SELECT conversation_id FROM job_delegation_owners WHERE job_id = ?1",
            [&job.id],
            |row| row.get(0),
        )
        .optional()?;
    let conversation = root
        .as_deref()
        .or_else(|| job.context.as_ref().map(|c| c.conversation_id.as_str()))
        .or_else(|| {
            job.latest_attempt
                .as_ref()
                .and_then(|attempt| attempt.conversation_id.as_deref())
        });
    let Some(conversation) = conversation else {
        let strict = project_policy(db, &job.project_id)?.mode == SubagentLimitMode::Strict;
        return Ok(strict.then_some(AdmissionBlocker::Policy {
            reason: STRICT_CONTROL_REQUIRED,
        }));
    };
    let quota = effective_quota(db, conversation)?;
    if quota.mode() == SubagentLimitMode::Strict {
        return Ok(Some(AdmissionBlocker::Policy {
            reason: STRICT_CONTROL_REQUIRED,
        }));
    }
    let Some(conversation_id) = root else {
        return Ok(None);
    };
    let active: u32 = db.query_row(
        "SELECT count(*) FROM managed_subagent_states WHERE conversation_id = ?1 AND state = 'active'",
        [&conversation_id],
        |row| row.get(0),
    )?;
    let limit = quota.authorized();
    Ok(
        (active >= limit).then_some(AdmissionBlocker::SubagentQuota {
            conversation_id,
            active,
            limit,
        }),
    )
}

pub(crate) fn check_conversation_subagent_controls(
    db: &Connection,
    conversation: &str,
) -> Result<()> {
    ensure!(
        effective_quota(db, conversation)?.mode() != SubagentLimitMode::Strict,
        STRICT_CONTROL_REQUIRED
    );
    Ok(())
}

pub(crate) fn record_native_subagent(
    db: &Connection,
    run: &str,
    seq: i64,
    event: &ProviderEventData,
) -> Result<()> {
    let ProviderEventData::ChildActivity {
        child_id, status, ..
    } = event
    else {
        return Ok(());
    };
    let conversation = delegation_owner(db, run)?;
    let key: Option<[u8; 32]> = child_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| Sha256::digest(id.as_bytes()).into());
    let state = status.as_deref().map(|status| match status {
        "pendingInit" | "pending" | "queued" => "queued",
        "running" | "started" | "task_started" | "task_progress" => "active",
        "interrupted" | "completed" | "errored" | "shutdown" | "failed" | "stopped"
        | "cancelled" | "canceled" | "finished" => "historical",
        _ => "unknown",
    });
    let rows = db.execute(
        "INSERT INTO native_subagent_observations(event_seq, conversation_id, session_id, child_key, state)
         SELECT ?1, ?2, agent_session_id, ?3, ?4 FROM agent_attempts WHERE run_id = ?5",
        params![seq, conversation, key, state, run],
    )?;
    ensure!(rows == 1, "Provider observation lost its session");
    Ok(())
}

pub(crate) fn migrate_subagent_observations(db: &Connection) -> Result<()> {
    // Jobs are enqueued after the origin attempt and its job, so parent owners are available first.
    let mut jobs = db.prepare(
        "SELECT id, origin_run_id FROM jobs WHERE origin_run_id IS NOT NULL ORDER BY sequence",
    )?;
    let mut rows = jobs.query([])?;
    while let Some(row) = rows.next()? {
        let (job, origin): (String, String) = (row.get(0)?, row.get(1)?);
        record_delegation_owner(db, &job, &origin)?;
    }
    let mut events = db.prepare(
        "SELECT seq, run_id, payload FROM agent_events WHERE source = 'provider' ORDER BY seq",
    )?;
    let mut rows = events.query([])?;
    while let Some(row) = rows.next()? {
        let event = row.get_ref(2)?.as_blob()?;
        let Ok(clear) = protection::unprotect(event) else {
            continue;
        };
        // Old or unrecognized payloads remain available as historical evidence.
        let Ok(event) = serde_json::from_slice::<ProviderEvent>(&clear) else {
            continue;
        };
        if matches!(event.data, ProviderEventData::ChildActivity { .. }) {
            let (seq, run): (i64, String) = (row.get(0)?, row.get(1)?);
            record_native_subagent(db, &run, seq, &event.data)
                .with_context(|| format!("Backfill native subagent event {seq}"))?;
        }
    }
    Ok(())
}
