use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::conversations::{AttemptPlan, MessageRevision};

use super::{
    DispatchRejection, JobPermission, JobRequirements, QueueState, load_job,
    policy::effective_policy,
};

pub(super) struct InstructionClaim {
    pub job_id: String,
    pub revision_id: String,
    pub policy_key: String,
    pub route: &'static str,
}

impl InstructionClaim {
    pub(super) fn record(&self, db: &Connection, plan: &AttemptPlan) -> Result<()> {
        ensure!(
            has_single_revision(&plan.input, &self.revision_id),
            "The receiving attempt must contain the claimed instruction exactly once"
        );
        db.execute(
            "INSERT INTO instruction_delivery_attempts (job_id, run_id, revision_id, policy_key, route)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.job_id, plan.run.id, self.revision_id, self.policy_key, self.route],
        )?;
        Ok(())
    }
}

fn has_single_revision(input: &[MessageRevision], revision: &str) -> bool {
    input
        .iter()
        .filter(|message| message.revision_id == revision)
        .take(2)
        .count()
        == 1
}

pub(super) fn adopted_run(db: &Connection, job: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT run_id FROM borrowed_instruction_deliveries WHERE job_id = ?1",
            [job],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn unconfirmed_adoption(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM instruction_delivery_attempts i
            JOIN context_snapshots s ON s.run_id = i.run_id
            JOIN dispatches d ON d.snapshot_id = s.id
            WHERE i.run_id = ?1 AND i.route = 'continuation' AND i.superseded = 0
              AND d.ordinal = 1 AND d.delivery <> 'submitted'
            UNION ALL SELECT 1 FROM borrowed_instruction_deliveries WHERE run_id = ?1 AND uncertain
         )",
        [run],
        |row| row.get(0),
    )?)
}

pub(super) fn unconfirmed_in_context(
    db: &Connection,
    conversation: &str,
    context: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT i.run_id FROM conversation_contexts c
         JOIN json_each(c.entries) e
         JOIN instruction_delivery_attempts i ON i.revision_id = e.value
         WHERE c.id = ?1 AND c.conversation_id = ?2
           AND i.superseded = 0 AND i.route IN ('standalone', 'continuation')
           AND (i.route = 'continuation' OR EXISTS (
               SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
               WHERE s.run_id = i.run_id AND d.ordinal = 1 AND d.delivery <> 'submitted'
           ))
           AND NOT EXISTS (
               SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
               WHERE s.run_id = i.run_id AND d.ordinal = 1 AND d.delivery = 'submitted'
           )
           AND NOT EXISTS (
               SELECT 1 FROM job_attempts original JOIN job_attempts recovery
                 ON recovery.job_id = original.job_id AND recovery.ordinal > original.ordinal
               JOIN context_snapshots s ON s.run_id = recovery.run_id
               JOIN dispatches d ON d.snapshot_id = s.id
               WHERE original.run_id = i.run_id AND original.job_id = i.job_id
                 AND d.ordinal = 1 AND d.delivery = 'submitted'
           )
         ORDER BY e.key LIMIT 1",
            params![context, conversation],
            |r| r.get(0),
        )
        .optional()?)
}

pub(super) fn carry_unsent_instruction(
    db: &Connection,
    previous: &str,
    plan: &AttemptPlan,
) -> Result<()> {
    let claim = db.query_row(
        "SELECT job_id, revision_id, policy_key, route FROM instruction_delivery_attempts i
         WHERE i.run_id = ?1 AND i.route <> 'steering' AND i.superseded = 0 AND NOT EXISTS (
             SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id WHERE s.run_id = i.run_id
         )",
        [previous],
        |row| Ok(InstructionClaim {
            job_id: row.get(0)?,
            revision_id: row.get(1)?,
            policy_key: row.get(2)?,
            route: if row.get_ref(3)?.as_str()? == "continuation" { "continuation" } else { "standalone" },
        }),
    ).optional()?;
    if let Some(claim) = claim {
        db.execute(
            "UPDATE instruction_delivery_attempts SET superseded = 1 WHERE run_id = ?1 AND job_id = ?2",
            params![previous, claim.job_id],
        )?;
        claim.record(db, plan)?;
    }
    Ok(())
}

pub(crate) fn validate_revisions(
    db: &Connection,
    run: &str,
    input: &[MessageRevision],
) -> Result<()> {
    let mut revisions = db.prepare(
        "SELECT revision_id FROM instruction_delivery_attempts WHERE run_id = ?1 AND route <> 'steering' AND superseded = 0
         UNION SELECT i.revision_id FROM job_attempts a JOIN queued_conversation_instructions i ON i.job_id = a.job_id
               WHERE a.run_id = ?1 AND a.kind = 'initial'",
    )?;
    for revision in revisions.query_map([run], |row| row.get::<_, String>(0))? {
        ensure!(
            has_single_revision(input, &revision?),
            "snapshot must contain the claimed instruction revision exactly once"
        );
    }
    Ok(())
}

pub(super) fn supplement_requirements(
    db: &Connection,
    run: &str,
    mut requirements: JobRequirements,
) -> Result<std::result::Result<JobRequirements, DispatchRejection>> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT j.project_id, j.id, i.policy_key FROM instruction_delivery_attempts i
         JOIN jobs j ON j.id = i.job_id
         WHERE i.run_id = ?1 AND i.route = 'continuation' AND i.superseded = 0",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((project, id, authorized_key)) = row else {
        return Ok(Ok(requirements));
    };
    let instruction = load_job(db, &project, &id)?.context("Claimed instruction is unavailable")?;
    let policy = effective_policy(db, &instruction)?;
    if instruction.queue_state != QueueState::Queued
        || policy.permission == JobPermission::Deny
        || policy.key != authorized_key
    {
        return Ok(Err(DispatchRejection::PolicyChanged));
    }
    if policy.permission == JobPermission::Review {
        let approved: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM job_approvals WHERE job_id = ?1 AND policy_key = ?2 AND decision = 'approved')",
            params![id, authorized_key], |r| r.get(0),
        )?;
        if !approved {
            return Ok(Err(DispatchRejection::PolicyChanged));
        }
    }
    for capability in policy.requirements.capabilities {
        if !requirements.capabilities.contains(&capability) {
            requirements.capabilities.push(capability);
        }
    }
    for customization in policy.requirements.customizations {
        if !requirements.customizations.contains(&customization) {
            requirements.customizations.push(customization);
        }
    }
    Ok(Ok(requirements))
}
