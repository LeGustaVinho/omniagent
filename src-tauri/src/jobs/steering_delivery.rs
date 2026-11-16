use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    conversations::{SessionScope, load_conversation, load_messages},
    dispatch::{ContextSnapshot, EventSource},
    protection,
    providers::{
        Capability, ProviderEventData, TurnInput,
        capabilities::{CapabilityStatus, CapabilitySubject},
        steering::{SteerOutcome, SteerTarget},
    },
    store::Store,
};

use super::{
    Job, JobPermission, QueueState, change_pending_job_state,
    history::{apply_job_history_policy, conversation_policy, load_binding},
    instructions, load_job, now_ms,
    policy::{attempt_requirements, effective_policy},
};

pub(crate) struct SteeringInput {
    pub target: SteerTarget,
    pub input: TurnInput,
}

struct LiveSteeringScope {
    target: SteerTarget,
    scope: SessionScope,
    snapshot: ContextSnapshot,
}

fn live_scope(db: &Connection, conversation: &str, run: &str) -> Result<Option<LiveSteeringScope>> {
    let row = db
        .query_row(
            "SELECT s.provider_session_id, a.provider_turn_id, s.scope, snap.payload
         FROM active_agent_attempts live JOIN agent_attempts a ON a.run_id = live.run_id
         JOIN agent_sessions s ON s.id = a.agent_session_id
         JOIN runs r ON r.id = a.run_id
         JOIN job_attempts j ON j.run_id = a.run_id
         JOIN context_snapshots snap ON snap.run_id = a.run_id
         JOIN dispatches d ON d.snapshot_id = snap.id AND d.ordinal = 1 AND d.delivery = 'submitted'
         WHERE live.conversation_id = ?1 AND a.run_id = ?2 AND a.detached_context = 0
           AND r.status = 'running' AND j.cancel_requested = 0 AND a.provider_terminal IS NULL
           AND s.provider_session_id IS NOT NULL AND a.provider_turn_id IS NOT NULL",
            params![conversation, run],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((session_id, turn_id, scope, snapshot)) = row else {
        return Ok(None);
    };
    let scope = serde_json::from_str(&scope)?;
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
    if snapshot.scope != scope
        || !snapshot.capabilities.as_ref().is_some_and(|assessment| {
            assessment.entries.iter().any(|entry| {
                matches!(
                    entry.descriptor.subject,
                    CapabilitySubject::Protocol {
                        capability: Capability::LiveSteering
                    }
                ) && entry.status() == CapabilityStatus::Verified
            })
        })
    {
        return Ok(None);
    }
    Ok(Some(LiveSteeringScope {
        target: SteerTarget {
            session_id,
            turn_id,
        },
        scope,
        snapshot,
    }))
}

fn delivery_policy(
    db: &Connection,
    conversation: &str,
    job_id: &str,
    run: &str,
    live: &LiveSteeringScope,
) -> Result<Option<(Job, String)>> {
    let selected = load_conversation(db, conversation)?;
    let project: String = db.query_row(
        "SELECT project_id FROM checkouts WHERE id = ?1",
        [&selected.checkout_id],
        |row| row.get(0),
    )?;
    let Some(job) = load_job(db, &project, job_id)? else {
        return Ok(None);
    };
    let Some(context) = job.context.as_ref() else {
        return Ok(None);
    };
    if context.conversation_id != conversation
        || job.checkout_id != selected.checkout_id
        || job.queue_state != QueueState::Queued
        || load_binding(db, &job.id)?.is_some()
        || crate::conversations::continuation::pinned_continuation(db, &job.id)?
        || !instructions::preserves_destination(
            db,
            conversation,
            &context.context_id,
            &selected.context_id,
        )?
        || live.snapshot.context_id != selected.context_id
    {
        return Ok(None);
    }
    let saved_scope: Option<Vec<u8>> = db.query_row(
        "SELECT operator_scope FROM jobs WHERE id = ?1",
        [job_id],
        |row| row.get(0),
    )?;
    let Some(saved_scope) = saved_scope else {
        return Ok(None);
    };
    if serde_json::from_slice::<SessionScope>(&protection::unprotect(&saved_scope)?)? != live.scope
    {
        return Ok(None);
    }
    let policy = effective_policy(db, &job)?;
    if policy.permission == JobPermission::Deny {
        return Ok(None);
    }
    let decision: Option<(String, String)> = db
        .query_row(
            "SELECT policy_key, decision FROM job_approvals WHERE job_id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if decision
        .as_ref()
        .is_some_and(|(_, decision)| decision == "denied")
        || (policy.permission == JobPermission::Review
            && !decision
                .as_ref()
                .is_some_and(|(key, decision)| key == &policy.key && decision == "approved"))
    {
        return Ok(None);
    }
    let Ok(active_requirements) = attempt_requirements(db, run)? else {
        return Ok(None);
    };
    if policy
        .requirements
        .capabilities
        .iter()
        .any(|item| !active_requirements.capabilities.contains(item))
        || policy
            .requirements
            .customizations
            .iter()
            .any(|item| !active_requirements.customizations.contains(item))
    {
        return Ok(None);
    }
    Ok(Some((job, policy.key)))
}

impl Store {
    pub async fn resolve_uncertain_steering(
        &self,
        conversation: String,
        job: String,
        revision: String,
        run: String,
        confirmed_undelivered: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let outcome = if confirmed_undelivered { "undelivered" } else { "discarded" };
            let changed = tx.execute(
                "UPDATE instruction_delivery_attempts SET superseded = 1, review_resolution = ?5, reviewed_ms = ?6
                 WHERE job_id = ?1 AND revision_id = ?2 AND run_id = ?3
                   AND steering_state = 'unconfirmed' AND superseded = 0
                   AND EXISTS (SELECT 1 FROM jobs WHERE id = ?1 AND conversation_id = ?4)
                   AND NOT EXISTS (SELECT 1 FROM active_agent_attempts WHERE run_id = ?3)",
                params![job, revision, run, conversation, outcome, now_ms()],
            )?;
            ensure!(changed == 1, "The steering attempt is still active or no longer awaits review");
            if confirmed_undelivered {
                recheck_fallback_destination(&tx, &conversation, &job)?;
            } else {
                let (project, _) = conversation_policy(&tx, &conversation)?;
                change_pending_job_state(&tx, &project, &job, QueueState::Cancelled)?;
            }
            tx.commit()?;
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub(crate) async fn claim_native_instruction(
        &self,
        conversation: String,
        job: String,
        revision: String,
        run: String,
    ) -> Result<Option<SteeringInput>> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(live) = live_scope(&tx, &conversation, &run)? else { return Ok(None) };
            let Some((job_record, policy)) = delivery_policy(&tx, &conversation, &job, &run, &live)? else { return Ok(None) };
            let available: bool = tx.query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM queued_conversation_instructions i
                    WHERE i.job_id = ?1 AND i.revision_id = ?2 AND i.attachment_labels IS NULL
                      AND NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = i.job_id)
                      AND NOT EXISTS (
                          SELECT 1 FROM instruction_delivery_attempts
                          WHERE (job_id = i.job_id AND (superseded = 0 OR run_id = ?3))
                             OR (run_id = ?3 AND steering_state IN ('claimed', 'prepared', 'unconfirmed'))
                      )
                )", params![job, revision, run], |row| row.get(0),
            )?;
            if !available {
                return Ok(None);
            }
            if instructions::predecessor(&tx, &job_record)?.is_some() {
                return Ok(None);
            }
            let message = load_messages(&tx, &conversation, std::slice::from_ref(&revision))?
                .pop().context("Queued instruction lost its revision")?;
            tx.execute(
                "INSERT INTO instruction_delivery_attempts(job_id, run_id, revision_id, policy_key, route,
                     provider_session_id, provider_turn_id, steering_state)
                 VALUES (?1, ?2, ?3, ?4, 'steering', ?5, ?6, 'claimed')",
                params![job, run, revision, policy, live.target.session_id, live.target.turn_id],
            )?;
            tx.commit()?;
            Ok(Some(SteeringInput {
                target: live.target,
                input: TurnInput { message_id: revision, text: message.text, attachments: Vec::new() },
            }))
        }).await
    }

    pub(super) async fn release_unsent_steering(
        &self,
        run: String,
        revision: String,
    ) -> Result<bool> {
        let released = self.execute(move |db| {
            Ok(db.execute(
                "UPDATE instruction_delivery_attempts SET steering_state = 'rejected', superseded = 1
                 WHERE run_id = ?1 AND revision_id = ?2 AND steering_state = 'claimed'",
                params![run, revision],
            )? == 1)
        }).await?;
        self.job_signal.send_replace(());
        Ok(released)
    }
}

