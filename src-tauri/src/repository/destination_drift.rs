mod git;

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use self::git::{DestinationRepository, MergePreview};
use super::{
    verification::{
        CandidateGateAction, CandidateGateOutcome, CandidateVerification, command::run_in_checkout,
        gate_results_for_required_checks, load_scoped_candidate_acceptance,
    },
    verified_git::{VerifiedCommit, load_verified_delivery_commit},
};
use crate::{protection, store::Store, workflows::load_delivery_units};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationDriftPolicy {
    #[default]
    Pause,
    PrepareAndRecheck,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationDriftPreview {
    pub id: String,
    pub source_sha: String,
    pub destination_branch: String,
    pub expected_destination_sha: String,
    pub destination_sha: String,
    pub tree: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationDriftPatch {
    pub patch_base64: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DestinationDriftOutcome {
    Paused {
        expected_destination_sha: String,
        actual_sha: String,
    },
    Preparing {
        preview_id: String,
    },
    Conflicted {
        preview_id: String,
        destination_sha: String,
        paths: Vec<String>,
    },
    ChecksChanged {
        preview_id: String,
    },
    ChecksFailed {
        preview: DestinationDriftPreview,
        verification: CandidateVerification,
    },
    RequiresAcceptance {
        preview: DestinationDriftPreview,
        verification: CandidateVerification,
    },
    Accepted {
        preview: DestinationDriftPreview,
        verification: CandidateVerification,
        acceptance_id: String,
    },
    DestinationMoved {
        expected_destination_sha: String,
        actual_sha: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DestinationPin {
    workflow_run_id: String,
    source_commit_id: String,
    source_sha: String,
    destination_branch: String,
    expected_destination_sha: String,
    destination_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Review {
    id: String,
    pin: DestinationPin,
    #[serde(default)]
    user_requested: bool,
    state: Option<ReviewState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ReviewState {
    Conflicted(Vec<String>),
    Checked {
        preview: DestinationDriftPreview,
        verification: CandidateVerification,
        acceptance: Option<ReviewAcceptance>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewAcceptance {
    id: String,
    accepted_at: String,
}

impl Review {
    fn outcome(self) -> DestinationDriftOutcome {
        match self.state {
            None => DestinationDriftOutcome::Preparing {
                preview_id: self.id,
            },
            Some(ReviewState::Conflicted(paths)) => DestinationDriftOutcome::Conflicted {
                preview_id: self.id,
                destination_sha: self.pin.destination_sha,
                paths,
            },
            Some(ReviewState::Checked {
                preview,
                verification,
                acceptance,
            }) => match acceptance {
                Some(acceptance) => DestinationDriftOutcome::Accepted {
                    preview,
                    verification,
                    acceptance_id: acceptance.id,
                },
                None if verification.allows_acceptance() => {
                    DestinationDriftOutcome::RequiresAcceptance {
                        preview,
                        verification,
                    }
                }
                None => DestinationDriftOutcome::ChecksFailed {
                    preview,
                    verification,
                },
            },
        }
    }

    fn permitted(&self, policy: DestinationDriftPolicy) -> bool {
        self.user_requested || policy == DestinationDriftPolicy::PrepareAndRecheck
    }

    fn checks_are_current(&self, revision: &str) -> bool {
        match &self.state {
            Some(ReviewState::Checked { verification, .. }) => {
                verification.policy_revision == revision
            }
            _ => true,
        }
    }
}

impl Store {
    pub async fn delivery_destination_drift_policy(
        &self,
        project: String,
    ) -> Result<DestinationDriftPolicy> {
        self.execute(move |db| load_policy(db, &project)).await
    }

    pub async fn set_delivery_destination_drift_policy(
        &self,
        project: String,
        policy: DestinationDriftPolicy,
    ) -> Result<DestinationDriftPolicy> {
        self.execute(move |db| {
            let mode = match policy {
                DestinationDriftPolicy::Pause => "pause",
                DestinationDriftPolicy::PrepareAndRecheck => "prepare_and_recheck",
            };
            db.execute(
                "INSERT INTO project_destination_drift_policies VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode",
                params![project, mode],
            )?;
            Ok(policy)
        })
        .await
    }

    /// Call after the merge engine has admitted the original delivery and observed ref movement.
    pub(crate) async fn prepare_delivery_destination_drift(
        &self,
        source: &VerifiedCommit,
        workflow_run_id: &str,
        destination_branch: &str,
        expected_destination_sha: &str,
        actual_sha: &str,
        user_requested: bool,
    ) -> Result<DestinationDriftOutcome> {
        let pin = DestinationPin {
            workflow_run_id: workflow_run_id.into(),
            source_commit_id: source.id.clone(),
            source_sha: source.sha.clone(),
            destination_branch: destination_branch.into(),
            expected_destination_sha: git::normalized_oid(expected_destination_sha)?,
            destination_sha: git::normalized_oid(actual_sha)?,
        };
        ensure!(
            pin.destination_sha != pin.expected_destination_sha,
            "the delivery destination has not moved"
        );
        let gate = preparation_gate(&source.id);
        let _serial = gate.lock().await;
        let (policy, required, saved) = self
            .execute({
                let source = source.clone();
                let pin = pin.clone();
                move |db| {
                    let required = require_source(db, &source, &pin)?;
                    let saved = find_review(db, &pin)?;
                    Ok((load_policy(db, &source.project_id)?, required, saved))
                }
            })
            .await?;
        if policy == DestinationDriftPolicy::Pause
            && !user_requested
            && !saved.as_ref().is_some_and(|review| review.user_requested)
        {
            return Ok(paused(&pin));
        }
        let repository = DestinationRepository::load(self, source, destination_branch).await?;
        if let Some(movement) = repository.movement(&pin.destination_sha).await? {
            return Ok(movement);
        }
        if let Some(review) = &saved {
            if review.pin.destination_sha != pin.destination_sha {
                return Ok(moved(
                    &review.pin.destination_sha,
                    Some(pin.destination_sha),
                ));
            }
        }

        let (mut review, mut verification) = self
            .execute({
                let source = source.clone();
                move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let (revision, gates) =
                        gate_results_for_required_checks(&tx, &source.project_id, required)?;
                    let review = match find_review(&tx, &pin)? {
                        Some(mut review) => {
                            ensure!(review.pin == pin, "destination changed during preparation");
                            if user_requested && !review.user_requested {
                                review.user_requested = true;
                                update_review(&tx, &review)?;
                            }
                            review
                        }
                        None => start_review(&tx, &source, pin, user_requested)?,
                    };
                    let verification = CandidateVerification {
                        id: Uuid::new_v4().to_string(),
                        candidate_id: review.id.clone(),
                        policy_revision: revision,
                        started_at: timestamp()?,
                        finished_at: None,
                        gates,
                    };
                    tx.commit()?;
                    Ok((review, verification))
                }
            })
            .await?;
        if review.state.is_some() && review.checks_are_current(&verification.policy_revision) {
            return Ok(review.outcome());
        }
        let merge_preview = repository.preview(&review.pin).await?;
        let state = match merge_preview {
            MergePreview::Conflicted(paths) => ReviewState::Conflicted(paths),
            MergePreview::Clean {
                tree,
                changed_paths,
            } => {
                let preview = DestinationDriftPreview {
                    id: review.id.clone(),
                    source_sha: review.pin.source_sha.clone(),
                    destination_branch: review.pin.destination_branch.clone(),
                    expected_destination_sha: review.pin.expected_destination_sha.clone(),
                    destination_sha: review.pin.destination_sha.clone(),
                    tree,
                    changed_paths,
                };
                for gate in &mut verification.gates {
                    if let CandidateGateAction::Unavailable { reason } = &gate.gate.action {
                        gate.outcome = Some(CandidateGateOutcome::CouldNotRun {
                            reason: reason.clone(),
                        });
                        continue;
                    }
                    if let Some(movement) = repository.movement(&preview.destination_sha).await? {
                        return Ok(movement);
                    }
                    gate.outcome = Some(
                        tokio::task::spawn_blocking({
                            let repository = repository.clone();
                            let pin = review.pin.clone();
                            let gate = gate.clone();
                            let tree = preview.tree.clone();
                            move || {
                                match repository.materialize(&pin, &tree) {
                                    Ok(checkout) => run_in_checkout(
                                        checkout.path(),
                                        &gate.gate.action,
                                        gate.executable_revision.as_deref(),
                                    ),
                                    Err(error) => CandidateGateOutcome::CouldNotRun {
                                        reason: crate::diagnostics::preview(&format!(
                                            "Could not materialize the exact destination update: {error:#}"
                                        )),
                                    },
                                }
                            }
                        })
                        .await?,
                    );
                }
                verification.finished_at = Some(timestamp()?);
                ReviewState::Checked {
                    preview,
                    verification,
                    acceptance: None,
                }
            }
        };
        if let Some(movement) = repository.movement(&review.pin.destination_sha).await? {
            return Ok(movement);
        }
        review.state = Some(state);
        self.execute({
            let source = source.clone();
            move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let required = require_source(&tx, &source, &review.pin)?;
                let current = find_review(&tx, &review.pin)?
                    .context("the destination update is no longer available")?;
                ensure!(
                    current.pin == review.pin,
                    "destination update pins have changed"
                );
                review.user_requested |= current.user_requested;
                if !review.permitted(load_policy(&tx, &source.project_id)?) {
                    return Ok(paused(&review.pin));
                }
                let (revision, _) =
                    gate_results_for_required_checks(&tx, &source.project_id, required)?;
                if current.state.is_some() && current.checks_are_current(&revision) {
                    return Ok(current.outcome());
                }
                if !review.checks_are_current(&revision) {
                    return Ok(DestinationDriftOutcome::ChecksChanged {
                        preview_id: review.id,
                    });
                }
                update_review(&tx, &review)?;
                tx.commit()?;
                Ok(review.outcome())
            }
        })
        .await
    }

    /// Open the exact update against its pinned destination even after the live ref advances.
    pub async fn delivery_destination_drift_patch(
        &self,
        project: String,
        task: String,
        workflow_run: String,
        preview_id: String,
    ) -> Result<DestinationDriftPatch> {
        let (source, review) = self
            .execute(move |db| {
                let (source, review) =
                    load_scoped_review(db, &project, &task, &workflow_run, &preview_id)?;
                require_source(db, &source, &review.pin)?;
                Ok((source, review))
            })
            .await?;
        let Some(ReviewState::Checked { preview, .. }) = review.state else {
            anyhow::bail!("the destination update does not have a reviewable tree");
        };
        let repository =
            DestinationRepository::load(self, &source, &review.pin.destination_branch).await?;
        let (patch, truncated) = repository.patch(review.pin, preview.tree).await?;
        Ok(DestinationDriftPatch {
            patch_base64: STANDARD.encode(patch),
            truncated,
        })
    }

    pub async fn accept_delivery_destination_drift(
        &self,
        project: String,
        task: String,
        workflow_run: String,
        preview_id: String,
        verification_id: String,
    ) -> Result<DestinationDriftOutcome> {
        let (source, review) = self
            .execute(move |db| load_scoped_review(db, &project, &task, &workflow_run, &preview_id))
            .await?;
        let gate = preparation_gate(&source.id);
        let _serial = gate.lock().await;
        let repository =
            DestinationRepository::load(self, &source, &review.pin.destination_branch).await?;
        if let Some(movement) = repository.movement(&review.pin.destination_sha).await? {
            return Ok(movement);
        }
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (_, mut review) = load_scoped_review(
                &tx,
                &source.project_id,
                &source.task_id,
                &review.pin.workflow_run_id,
                &review.id,
            )?;
            let required = require_source(&tx, &source, &review.pin)?;
            if !review.permitted(load_policy(&tx, &source.project_id)?) {
                return Ok(paused(&review.pin));
            }
            let (revision, _) =
                gate_results_for_required_checks(&tx, &source.project_id, required)?;
            if !review.checks_are_current(&revision) {
                return Ok(DestinationDriftOutcome::ChecksChanged {
                    preview_id: review.id,
                });
            }
            let Some(ReviewState::Checked {
                verification,
                acceptance,
                ..
            }) = &mut review.state
            else {
                anyhow::bail!("the destination update does not have new check evidence");
            };
            ensure!(
                verification.id == verification_id && verification.allows_acceptance(),
                "renewed acceptance requires the exact preview and recently passed required checks"
            );
            if acceptance.is_none() {
                *acceptance = Some(ReviewAcceptance {
                    id: Uuid::new_v4().to_string(),
                    accepted_at: timestamp()?,
                });
            }
            update_review(&tx, &review)?;
            tx.commit()?;
            Ok(review.outcome())
        })
        .await
    }
}

/// Resolve acceptance inside the merge engine's publication transaction. The engine must still
/// CAS the returned destination SHA and match the result's tree to the returned exact tree.
pub(crate) fn authorized_destination_drift(
    db: &Connection,
    source: &VerifiedCommit,
    workflow_run_id: &str,
    destination_branch: &str,
    expected_destination_sha: &str,
    live_sha: &str,
    acceptance_id: &str,
) -> Result<DestinationDriftPreview> {
    let pin = DestinationPin {
        workflow_run_id: workflow_run_id.into(),
        source_commit_id: source.id.clone(),
        source_sha: source.sha.clone(),
        destination_branch: destination_branch.into(),
        expected_destination_sha: git::normalized_oid(expected_destination_sha)?,
        destination_sha: git::normalized_oid(live_sha)?,
    };
    let required = require_source(db, source, &pin)?;
    let review = find_review(db, &pin)?.context("destination update has not been prepared")?;
    ensure!(
        review.permitted(load_policy(db, &source.project_id)?),
        "destination updates now pause for user action"
    );
    ensure!(
        review.pin == pin,
        "the destination moved again after preparation"
    );
    let (revision, _) = gate_results_for_required_checks(db, &source.project_id, required)?;
    ensure!(
        review.checks_are_current(&revision),
        "required check policy has changed"
    );
    let Some(ReviewState::Checked {
        preview,
        verification,
        acceptance,
    }) = review.state
    else {
        anyhow::bail!("destination update has no valid check evidence");
    };
    ensure!(
        acceptance
            .as_ref()
            .is_some_and(|accepted| accepted.id == acceptance_id)
            && verification.allows_acceptance(),
        "the current destination update requires its own acceptance"
    );
    Ok(preview)
}

fn paused(pin: &DestinationPin) -> DestinationDriftOutcome {
    DestinationDriftOutcome::Paused {
        expected_destination_sha: pin.expected_destination_sha.clone(),
        actual_sha: pin.destination_sha.clone(),
    }
}

fn moved(expected: &str, actual: Option<String>) -> DestinationDriftOutcome {
    DestinationDriftOutcome::DestinationMoved {
        expected_destination_sha: expected.into(),
        actual_sha: actual,
    }
}

fn load_policy(db: &Connection, project: &str) -> Result<DestinationDriftPolicy> {
    let mode: Option<String> = db
        .query_row(
            "SELECT d.mode FROM projects p LEFT JOIN project_destination_drift_policies d
         ON d.project_id = p.id WHERE p.id = ?1",
            [project],
            |row| row.get(0),
        )
        .context("project is unavailable")?;
    match mode.as_deref() {
        None | Some("pause") => Ok(DestinationDriftPolicy::Pause),
        Some("prepare_and_recheck") => Ok(DestinationDriftPolicy::PrepareAndRecheck),
        _ => anyhow::bail!("invalid destination drift policy"),
    }
}

fn require_source(
    db: &Connection,
    source: &VerifiedCommit,
    pin: &DestinationPin,
) -> Result<Vec<String>> {
    let persisted = load_verified_delivery_commit(
        db,
        &source.project_id,
        &source.task_id,
        &pin.source_commit_id,
    )?;
    ensure!(
        persisted.as_ref() == Some(source) && pin.source_sha == source.sha,
        "destination recovery requires the recorded verified source commit"
    );
    let units = load_delivery_units(
        db,
        &source.project_id,
        &source.task_id,
        &pin.workflow_run_id,
    )?;
    let unit = units
        .last()
        .context("the source no longer belongs to the active workflow delivery")?;
    ensure!(
        unit.unit.id == source.unit_id
            && unit.commit_evidence_id.as_deref() == Some(source.id.as_str())
            && unit.workflow_run_id == pin.workflow_run_id,
        "the destination update requires the final commit of the pinned workflow delivery"
    );
    ensure!(
        source.branch != pin.destination_branch,
        "the delivery cannot merge its source branch into itself"
    );
    let mut required = BTreeSet::new();
    for unit in units {
        let id = unit
            .commit_evidence_id
            .context("delivery unit has no verified commit")?;
        let commit = load_verified_delivery_commit(db, &source.project_id, &source.task_id, &id)?
            .context("verified unit evidence is unavailable")?;
        let acceptance = load_scoped_candidate_acceptance(
            db,
            &source.project_id,
            &source.task_id,
            &commit.acceptance_id,
        )?
        .context("source check evidence is unavailable")?;
        ensure!(
            acceptance.verification.id == commit.verification_id
                && acceptance.verification.allows_acceptance(),
            "a source commit lacks qualifying check evidence"
        );
        required.extend(
            acceptance
                .verification
                .gates
                .into_iter()
                .filter(|gate| gate.gate.required)
                .map(|gate| gate.gate.name),
        );
    }
    Ok(required.into_iter().collect())
}

fn find_review(db: &Connection, pin: &DestinationPin) -> Result<Option<Review>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_destination_drift_updates
         WHERE commit_evidence_id = ?1 AND workflow_run_id = ?2 AND destination_branch = ?3
           AND expected_destination_sha = ?4",
            params![
                pin.source_commit_id,
                pin.workflow_run_id,
                pin.destination_branch,
                pin.expected_destination_sha
            ],
            |row| row.get(0),
        )
        .optional()?;
    payload.as_deref().map(decode_review).transpose()
}

fn load_scoped_review(
    db: &Connection,
    project: &str,
    task: &str,
    workflow_run: &str,
    id: &str,
) -> Result<(VerifiedCommit, Review)> {
    let payload: Vec<u8> = db
        .query_row(
            "SELECT payload FROM workflow_destination_drift_updates
         WHERE project_id = ?1 AND task_id = ?2 AND workflow_run_id = ?3 AND id = ?4",
            params![project, task, workflow_run, id],
            |row| row.get(0),
        )
        .optional()?
        .context("destination update is outside the selected delivery")?;
    let review = decode_review(&payload)?;
    let source = load_verified_delivery_commit(db, project, task, &review.pin.source_commit_id)?
        .context("verified source evidence is unavailable")?;
    Ok((source, review))
}

fn start_review(
    db: &Connection,
    source: &VerifiedCommit,
    pin: DestinationPin,
    user_requested: bool,
) -> Result<Review> {
    let review = Review {
        id: Uuid::new_v4().to_string(),
        pin,
        user_requested,
        state: None,
    };
    db.execute(
        "INSERT INTO workflow_destination_drift_updates
         (id, project_id, task_id, workflow_run_id, commit_evidence_id,
          destination_branch, expected_destination_sha, observed_destination_sha, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            review.id,
            source.project_id,
            source.task_id,
            review.pin.workflow_run_id,
            source.id,
            review.pin.destination_branch,
            review.pin.expected_destination_sha,
            review.pin.destination_sha,
            encode_review(&review)?,
        ],
    )?;
    Ok(review)
}

fn update_review(db: &Connection, review: &Review) -> Result<()> {
    ensure!(
        db.execute(
            "UPDATE workflow_destination_drift_updates SET payload = ?2 WHERE id = ?1",
            params![review.id, encode_review(review)?],
        )? == 1,
        "the destination update is unavailable"
    );
    Ok(())
}

fn encode_review(review: &Review) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(review)?)?)
}

fn decode_review(payload: &[u8]) -> Result<Review> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}

fn timestamp() -> Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

type Preparations = Mutex<HashMap<String, Weak<AsyncMutex<()>>>>;
static PREPARATIONS: OnceLock<Preparations> = OnceLock::new();

fn preparation_gate(source: &str) -> Arc<AsyncMutex<()>> {
    let mut preparations = PREPARATIONS
        .get_or_init(Default::default)
        .lock()
        .expect("destination preparation lock");
    if let Some(gate) = preparations.get(source).and_then(Weak::upgrade) {
        return gate;
    }
    preparations.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(AsyncMutex::new(()));
    preparations.insert(source.into(), Arc::downgrade(&gate));
    gate
}
