use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    mcp_bridge::{RunScope, read_active_scope},
    protection,
    runs::load_run,
    store::Store,
};

use super::digest;

mod model;
mod resources;
#[cfg(test)]
mod tests;

pub use model::*;
pub(crate) use resources::{read_question_timeout_setting, write_question_timeout_setting};

pub(super) fn template_label_for_display(
    db: &Connection,
    project: &str,
    template_id: &str,
) -> Result<Option<String>> {
    Ok(resources::read_template_label(db, project, template_id)?
        .display_label
        .value)
}

const MAX_CHANGES: usize = 8;
const MAX_REQUESTS_PER_RUN: i64 = 32;
const PAGE_SIZE: i64 = 20;
const TEMPLATE_PAGE: i64 = 6;

impl Store {
    pub async fn settings_patch_question_timeout(
        &self,
        project: String,
    ) -> Result<RevisionedSetting<u32>> {
        self.execute(move |db| read_question_timeout_setting(db, &project))
            .await
    }

    pub async fn edit_project_question_timeout(
        &self,
        project: String,
        expected_revision: Option<i64>,
        minutes: u32,
    ) -> Result<RevisionedSetting<u32>> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let saved = write_question_timeout_setting(&tx, &project, expected_revision, minutes)?;
            tx.commit()?;
            Ok(saved)
        })
        .await
    }

    pub async fn template_display_label(
        &self,
        project: String,
        template_id: String,
    ) -> Result<TemplateDisplayLabel> {
        self.execute(move |db| resources::read_template_label(db, &project, &template_id))
            .await
    }

    pub async fn set_template_display_label(
        &self,
        project: String,
        template_id: String,
        template_revision: String,
        expected_revision: i64,
        label: Option<String>,
    ) -> Result<TemplateDisplayLabel> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = resources::write_template_label(
                &tx,
                &project,
                &template_id,
                &template_revision,
                expected_revision,
                label,
            )?;
            tx.commit()?;
            Ok(changed)
        })
        .await
    }

    pub async fn settings_patch_allowlist(
        &self,
        project: String,
    ) -> Result<SettingsPatchAllowlist> {
        self.execute(move |db| resources::read_allowlist(db, &project))
            .await
    }

    pub async fn set_settings_patch_allowlist(
        &self,
        project: String,
        expected_revision: i64,
        grants: BTreeSet<SettingsAutoGrant>,
    ) -> Result<SettingsPatchAllowlist> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let saved = resources::write_allowlist(&tx, &project, expected_revision, grants)?;
            tx.commit()?;
            Ok(saved)
        })
        .await
    }

    pub async fn settings_proposals(
        &self,
        project: String,
        offset: i64,
    ) -> Result<Vec<SettingsProposal>> {
        ensure!(
            (0..=16_384).contains(&offset),
            "The proposal page is outside the supported range"
        );
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT payload FROM workflow_settings_proposals WHERE project_id = ?1
                 ORDER BY rowid DESC LIMIT ?2 OFFSET ?3",
            )?;
            let rows = query.query_map(params![project, PAGE_SIZE, offset], |row| {
                row.get::<_, Vec<u8>>(0)
            })?;
            rows.map(|row| decode(&row?)).collect()
        })
        .await
    }

    pub(crate) async fn propose_settings_patch(
        &self,
        scope: RunScope,
        patch: SettingsPatch,
    ) -> Result<SettingsProposal> {
        validate_patch_shape(&patch)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let actor = origin(&tx, &scope)?;
            let proposed = propose(&tx, actor, patch)?;
            tx.commit()?;
            Ok(proposed)
        })
        .await
    }

    pub(crate) async fn read_editable_settings(
        &self,
        scope: RunScope,
        query: SettingsPatchCatalogQuery,
    ) -> Result<SettingsPatchCatalog> {
        ensure!(
            (0..=16_384).contains(&query.template_offset),
            "The template catalog page is outside the supported range"
        );
        self.execute(move |db| {
            let tx = db.transaction()?;
            let actor = origin(&tx, &scope)?;
            let catalog = read_catalog(&tx, &actor.project_id, query.template_offset)?;
            tx.commit()?;
            Ok(catalog)
        })
        .await
    }

    pub async fn review_settings_proposal(
        &self,
        project: String,
        proposal_id: String,
        expected_review_revision: String,
        accept: bool,
    ) -> Result<SettingsProposal> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut proposal = load_proposal(&tx, &project, &proposal_id)?;
            ensure!(
                proposal.review_revision == expected_review_revision,
                "The settings proposal differs from the reviewed revision"
            );
            match (&proposal.decision, accept) {
                (SettingsProposalDecision::Applied { .. }, true)
                | (SettingsProposalDecision::Rejected { .. }, false) => return Ok(proposal),
                (SettingsProposalDecision::Pending, _) => {}
                _ => anyhow::bail!("The settings proposal was already decided"),
            }
            let allowlist = resources::read_allowlist(&tx, &project)?;
            proposal.decision = if accept {
                apply(&tx, &proposal, SettingsDecisionSource::DesktopUser, allowlist.revision)?
            } else {
                SettingsProposalDecision::Rejected {
                    source: SettingsDecisionSource::DesktopUser,
                    allowlist_revision: allowlist.revision,
                    at: timestamp(&tx)?,
                }
            };
            tx.execute(
                "UPDATE workflow_settings_proposals SET payload = ?3 WHERE project_id = ?1 AND id = ?2",
                params![project, proposal_id, encode(&proposal)?],
            )?;
            tx.commit()?;
            Ok(proposal)
        }).await
    }
}