#[derive(Debug)]
pub(crate) struct SteeringPreparationRejected;

impl std::fmt::Display for SteeringPreparationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The active turn no longer qualifies for steering")
    }
}

impl std::error::Error for SteeringPreparationRejected {}

pub(crate) fn record_event(
    db: &Connection,
    conversation: &str,
    run: &str,
    source: EventSource,
    seq: i64,
    event: &ProviderEventData,
) -> Result<()> {
    let (revision, target, outcome) = match event {
        ProviderEventData::SteeringPrepared {
            message_id, target, ..
        } if source == EventSource::Application => (message_id, target, None),
        ProviderEventData::SteeringSettled {
            message_id,
            target,
            outcome,
        } => (message_id, target, Some(*outcome)),
        _ => return Ok(()),
    };
    let row: Option<(String, String, String, String, String)> = db.query_row(
        "SELECT job_id, policy_key, provider_session_id, provider_turn_id, steering_state
         FROM instruction_delivery_attempts WHERE run_id = ?1 AND revision_id = ?2 AND route = 'steering'",
        params![run, revision],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).optional()?;
    let Some((job, policy, session, turn, state)) = row else {
        return Ok(());
    };
    ensure!(
        target.session_id == session && target.turn_id == turn,
        "Steering evidence does not match the claimed turn"
    );
    let (next, fallback) = match (state.as_str(), outcome) {
        ("claimed", None) => {
            let Some(live) = live_scope(db, conversation, run)? else {
                return Err(SteeringPreparationRejected.into());
            };
            if live.target != *target
                || !delivery_policy(db, conversation, &job, run, &live)?
                    .is_some_and(|(_, key)| key == policy)
            {
                return Err(SteeringPreparationRejected.into());
            }
            db.execute(
                "UPDATE instruction_delivery_attempts SET steering_state = 'prepared', prepared_event_seq = ?3
                 WHERE job_id = ?1 AND run_id = ?2",
                params![job, run, seq],
            )?;
            return Ok(());
        }
        ("prepared", Some(SteerOutcome::Accepted)) if source == EventSource::Provider => {
            ("accepted", false)
        }
        ("prepared", Some(SteerOutcome::Unconfirmed)) => ("unconfirmed", false),
        ("prepared", Some(SteerOutcome::Unavailable | SteerOutcome::ProviderRejected))
            if source == EventSource::Provider =>
        {
            ("rejected", true)
        }
        (
            "claimed",
            Some(
                SteerOutcome::StaleTurn
                | SteerOutcome::UnsupportedInput
                | SteerOutcome::Unavailable
                | SteerOutcome::Busy,
            ),
        ) => ("rejected", true),
        (_, Some(_)) => return Ok(()),
        _ => return Err(SteeringPreparationRejected.into()),
    };
    db.execute(
        "UPDATE instruction_delivery_attempts SET steering_state = ?3, superseded = ?4, settled_event_seq = ?5
         WHERE job_id = ?1 AND run_id = ?2",
        params![job, run, next, fallback, seq],
    )?;
    if fallback {
        recheck_fallback_destination(db, conversation, &job)?;
    }
    Ok(())
}

