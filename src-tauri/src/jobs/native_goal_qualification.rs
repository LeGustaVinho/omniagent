use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    conversations::SessionScope,
    protection,
    providers::{
        Capability, Provider, SessionMode,
        capabilities::{
            Availability, CapabilityAssessment, CapabilityDescriptor, CapabilityScope,
            CapabilitySubject, DeclaredSource, EvidenceBoundary, native_goal_scope_key, revision,
        },
        goal::{NativeGoalAction, NativeGoalEvidence, NativeGoalSupport},
    },
    store::Store,
};

use super::{GoalDefinition, Job};

const SUBJECT: CapabilitySubject = CapabilitySubject::Protocol {
    capability: Capability::NativeGoalCommand,
};

pub(crate) struct NativeGoalSeed {
    pub(crate) proof: NativeGoalEvidence,
    pub(crate) objective: String,
    pub(crate) managed_instructions: String,
    pub(crate) token_budget: Option<u64>,
}

impl Store {
    pub async fn register_native_goal_qualification(
        &self,
        profile: SessionScope,
        scope: CapabilityScope,
        controls: BTreeSet<NativeGoalAction>,
        token_budget_verified: bool,
    ) -> Result<()> {
        ensure!(
            profile.provider == Provider::Codex,
            "No supervised native goal adapter"
        );
        ensure!(
            scope
                == CapabilityScope::from_dispatch(
                    &profile,
                    scope.checkout.id.clone(),
                    scope.executable_revision.clone(),
                    scope.platform.clone(),
                    scope.configuration.clone(),
                    scope.reported_model.clone(),
                    scope.reported_effort.clone()
                ),
            "Native goal evidence belongs to a different launch profile"
        );
        let scope_key =
            native_goal_scope_key(&scope)?.context("The native goal scope is incomplete")?;
        let launch_key = revision(&(&profile, &scope.checkout.id))?;
        let assessment = self
            .resolve_capability_assessment(
                scope,
                vec![CapabilityDescriptor {
                    subject: SUBJECT,
                    source: DeclaredSource::Adapter,
                    availability: Availability::Unknown,
                    observed: None,
                    blocker: None,
                    boundary: EvidenceBoundary::ExactCheckout,
                }],
            )
            .await?;
        let evidence_id = assessment
            .entries
            .into_iter()
            .filter_map(|entry| entry.evidence)
            .find(|evidence| evidence.verified())
            .context("A successful authenticated fixture is required for this native goal profile")?
            .id;
        let qualification = NativeGoalEvidence {
            scope_key,
            evidence_id: evidence_id.clone(),
            controls,
            token_budget_verified,
        };
        ensure!(
            matches!(
                qualification.resolve(&qualification.scope_key, false),
                NativeGoalSupport::Qualified { .. }
            ),
            "The native goal fixture did not verify all lifecycle controls"
        );
        let encoded = protection::protect(&serde_json::to_vec(&qualification)?)?;
        self.execute(move |db| {
            db.execute(
                "INSERT INTO native_goal_qualifications VALUES (?1, ?2, ?3)
                 ON CONFLICT(launch_key) DO UPDATE SET evidence_id = excluded.evidence_id,
                    qualification = excluded.qualification",
                params![launch_key, evidence_id, encoded],
            )?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn invalidate_native_goal_qualification(
        &self,
        evidence_id: String,
    ) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "DELETE FROM native_goal_qualifications WHERE evidence_id = ?1",
                [evidence_id],
            )?;
            Ok(())
        })
        .await
    }
}

pub(super) fn candidate(
    db: &Connection,
    checkout: &str,
    scope: &SessionScope,
    definition: &GoalDefinition,
) -> Result<Option<NativeGoalEvidence>> {
    // Codex exposes token budgets but cannot enforce a hard number of autonomous turns.
    if scope.provider != Provider::Codex
        || scope.mode == SessionMode::Plan
        || definition.max_turns.is_some()
        || definition
            .token_budget
            .is_some_and(|tokens| tokens > i64::MAX as u64)
        || definition.native_objective().chars().count() > 4000
    {
        return Ok(None);
    }
    let qualification: Option<Vec<u8>> = db
        .query_row(
            "SELECT qualification FROM native_goal_qualifications WHERE launch_key = ?1",
            [revision(&(scope, checkout))?],
            |row| row.get(0),
        )
        .optional()?;
    qualification
        .map(|payload| -> Result<_> {
            let proof: NativeGoalEvidence =
                serde_json::from_slice(&protection::unprotect(&payload)?)?;
            let qualified = matches!(
                proof.resolve(&proof.scope_key, definition.token_budget.is_some()),
                NativeGoalSupport::Qualified { .. }
            );
            Ok(qualified.then_some(proof))
        })
        .transpose()
        .map(Option::flatten)
}

pub(super) fn native_goal_seed(
    db: &Connection,
    job: &Job,
    scope: Option<&SessionScope>,
) -> Result<Option<NativeGoalSeed>> {
    let Some(scope) = scope.filter(|_| job.latest_attempt.is_none()) else {
        return Ok(None);
    };
    let encoded: Option<Vec<u8>> = db.query_row(
        "SELECT r.definition FROM goal_revisions r JOIN active_goals active ON active.revision_id = r.id
         WHERE r.initial_job_id = ?1 AND r.continuation_owner = 'managed'",
        [&job.id], |row| row.get(0),
    ).optional()?;
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let definition: GoalDefinition = serde_json::from_slice(&protection::unprotect(&encoded)?)?;
    Ok(
        candidate(db, &job.checkout_id, scope, &definition)?.map(|proof| NativeGoalSeed {
            proof,
            objective: definition.native_objective(),
            managed_instructions: definition.instructions(),
            token_budget: definition.token_budget,
        }),
    )
}

pub(crate) fn effective_qualification_matches(
    proof: &NativeGoalEvidence,
    assessment: &CapabilityAssessment,
    token_budget_requested: bool,
) -> Result<bool> {
    let Some(scope_key) = native_goal_scope_key(&assessment.scope)? else {
        return Ok(false);
    };
    Ok(matches!(
        proof.resolve(&scope_key, token_budget_requested),
        NativeGoalSupport::Qualified { .. }
    ) && assessment.entries.iter().any(|entry| {
        entry.descriptor.subject == SUBJECT
            && entry.evidence.as_ref().is_some_and(|verification| {
                verification.verified() && verification.id == proof.evidence_id
            })
    }))
}