fn read_catalog(
    db: &Connection,
    project: &str,
    template_offset: i64,
) -> Result<SettingsPatchCatalog> {
    let mut query = db.prepare(
        "SELECT id FROM conversation_templates WHERE project_id = ?1
         ORDER BY id LIMIT ?2 OFFSET ?3",
    )?;
    let ids = query
        .query_map(
            params![project, TEMPLATE_PAGE + 1, template_offset],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next_template_offset =
        (i64::try_from(ids.len())? > TEMPLATE_PAGE).then_some(template_offset + TEMPLATE_PAGE);
    let templates = ids
        .into_iter()
        .take(usize::try_from(TEMPLATE_PAGE)?)
        .map(|id| {
            let label = resources::read_template_label(db, project, &id)?;
            let template =
                super::role_templates::load_revision(db, project, &id, &label.template_revision)?;
            let name = crate::diagnostics::preview(&template.template.name)
                .chars()
                .take(80)
                .collect();
            Ok(SettingsPatchTemplate { name, label })
        })
        .collect::<Result<_>>()?;
    Ok(SettingsPatchCatalog {
        live_question_timeout: read_question_timeout_setting(db, project)?,
        allowlist_revision: resources::read_allowlist(db, project)?.revision,
        templates,
        next_template_offset,
    })
}

fn origin(db: &Connection, scope: &RunScope) -> Result<SettingsPatchActor> {
    ensure!(
        read_active_scope(db, &scope.run_id)?.as_ref() == Some(scope),
        "Settings proposals require the active originating run"
    );
    let run = load_run(db, &scope.run_id)?;
    let internal_job: bool = db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM job_attempts attempt WHERE attempt.run_id = ?1 AND
             (EXISTS(SELECT 1 FROM conversation_summary_requests WHERE job_id = attempt.job_id)
              OR EXISTS(SELECT 1 FROM workflow_authoring_requests WHERE job_id = attempt.job_id)
              OR EXISTS(SELECT 1 FROM workflow_package_requests WHERE job_id = attempt.job_id)
              OR EXISTS(SELECT 1 FROM deleted_workflow_authoring_jobs WHERE job_id = attempt.job_id)
              OR EXISTS(SELECT 1 FROM job_side_requests WHERE job_id = attempt.job_id)
              OR EXISTS(SELECT 1 FROM model_comparison_candidates WHERE job_id = attempt.job_id))
         )",
        [&run.id],
        |row| row.get(0),
    )?;
    ensure!(
        !internal_job,
        "This internal job does not have settings authority"
    );
    let dispatch_id = db
        .query_row(
            "SELECT dispatch.id FROM dispatches dispatch
         JOIN context_snapshots snapshot ON snapshot.id = dispatch.snapshot_id
         WHERE snapshot.run_id = ?1 AND dispatch.turn_id = ?2
           AND dispatch.ordinal = 1 AND dispatch.delivery = 'submitted'
           AND NOT EXISTS(SELECT 1 FROM job_attempts WHERE run_id = ?1 AND cancel_requested)",
            params![run.id, run.turn_id],
            |row| row.get(0),
        )
        .optional()?
        .context("Settings proposals require an exact submitted dispatch")?;
    Ok(SettingsPatchActor {
        run_id: run.id,
        project_id: run.project_id,
        checkout_id: run.checkout_id,
        turn_id: run.turn_id,
        dispatch_id,
    })
}