fn recheck_fallback_destination(db: &Connection, conversation: &str, id: &str) -> Result<()> {
    let (project, settings) = conversation_policy(db, conversation)?;
    let job = load_job(db, &project, id)?.context("Steering fallback lost its job")?;
    let selected = load_conversation(db, conversation)?;
    let recorded = job
        .context
        .as_ref()
        .context("Steering fallback lost its context")?;
    if !instructions::preserves_destination(
        db,
        conversation,
        &recorded.context_id,
        &selected.context_id,
    )? {
        apply_job_history_policy(
            db,
            &job,
            &recorded.context_id,
            false,
            settings
                .conversation_override
                .unwrap_or(settings.project_default),
        )?;
    }
    Ok(())
}

pub(crate) fn settle_interrupted(db: &Connection, run: &str) -> Result<()> {
    db.execute(
        "UPDATE instruction_delivery_attempts
         SET steering_state = CASE steering_state WHEN 'prepared' THEN 'unconfirmed' ELSE 'rejected' END,
             superseded = steering_state = 'claimed'
         WHERE run_id = ?1 AND steering_state IN ('claimed', 'prepared')",
        [run],
    )?;
    Ok(())
}

pub(crate) fn accepted_revisions(db: &Connection, run: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT revision_id FROM instruction_delivery_attempts
         WHERE run_id = ?1 AND steering_state = 'accepted' ORDER BY prepared_event_seq",
    )?;
    Ok(statement
        .query_map([run], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
