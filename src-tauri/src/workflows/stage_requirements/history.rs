use super::*;
use crate::providers::capabilities::verification_still_current;

#[cfg(test)]
mod tests;

pub(super) fn match_profile(
    db: &Connection,
    project: &str,
    selection: ModelSelection,
    configured: &StageRequirementConfiguration,
    current: &CapabilityScope,
    inventory: &[StageInventoryChoice],
) -> Result<StageProfileMatch> {
    let required = effective_requirements(db, project, configured)?;
    let mut result = StageProfileMatch {
        selection,
        attempt_id: None,
        checked_at: None,
        blockers: Vec::new(),
    };
    if required.is_empty() {
        result
            .blockers
            .push("No role or stage requirements have been selected.".into());
        return Ok(result);
    }
    for source in &required.sources {
        if !inventory
            .iter()
            .any(|entry| same_source(&entry.source, source))
        {
            result.blockers.push(match source {
                RequiredStageSource::Instruction { path, .. } => format!(
                    "Required instruction {} is absent from the live inventory.",
                    path.display()
                ),
                RequiredStageSource::Skill { path, .. } => format!(
                    "Required skill {} is absent from the live inventory.",
                    path.display()
                ),
                RequiredStageSource::Plugin { identity, .. } => format!(
                    "Required plugin installation {} is absent from the live inventory.",
                    identity.id
                ),
            });
        }
    }
    if !result.blockers.is_empty() {
        return Ok(result);
    }
    let mut query = db.prepare(
        "SELECT a.run_id, s.checked_at, s.payload FROM workflow_stage_assessments s
         JOIN job_attempts a ON a.run_id = s.run_id
         JOIN jobs j ON j.id = a.job_id
         WHERE j.project_id = ?1 AND j.checkout_id = ?2
         ORDER BY s.checked_at DESC, a.ordinal DESC, a.run_id DESC LIMIT 128",
    )?;
    let mut rows = query.query(rusqlite::params![project, current.checkout.id])?;
    while let Some(row) = rows.next()? {
        let saved: WorkflowStageAssessment =
            serde_json::from_slice(&protection::unprotect(row.get_ref(2)?.as_blob()?)?)?;
        if !same_scope(&saved.scope, current)
            || saved.role_template != configured.role_template
            || !has_requirements(&saved, &required)
        {
            continue;
        }
        result.attempt_id = Some(row.get(0)?);
        result.checked_at = Some(row.get(1)?);
        for decision in &saved.decisions {
            if let Some(blocker) = decision.blocker() {
                result.blockers.push(blocker.to_owned());
            } else if let StageRequirementDecision::Capability { evidence_id, .. } = decision
                && !evidence_id
                    .as_ref()
                    .map(|id| verification_still_current(db, id))
                    .transpose()?
                    .unwrap_or(false)
            {
                result.blockers.push(
                    "A required capability's authenticated verification has been superseded."
                        .into(),
                );
            }
        }
        return Ok(result);
    }
    result.blockers.push("No recent assessment matched this checkout, CLI build, account, model, mode and configuration files.".into());
    Ok(result)
}

fn same_source(candidate: &RequiredStageSource, required: &RequiredStageSource) -> bool {
    if let RequiredStageSource::Skill { invoke, .. } = required {
        let mut candidate = candidate.clone();
        if let RequiredStageSource::Skill { invoke: flag, .. } = &mut candidate {
            *flag = *invoke;
        }
        &candidate == required
    } else {
        candidate == required
    }
}

fn same_scope(previous: &CapabilityScope, current: &CapabilityScope) -> bool {
    previous.checkout == current.checkout
        && previous.provider == current.provider
        && previous.transport == current.transport
        && previous.profile_id == current.profile_id
        && previous.cli_version == current.cli_version
        && previous.executable_revision == current.executable_revision
        && previous.account_binding.is_some()
        && previous.account_binding == current.account_binding
        && previous.requested_model == current.requested_model
        && previous.requested_effort == current.requested_effort
        && previous.runner == current.runner
        && previous.platform == current.platform
        && previous.mode == current.mode
        && previous.permission_revision == current.permission_revision
        && previous.configuration.detected == current.configuration.detected
        && previous.configuration.unresolved == current.configuration.unresolved
}

fn has_requirements(saved: &WorkflowStageAssessment, required: &StageRequirements) -> bool {
    saved.decisions.len() == required.capabilities.len() + required.sources.len()
        && required.capabilities.iter().all(|subject| saved.decisions.iter().any(
            |decision| matches!(decision, StageRequirementDecision::Capability { subject: observed, .. } if observed == subject)))
        && required.sources.iter().all(|source| saved.decisions.iter().any(
            |decision| matches!(decision, StageRequirementDecision::Source { source: observed, .. } if observed == source)))
}