fn propose(
    db: &Transaction<'_>,
    actor: SettingsPatchActor,
    patch: SettingsPatch,
) -> Result<SettingsProposal> {
    let fingerprint = digest(&serde_json::to_vec(&patch)?);
    let previous = db.query_row(
        "SELECT fingerprint, payload FROM workflow_settings_proposals WHERE origin_run_id = ?1 AND request_id = ?2",
        params![actor.run_id, patch.request_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    ).optional()?;
    if let Some((recorded, payload)) = previous {
        ensure!(
            recorded == fingerprint,
            "This run has already used the request ID for different changes"
        );
        return decode(&payload);
    }
    let prior: i64 = db.query_row(
        "SELECT count(*) FROM workflow_settings_proposals WHERE origin_run_id = ?1",
        [&actor.run_id],
        |row| row.get(0),
    )?;
    ensure!(
        prior < MAX_REQUESTS_PER_RUN,
        "This run reached its settings proposal limit"
    );
    let diffs = patch
        .changes
        .into_iter()
        .map(|change| resources::review_diff(db, &actor.project_id, change))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    ensure!(
        !diffs.is_empty(),
        "The requested setting values already match"
    );
    let allowlist = resources::read_allowlist(db, &actor.project_id)?;
    let id = Uuid::new_v4().to_string();
    let review_revision = digest(&serde_json::to_vec(&(
        &id,
        &actor,
        allowlist.revision,
        &diffs,
    ))?);
    let mut proposal = SettingsProposal {
        id,
        actor,
        request_id: patch.request_id,
        review_revision,
        proposed_at: timestamp(db)?,
        allowlist_revision: allowlist.revision,
        diffs,
        decision: SettingsProposalDecision::Pending,
    };
    if proposal
        .diffs
        .iter()
        .all(|diff| resources::permits(&allowlist, &proposal.actor.checkout_id, diff))
    {
        proposal.decision = apply(
            db,
            &proposal,
            SettingsDecisionSource::UserAllowlist,
            allowlist.revision,
        )?;
    }
    db.execute(
        "INSERT INTO workflow_settings_proposals(id, project_id, origin_run_id, dispatch_id, request_id, fingerprint, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![proposal.id, proposal.actor.project_id, proposal.actor.run_id, proposal.actor.dispatch_id,
            proposal.request_id, fingerprint, encode(&proposal)?],
    )?;
    Ok(proposal)
}

fn load_proposal(db: &Connection, project: &str, id: &str) -> Result<SettingsProposal> {
    let payload: Vec<u8> = db
        .query_row(
            "SELECT payload FROM workflow_settings_proposals WHERE project_id = ?1 AND id = ?2",
            params![project, id],
            |row| row.get(0),
        )
        .optional()?
        .context("The settings proposal is outside this project or is unavailable")?;
    decode(&payload)
}

fn apply(
    db: &Transaction<'_>,
    proposal: &SettingsProposal,
    source: SettingsDecisionSource,
    allowlist_revision: i64,
) -> Result<SettingsProposalDecision> {
    let revisions = proposal
        .diffs
        .iter()
        .map(|diff| resources::apply_diff(db, &proposal.actor.project_id, diff))
        .collect::<Result<Vec<_>>>()?;
    Ok(SettingsProposalDecision::Applied {
        source,
        allowlist_revision,
        at: timestamp(db)?,
        revisions,
    })
}

fn validate_patch_shape(patch: &SettingsPatch) -> Result<()> {
    valid_id(&patch.request_id)?;
    ensure!(
        (1..=MAX_CHANGES).contains(&patch.changes.len()),
        "Send one to {MAX_CHANGES} eligible field changes"
    );
    let mut fields = BTreeSet::new();
    for change in &patch.changes {
        let (field, target, expected) = match change {
            SettingsChange::LiveQuestionTimeout {
                expected_revision, ..
            } => (SettingsField::LiveQuestionTimeout, None, expected_revision),
            SettingsChange::TemplateDisplayLabel {
                template_id,
                template_revision,
                expected_revision,
                ..
            } => {
                valid_id(template_id)?;
                valid_id(template_revision)?;
                (
                    SettingsField::TemplateDisplayLabel,
                    Some(template_id),
                    expected_revision,
                )
            }
        };
        ensure!(
            *expected >= 0,
            "Expected setting revisions must be nonnegative"
        );
        ensure!(
            fields.insert((field, target)),
            "A patch must identify each field once"
        );
    }
    Ok(())
}

fn valid_id(id: &str) -> Result<()> {
    ensure!(
        !id.trim().is_empty() && id.len() <= 160 && !id.chars().any(char::is_control),
        "Expected a bounded nonempty identifier"
    );
    Ok(())
}

fn timestamp(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT unixepoch()", [], |row| row.get(0))?)
}

fn next_revision(current: i64) -> Result<i64> {
    current
        .checked_add(1)
        .context("The setting reached its revision limit")
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}
