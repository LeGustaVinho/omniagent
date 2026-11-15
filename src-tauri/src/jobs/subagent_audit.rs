use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    conversations::SessionScope,
    dispatch::{ContextSnapshot, EventSource},
    protection,
    providers::{
        ProviderEvent, ProviderEventData,
        capabilities::{CapabilityScope, revision},
    },
    store::Store,
};

use super::{Job, JobOrigin, history::structured_job_run};

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS,
)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum SubagentAuditPolicy {
    #[default]
    Available,
    Complete,
}

impl SubagentAuditPolicy {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Complete => "complete",
        }
    }

    fn from_key(value: &str) -> Result<Self> {
        match value {
            "available" => Ok(Self::Available),
            "complete" => Ok(Self::Complete),
            _ => anyhow::bail!("Unknown subagent audit policy"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConversationSubagentAudit {
    pub project_default: SubagentAuditPolicy,
    pub conversation_override: Option<SubagentAuditPolicy>,
}

impl ConversationSubagentAudit {
    fn effective(&self) -> SubagentAuditPolicy {
        self.conversation_override.unwrap_or(self.project_default)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum SubagentAuditField {
    Identity,
    Prompt,
    ContextProvenance,
    Response,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum SubagentAuditSource {
    Native,
    Managed,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct SubagentAuditGap {
    pub source: SubagentAuditSource,
    pub affected_children: u32,
    pub unidentified: u32,
    pub missing: Vec<SubagentAuditField>,
    pub native_delegation_detected: bool,
}

#[derive(Default, PartialEq, Eq)]
pub(crate) enum SubagentAuditRoute {
    #[default]
    Available,
    Managed,
    Native {
        evidence_id: String,
    },
}

fn project_default(db: &Connection, project: &str) -> Result<SubagentAuditPolicy> {
    let value: Option<String> = db.query_row(
        "SELECT a.policy FROM projects p LEFT JOIN project_subagent_audit a ON a.project_id = p.id
         WHERE p.id = ?1",
        [project],
        |row| row.get(0),
    )?;
    value
        .as_deref()
        .map(SubagentAuditPolicy::from_key)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn conversation_settings(db: &Connection, conversation: &str) -> Result<ConversationSubagentAudit> {
    let (project, value): (String, Option<String>) = db.query_row(
        "SELECT w.project_id, a.policy FROM conversations c JOIN checkouts w ON w.id = c.checkout_id
         LEFT JOIN conversation_subagent_audit a ON a.conversation_id = c.id WHERE c.id = ?1",
        [conversation],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(ConversationSubagentAudit {
        project_default: project_default(db, &project)?,
        conversation_override: value
            .as_deref()
            .map(SubagentAuditPolicy::from_key)
            .transpose()?,
    })
}

impl Store {
    pub async fn project_subagent_audit(&self, project: String) -> Result<SubagentAuditPolicy> {
        self.execute(move |db| project_default(db, &project)).await
    }

    pub async fn set_project_subagent_audit(
        &self,
        project: String,
        policy: SubagentAuditPolicy,
    ) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_subagent_audit(project_id, policy) VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET policy = excluded.policy",
                params![project, policy.key()],
            )?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn conversation_subagent_audit(
        &self,
        conversation: String,
    ) -> Result<ConversationSubagentAudit> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            conversation_settings(&tx, &conversation)
        })
        .await
    }

    pub async fn set_conversation_subagent_audit(
        &self,
        conversation: String,
        policy: Option<SubagentAuditPolicy>,
    ) -> Result<ConversationSubagentAudit> {
        let settings = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(policy) = policy {
                tx.execute(
                    "INSERT INTO conversation_subagent_audit(conversation_id, policy) VALUES (?1, ?2)
                     ON CONFLICT(conversation_id) DO UPDATE SET policy = excluded.policy",
                    params![conversation, policy.key()],
                )?;
            } else {
                tx.execute("DELETE FROM conversation_subagent_audit WHERE conversation_id = ?1", [&conversation])?;
            }
            let settings = conversation_settings(&tx, &conversation)?;
            tx.commit()?;
            Ok(settings)
        }).await?;
        self.job_signal.send_replace(());
        Ok(settings)
    }

    pub(crate) async fn invalidate_native_subagent_audit(&self, evidence_id: String) -> Result<()> {
        self.execute(move |db| remove_candidate(db, &evidence_id))
            .await?;
        self.job_signal.send_replace(());
        Ok(())
    }
}

pub(super) fn job_audit_policy(
    db: &Connection,
    job: &Job,
    conversation: Option<&str>,
) -> Result<SubagentAuditPolicy> {
    let policy = conversation
        .map(|id| conversation_settings(db, id).map(|settings| settings.effective()))
        .unwrap_or_else(|| project_default(db, &job.project_id))?
        .max(stage_policy(db, &job.id)?);
    if let JobOrigin::Agent { run_id, .. } = &job.origin
        && has_complete_audit(db, run_id)?
    {
        return Ok(SubagentAuditPolicy::Complete);
    }
    Ok(policy)
}

pub(super) fn direct_audit_policy(
    db: &Connection,
    conversation: &str,
    source_run: Option<&str>,
) -> Result<SubagentAuditPolicy> {
    let mut policy = conversation_settings(db, conversation)?.effective();
    if let Some(run) = source_run {
        if has_complete_audit(db, run)? {
            policy = SubagentAuditPolicy::Complete;
        }
        if let Some(source) = structured_job_run(db, run)? {
            let job: String = db.query_row(
                "SELECT job_id FROM job_attempts WHERE run_id = ?1",
                [&source],
                |row| row.get(0),
            )?;
            policy = policy.max(stage_policy(db, &job)?);
        }
    }
    Ok(policy)
}

fn stage_policy(db: &Connection, job: &str) -> Result<SubagentAuditPolicy> {
    let value: Option<String> = db.query_row(
        "SELECT n.subagent_audit FROM job_pipeline_origins p JOIN job_pipeline_nodes n
         ON n.pipeline_run_id = p.pipeline_run_id AND n.instance_id = p.node_instance_id WHERE p.job_id = ?1",
        [job],
        |row| row.get(0),
    ).optional()?;
    value
        .as_deref()
        .map(SubagentAuditPolicy::from_key)
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(super) fn audit_checkout(db: &Connection, conversation: &str) -> Result<String> {
    Ok(db.query_row(
        "SELECT checkout_id FROM conversations WHERE id = ?1",
        [conversation],
        |row| row.get(0),
    )?)
}

pub(super) fn native_audit_candidate(
    db: &Connection,
    scope: &SessionScope,
    checkout: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT evidence_id FROM native_subagent_audit_candidates WHERE launch_key = ?1",
            [revision(&(scope, checkout))?],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn native_audit_fixture_key(
    db: &Connection,
    scope: &CapabilityScope,
    run: &str,
) -> Result<String> {
    let payload: Vec<u8> = db.query_row(
        "SELECT s.payload FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
         JOIN runs r ON r.id = s.run_id WHERE s.run_id = ?1 AND d.delivery = 'submitted' AND r.status = 'succeeded'",
        [run],
        |row| row.get(0),
    ).optional()?.context("Native subagent audit requires a successful fixture dispatch")?;
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
    ensure!(
        snapshot
            .capabilities
            .as_ref()
            .is_some_and(|assessment| &assessment.scope == scope),
        "The audit fixture belongs to a different effective CLI scope"
    );
    let (observed, gap) = native_gaps(db, run, false)?;
    ensure!(
        observed && gap.is_none(),
        "The fixture did not capture complete native child evidence"
    );
    revision(&(&snapshot.scope, &scope.checkout.id))
}

pub(crate) fn pin_attempt_audit(
    db: &Connection,
    run: &str,
    route: &SubagentAuditRoute,
) -> Result<()> {
    let (kind, evidence) = match route {
        SubagentAuditRoute::Available => return Ok(()),
        SubagentAuditRoute::Managed => ("managed", None),
        SubagentAuditRoute::Native { evidence_id } => ("native", Some(evidence_id)),
    };
    db.execute(
        "INSERT INTO attempt_subagent_audit(run_id, route, native_evidence_id) VALUES (?1, ?2, ?3)",
        params![run, kind, evidence],
    )?;
    Ok(())
}

fn has_complete_audit(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempt_subagent_audit WHERE run_id = ?1)",
        [run],
        |row| row.get(0),
    )?)
}

#[derive(Default)]
struct ChildCoverage {
    prompt: bool,
    context_provenance: bool,
    response: bool,
}

fn native_gaps(
    db: &Connection,
    run: &str,
    disabled: bool,
) -> Result<(bool, Option<SubagentAuditGap>)> {
    let mut statement = db.prepare(
        "SELECT e.payload FROM agent_events e JOIN native_subagent_observations n ON n.event_seq = e.seq
         WHERE e.run_id = ?1 ORDER BY e.seq",
    )?;
    let mut rows = statement.query([run])?;
    let mut children = BTreeMap::<(Option<String>, String), ChildCoverage>::new();
    let mut identified_items = BTreeSet::new();
    let mut unidentified_items = BTreeSet::new();
    let mut unidentified = 0;
    let mut violation = false;
    while let Some(row) = rows.next()? {
        let payload: Vec<u8> = row.get(0)?;
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        violation |= disabled && event.data.observed_native_subagent();
        let ProviderEventData::ChildActivity {
            child_id,
            status,
            details,
        } = event.data
        else {
            continue;
        };
        let Some(id) = child_id.filter(|id| !id.trim().is_empty()) else {
            if matches!(
                status.as_deref(),
                Some("failed" | "rejected" | "interrupted")
            ) {
                continue;
            }
            if let Some(item) = event.location.item_id {
                unidentified_items.insert((event.location.session_id, item));
            } else {
                unidentified += 1;
            }
            continue;
        };
        if let Some(item) = event.location.item_id {
            identified_items.insert((event.location.session_id.clone(), item));
        }
        let child = children.entry((event.location.session_id, id)).or_default();
        if let Some(details) = details {
            child.prompt |= present(details.prompt);
            child.context_provenance |= present(details.context_provenance);
            child.response |= present(details.response);
        }
    }
    unidentified += unidentified_items.difference(&identified_items).count() as u32;
    let mut missing = BTreeSet::new();
    if unidentified > 0 {
        missing.insert(SubagentAuditField::Identity);
    }
    let mut affected_children = 0;
    for child in children.values() {
        if !child.prompt || !child.context_provenance || !child.response {
            affected_children += 1;
            missing.extend(
                [
                    (!child.prompt).then_some(SubagentAuditField::Prompt),
                    (!child.context_provenance).then_some(SubagentAuditField::ContextProvenance),
                    (!child.response).then_some(SubagentAuditField::Response),
                ]
                .into_iter()
                .flatten(),
            );
        }
    }
    Ok((
        !children.is_empty(),
        (!missing.is_empty() || violation).then(|| SubagentAuditGap {
            source: SubagentAuditSource::Native,
            affected_children,
            unidentified,
            missing: missing.into_iter().collect(),
            native_delegation_detected: violation,
        }),
    ))
}

fn present(value: Option<String>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

pub(crate) fn settle_subagent_audit(
    db: &Connection,
    run: &str,
    has_response: bool,
) -> Result<bool> {
    let audit: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT route, native_evidence_id FROM attempt_subagent_audit WHERE run_id = ?1",
            [run],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let disabled: bool = db.query_row(
        "SELECT s.native_subagent_limit IS 0 FROM agent_attempts a JOIN agent_sessions s ON s.id = a.agent_session_id
         WHERE a.run_id = ?1",
        [run],
        |row| row.get(0),
    )?;
    let (_, native_gap) = native_gaps(db, run, disabled)?;
    let managed_gap = managed_gap(db, run, has_response)?;
    let reject = native_gap
        .as_ref()
        .is_some_and(|gap| gap.native_delegation_detected)
        || audit.is_some() && (native_gap.is_some() || managed_gap.is_some());
    if let Some((_, Some(evidence))) = audit
        && native_gap.is_some()
    {
        remove_candidate(db, &evidence)?;
    }
    for report in [native_gap, managed_gap].into_iter().flatten() {
        let event = ProviderEvent {
            location: Default::default(),
            data: ProviderEventData::SubagentAuditGap { report },
        };
        db.execute(
            "INSERT INTO agent_events(run_id, source, payload) VALUES (?1, ?2, ?3)",
            params![
                run,
                EventSource::Application.as_sql(),
                protection::protect(&serde_json::to_vec(&event)?)?
            ],
        )?;
    }
    Ok(reject)
}

fn managed_gap(db: &Connection, run: &str, has_response: bool) -> Result<Option<SubagentAuditGap>> {
    let managed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM job_attempts a JOIN job_delegation_owners d ON d.job_id = a.job_id WHERE a.run_id = ?1)",
        [run],
        |row| row.get(0),
    )?;
    if !managed {
        return Ok(None);
    }
    let (submitted, provenance): (bool, bool) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatches d JOIN context_snapshots s ON s.id = d.snapshot_id
                       WHERE s.run_id = a.run_id AND d.delivery = 'submitted'),
                EXISTS(SELECT 1 FROM context_snapshots s WHERE s.run_id = a.run_id AND s.context_id = a.context_id)
         FROM agent_attempts a WHERE run_id = ?1",
        [run],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let missing: Vec<_> = [
        (!submitted).then_some(SubagentAuditField::Prompt),
        (!provenance).then_some(SubagentAuditField::ContextProvenance),
        (!has_response).then_some(SubagentAuditField::Response),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok((!missing.is_empty()).then_some(SubagentAuditGap {
        source: SubagentAuditSource::Managed,
        affected_children: 1,
        unidentified: 0,
        missing,
        native_delegation_detected: false,
    }))
}

fn remove_candidate(db: &Connection, evidence_id: &str) -> Result<()> {
    db.execute(
        "DELETE FROM native_subagent_audit_candidates WHERE evidence_id = ?1",
        [evidence_id],
    )?;
    Ok(())
}
