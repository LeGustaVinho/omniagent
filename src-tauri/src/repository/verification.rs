pub(super) mod command;
pub mod ipc;
pub(super) mod materialize;

use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{dispatch::ContextSnapshot, protection, store::Store};

use super::{AttemptWorktreeEvidence, CandidateRevision};
pub use materialize::MaterializedCandidate;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateTarget {
    pub project_id: String,
    pub task_id: String,
    pub unit_id: String,
    pub run_id: String,
    pub capture_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSelection {
    /// None includes every captured tracked change. Some selects named changes only.
    pub tracked_paths: Option<Vec<String>>,
    pub untracked_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedCandidate {
    pub id: String,
    pub target: CandidateTarget,
    pub checkout_id: String,
    pub branch: String,
    pub base_commit: String,
    pub tree: String,
    pub selection: CandidateSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateGate {
    pub name: String,
    pub required: bool,
    pub action: CandidateGateAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateGatePolicy {
    pub revision: String,
    pub gates: Vec<CandidateGate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CandidateGateAction {
    Command {
        executable: PathBuf,
        arguments: Vec<String>,
        timeout_seconds: Option<u64>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PinnedGate {
    gate: CandidateGate,
    executable_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CandidateGateOutcome {
    Passed {
        stdout: String,
        stderr: String,
        truncated: bool,
    },
    Failed {
        exit_code: u32,
        stdout: String,
        stderr: String,
        truncated: bool,
    },
    CouldNotRun {
        reason: String,
    },
    Interrupted {
        reason: String,
        stdout: String,
        stderr: String,
        truncated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateGateResult {
    pub gate: CandidateGate,
    pub executable_revision: Option<String>,
    pub outcome: Option<CandidateGateOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateVerification {
    pub id: String,
    pub candidate_id: String,
    pub policy_revision: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub gates: Vec<CandidateGateResult>,
}

impl CandidateVerification {
    pub fn allows_acceptance(&self) -> bool {
        self.finished_at.is_some()
            && self.gates.iter().all(|gate| {
                !gate.gate.required
                    || matches!(gate.outcome, Some(CandidateGateOutcome::Passed { .. }))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateAcceptance {
    pub id: String,
    pub candidate: SelectedCandidate,
    pub verification: CandidateVerification,
    pub accepted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedDeliveryPatch {
    pub candidate_id: String,
    pub base_commit: String,
    pub tree: String,
    pub patch_base64: String,
}

impl Store {
    /// Inspect the exact verified selection in an isolated Git object store.
    pub async fn verified_delivery_patch(
        &self,
        project: String,
        task: String,
        evidence: String,
    ) -> Result<Option<VerifiedDeliveryPatch>> {
        let Some(commit) = self
            .verified_delivery_commit(project.clone(), task.clone(), evidence)
            .await?
        else {
            return Ok(None);
        };
        let accepted = self
            .candidate_acceptance(project.clone(), task.clone(), commit.acceptance_id.clone())
            .await?
            .context("the verified commit's candidate acceptance is unavailable")?;
        let selected = accepted.candidate;
        ensure!(
            selected.target.project_id == project
                && selected.target.task_id == task
                && selected.target.unit_id == commit.unit_id
                && selected.target.run_id == commit.capture_run_id
                && selected.id == commit.candidate_revision
                && selected.checkout_id == commit.checkout_id
                && selected.tree == commit.tree
                && selected.base_commit == commit.base_commit
                && accepted.verification.id == commit.verification_id,
            "the accepted selection does not match the verified commit"
        );
        let capture = self
            .execute({
                let target = selected.target.clone();
                move |db| load_capture(db, &target).map(|(capture, _)| capture)
            })
            .await?;
        let patch = tokio::task::spawn_blocking(move || {
            let bytes = materialize::saved_patch(&capture, &selected)?;
            Ok::<_, anyhow::Error>(VerifiedDeliveryPatch {
                candidate_id: selected.id,
                base_commit: selected.base_commit,
                tree: selected.tree,
                patch_base64: STANDARD.encode(bytes),
            })
        })
        .await??;
        Ok(Some(patch))
    }
    pub async fn candidate_gate_policy(&self, project: String) -> Result<CandidateGatePolicy> {
        self.execute(move |db| {
            ensure!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                    [&project],
                    |row| row.get::<_, bool>(0)
                )?,
                "project is not registered"
            );
            let (revision, gates) = load_policy(db, &project)?;
            Ok(CandidateGatePolicy {
                revision,
                gates: gates.into_iter().map(|pinned| pinned.gate).collect(),
            })
        })
        .await
    }

    /// Configure trusted build and test commands or record why an applicable gate is unavailable.
    pub async fn set_candidate_gate_policy(
        &self,
        project: String,
        gates: Vec<CandidateGate>,
    ) -> Result<String> {
        let policy = tokio::task::spawn_blocking(move || pin_policy(gates)).await??;
        let revision = fingerprint(&policy)?;
        let payload = encode(&policy)?;
        self.execute(move |db| {
            db.execute("INSERT INTO candidate_gate_policies(project_id, revision, payload) VALUES (?1, ?2, ?3)
                         ON CONFLICT(project_id) DO UPDATE SET revision=excluded.revision, payload=excluded.payload", params![project, revision, payload])?;
            Ok(revision)
        }).await
    }

    pub async fn select_worktree_candidate(
        &self,
        target: CandidateTarget,
        mut selection: CandidateSelection,
    ) -> Result<SelectedCandidate> {
        ensure!(
            !target.unit_id.trim().is_empty(),
            "candidate needs a logical unit identity"
        );
        selection.normalize()?;
        let (capture, checkout_id, prior) = self
            .execute({
                let target = target.clone();
                move |db| {
                    let (capture, checkout) = load_capture(db, &target)?;
                    let prior = active_candidate_id(db, &target)?;
                    Ok((capture, checkout, prior))
                }
            })
            .await?;
        let branch = capture.start.worktree.branch.clone();
        let base_commit = capture.start.starting.head.clone();
        let tree = tokio::task::spawn_blocking({
            let selection = selection.clone();
            move || materialize::selected_tree(&capture, &selection)
        })
        .await??;
        let mut candidate = SelectedCandidate {
            id: Uuid::new_v4().to_string(),
            target,
            checkout_id,
            branch,
            base_commit,
            tree,
            selection,
        };
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (_, checkout) = load_capture(&tx, &candidate.target)?;
            ensure!(checkout == candidate.checkout_id, "captured checkout changed during selection");
            let current = active_candidate_id(&tx, &candidate.target)?;
            if let Some(id) = &current {
                let original = load_selected_candidate(&tx, &candidate.target.project_id, &candidate.target.task_id, id)?;
                candidate.id.clone_from(id);
                if candidate == original {
                    return Ok(original);
                }
                candidate.id = Uuid::new_v4().to_string();
            }
            ensure!(current == prior, "a different candidate was selected for the unit during materialization");
            let target = &candidate.target;
            tx.execute("INSERT INTO selected_candidate_revisions(id, project_id, task_id, unit_id, capture_run_id, payload)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![candidate.id, target.project_id, target.task_id, target.unit_id, target.run_id, encode(&candidate)?])?;
            tx.execute("INSERT INTO active_unit_candidates(project_id, task_id, unit_id, candidate_id) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(project_id, task_id, unit_id) DO UPDATE SET candidate_id=excluded.candidate_id",
                params![target.project_id, target.task_id, target.unit_id, candidate.id])?;
            tx.commit()?;
            Ok(candidate)
        }).await
    }

    /// Recreate a saved candidate for inspection or review. Historical selections remain readable.
    pub async fn materialize_selected_candidate(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<MaterializedCandidate> {
        let (candidate, capture) = self
            .execute(move |db| {
                let candidate = load_selected_candidate(db, &project, &task, &id)?;
                let (capture, _) = load_capture(db, &candidate.target)?;
                Ok((candidate, capture))
            })
            .await?;
        tokio::task::spawn_blocking(move || materialize::materialize(&capture, &candidate)).await?
    }

    pub async fn selected_candidate(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<Option<SelectedCandidate>> {
        self.execute(move |db| {
            let payload: Option<Vec<u8>> = db.query_row("SELECT payload FROM selected_candidate_revisions
                WHERE project_id = ?1 AND task_id = ?2 AND id = ?3
                  AND NOT EXISTS (SELECT 1 FROM history_deleted_candidate_diffs WHERE candidate_id = ?3)",
                params![project, task, id], |row| row.get(0)).optional()?;
            payload.as_deref().map(decode).transpose()
        }).await
    }

    pub async fn current_selected_candidate(
        &self,
        project: String,
        task: String,
        unit: String,
    ) -> Result<Option<SelectedCandidate>> {
        self.execute(move |db| {
            let id: Option<String> = db.query_row(
                "SELECT candidate_id FROM active_unit_candidates WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
                params![project, task, unit], |row| row.get(0),
            ).optional()?;
            id.as_deref().map(|id| load_selected_candidate(db, &project, &task, id)).transpose()
        }).await
    }

    /// Pin the gate policy before running any command and persist each gate result separately.
    pub async fn verify_selected_candidate(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<CandidateVerification> {
        self.verify_selected_candidate_with_requirements(project, task, id, Vec::new())
            .await
    }

    pub async fn verify_selected_candidate_with_requirements(
        &self,
        project: String,
        task: String,
        id: String,
        required: Vec<String>,
    ) -> Result<CandidateVerification> {
        let (candidate, capture, verification, gates) = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate = load_selected_candidate(&tx, &project, &task, &id)?;
            require_current(&tx, &candidate)?;
            let (capture, _) = load_capture(&tx, &candidate.target)?;
            let (policy_revision, gates) = policy_with_requirements(&tx, &project, required)?;
            let verification = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO candidate_verifications(id, candidate_id, policy_revision, gates) VALUES (?1, ?2, ?3, ?4)",
                params![verification, id, policy_revision, encode(&gates)?])?;
            tx.commit()?;
            Ok((candidate, capture, verification, gates))
        }).await?;
        let store = self.clone();
        tokio::spawn(async move {
            for (index, gate) in gates.into_iter().enumerate() {
                let outcome = match &gate.gate.action {
                    CandidateGateAction::Unavailable { reason } => CandidateGateOutcome::CouldNotRun { reason: reason.clone() },
                    CandidateGateAction::Command { .. } => tokio::task::spawn_blocking({
                        let capture = capture.clone();
                        let candidate = candidate.clone();
                        move || command::run(&capture, &candidate, &gate)
                    }).await?,
                };
                let id = verification.clone();
                store.execute(move |db| {
                    db.execute("INSERT INTO candidate_gate_results(verification_id, gate_index, payload) VALUES (?1, ?2, ?3)", params![id, index as i64, encode(&outcome)?])?;
                    Ok(())
                }).await?;
            }
            store.execute(move |db| {
                db.execute("UPDATE candidate_verifications SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1", [&verification])?;
                load_verification(db, &verification)
            }).await
        }).await?
    }

    /// Historical evidence for the inspector, including failed and in-progress checks.
    pub async fn candidate_verification(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<Option<CandidateVerification>> {
        self.execute(move |db| load_scoped_candidate_verification(db, &project, &task, &id))
            .await
    }

    pub async fn current_candidate_verification(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<CandidateVerification> {
        self.execute(move |db| load_current_candidate_verification(db, &project, &task, &id))
            .await
    }

    /// Explicitly accept the reviewed selection. Only the most recent current verification can qualify.
    pub async fn accept_selected_candidate(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<CandidateAcceptance> {
        self.record_candidate_acceptance(project, task, id, None)
            .await
    }

    pub async fn accept_candidate_verification(
        &self,
        project: String,
        task: String,
        id: String,
        verification_id: String,
    ) -> Result<CandidateAcceptance> {
        self.record_candidate_acceptance(project, task, id, Some(verification_id))
            .await
    }

    async fn record_candidate_acceptance(
        &self,
        project: String,
        task: String,
        id: String,
        verification_id: Option<String>,
    ) -> Result<CandidateAcceptance> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let candidate = load_selected_candidate(&tx, &project, &task, &id)?;
            require_current(&tx, &candidate)?;
            let verification = current_verification(&tx, &candidate)?;
            ensure!(verification_id.as_ref().is_none_or(|id| id == &verification.id), "verification was superseded");
            ensure!(verification.allows_acceptance(), "required checks are still pending, failed, or unavailable");
            let accepted = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO candidate_acceptances(id, candidate_id, verification_id) VALUES(?1, ?2, ?3)
                         ON CONFLICT(candidate_id, verification_id) DO NOTHING", params![accepted, id, verification.id])?;
            let (id, accepted_at) = tx.query_row("SELECT id, accepted_at FROM candidate_acceptances WHERE candidate_id = ?1 AND verification_id = ?2",
                params![id, verification.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            tx.commit()?;
            Ok(CandidateAcceptance { id, candidate, verification, accepted_at })
        }).await
    }

    pub async fn candidate_acceptance(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<Option<CandidateAcceptance>> {
        self.execute(move |db| load_scoped_candidate_acceptance(db, &project, &task, &id))
            .await
    }

    pub async fn current_candidate_acceptance(
        &self,
        project: String,
        task: String,
        id: String,
    ) -> Result<CandidateAcceptance> {
        self.execute(move |db| load_current_candidate_acceptance(db, &project, &task, &id))
            .await
    }
}

fn load_capture(db: &Connection, target: &CandidateTarget) -> Result<(CandidateRevision, String)> {
    let (payload, checkout, common): (Vec<u8>, String, String) = db
        .query_row(
            "SELECT c.evidence, j.checkout_id, p.git_common_dir FROM job_worktree_candidates c
         JOIN job_attempts a ON a.run_id = c.run_id JOIN jobs j ON j.id = a.job_id
         JOIN projects p ON p.id = j.project_id
         LEFT JOIN job_worktree_scopes s ON s.job_id = j.id
         WHERE c.run_id = ?1 AND j.project_id = ?2 AND coalesce(s.scope_task_id, j.task_id) = ?3
           AND c.capture_confirmed = 1 AND j.mode = 'approved_write'",
            params![target.run_id, target.project_id, target.task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .context("confirmed capture is outside the requested work scope")?;
    let AttemptWorktreeEvidence::Saved { artifact } = decode(&payload)? else {
        anyhow::bail!("captured candidate is unavailable");
    };
    ensure!(
        artifact.revision == target.capture_revision
            && artifact.start.worktree.checkout_id == checkout
            && artifact.start.worktree.project_id == target.project_id
            && artifact.start.git_common_dir == PathBuf::from(common),
        "capture revision or registered repository does not match this selection"
    );
    Ok((*artifact, checkout))
}

fn active_candidate_id(db: &Connection, target: &CandidateTarget) -> Result<Option<String>> {
    Ok(db.query_row("SELECT candidate_id FROM active_unit_candidates WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
        params![target.project_id, target.task_id, target.unit_id], |row| row.get(0)).optional()?)
}

pub(crate) fn load_selected_candidate(
    db: &Connection,
    project: &str,
    task: &str,
    id: &str,
) -> Result<SelectedCandidate> {
    let payload: Vec<u8> = db.query_row("SELECT payload FROM selected_candidate_revisions
        WHERE project_id = ?1 AND task_id = ?2 AND id = ?3
          AND NOT EXISTS (SELECT 1 FROM history_deleted_candidate_diffs WHERE candidate_id = ?3)",
        params![project, task, id], |row| row.get(0)).optional()?.context("selected candidate is unavailable in the requested work scope")?;
    decode(&payload)
}

pub(crate) fn require_current(db: &Connection, candidate: &SelectedCandidate) -> Result<()> {
    ensure!(
        active_candidate_id(db, &candidate.target)?.as_deref() == Some(&candidate.id),
        "candidate selection has been superseded"
    );
    Ok(())
}

pub(crate) fn require_candidate_review_dispatch(
    db: &Connection,
    review_run: &str,
    candidate: &SelectedCandidate,
) -> Result<()> {
    let payload: Vec<u8> = db
        .query_row(
            "SELECT s.payload FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
         WHERE s.run_id = ?1 AND d.ordinal = 1 AND d.delivery = 'submitted'",
            [review_run],
            |row| row.get(0),
        )
        .optional()?
        .context("review is missing its submitted candidate checkout")?;
    let review = decode::<ContextSnapshot>(&payload)?
        .scoped_launch
        .and_then(|launch| launch.candidate_checkout)
        .context("review is missing its materialized candidate checkout")?;
    ensure!(
        review.candidate_id == candidate.id
            && review.source_run_id == candidate.target.run_id
            && review.base_commit == candidate.base_commit
            && review.tree == candidate.tree,
        "review used a different candidate checkout"
    );
    Ok(())
}

pub(crate) fn load_current_candidate_verification(
    db: &Connection,
    project: &str,
    task: &str,
    id: &str,
) -> Result<CandidateVerification> {
    let candidate = load_selected_candidate(db, project, task, id)?;
    require_current(db, &candidate)?;
    current_verification(db, &candidate)
}

pub(super) fn gate_results_for_required_checks(
    db: &Connection,
    project: &str,
    required: Vec<String>,
) -> Result<(String, Vec<CandidateGateResult>)> {
    let (revision, gates) = policy_with_requirements(db, project, required)?;
    Ok((
        revision,
        gates
            .into_iter()
            .map(|pinned| CandidateGateResult {
                gate: pinned.gate,
                executable_revision: pinned.executable_revision,
                outcome: None,
            })
            .collect(),
    ))
}

fn policy_with_requirements(
    db: &Connection,
    project: &str,
    required: Vec<String>,
) -> Result<(String, Vec<PinnedGate>)> {
    let (revision, mut gates) = load_policy(db, project)?;
    for name in required {
        ensure!(
            !name.trim().is_empty(),
            "required check name must be non-empty"
        );
        if let Some(pinned) = gates.iter_mut().find(|pinned| pinned.gate.name == name) {
            pinned.gate.required = true;
        } else {
            gates.push(PinnedGate {
                gate: CandidateGate {
                    action: CandidateGateAction::Unavailable {
                        reason: format!("No command is configured for required check {name}"),
                    },
                    required: true,
                    name,
                },
                executable_revision: None,
            });
        }
    }
    Ok((revision, gates))
}

fn load_policy(db: &Connection, project: &str) -> Result<(String, Vec<PinnedGate>)> {
    let row: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT revision, payload FROM candidate_gate_policies WHERE project_id = ?1",
            [project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((revision, payload)) = row {
        return Ok((revision, decode(&payload)?));
    }
    let gates = ["build", "tests"]
        .into_iter()
        .map(|name| PinnedGate {
            gate: CandidateGate {
                name: name.into(),
                required: false,
                action: CandidateGateAction::Unavailable {
                    reason: format!("No {name} command is configured for the project"),
                },
            },
            executable_revision: None,
        })
        .collect::<Vec<_>>();
    Ok((fingerprint(&gates)?, gates))
}

fn current_verification(
    db: &Connection,
    candidate: &SelectedCandidate,
) -> Result<CandidateVerification> {
    let (policy, _) = load_policy(db, &candidate.target.project_id)?;
    let id: String = db.query_row("SELECT id FROM candidate_verifications WHERE candidate_id = ?1 AND policy_revision = ?2 ORDER BY sequence DESC LIMIT 1",
        params![candidate.id, policy], |row| row.get(0)).optional()?.context("verify the candidate under the current project gate policy")?;
    load_verification(db, &id)
}

fn load_verification(db: &Connection, id: &str) -> Result<CandidateVerification> {
    let (candidate_id, policy_revision, started_at, finished_at, pinned): (String, String, String, Option<String>, Vec<u8>) = db.query_row(
        "SELECT candidate_id, policy_revision, started_at, finished_at, gates FROM candidate_verifications WHERE id = ?1", [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?;
    let mut gates = decode::<Vec<PinnedGate>>(&pinned)?
        .into_iter()
        .map(|pinned| CandidateGateResult {
            gate: pinned.gate,
            executable_revision: pinned.executable_revision,
            outcome: None,
        })
        .collect::<Vec<_>>();
    let mut query = db.prepare("SELECT gate_index, payload FROM candidate_gate_results WHERE verification_id = ?1 ORDER BY gate_index")?;
    let mut rows = query.query([id])?;
    while let Some(row) = rows.next()? {
        let index = usize::try_from(row.get::<_, i64>(0)?)?;
        let payload: Vec<u8> = row.get(1)?;
        gates
            .get_mut(index)
            .context("gate result is outside the saved policy")?
            .outcome = Some(decode(&payload)?);
    }
    Ok(CandidateVerification {
        id: id.into(),
        candidate_id,
        policy_revision,
        started_at,
        finished_at,
        gates,
    })
}

pub(crate) fn load_scoped_candidate_verification(
    db: &Connection,
    project: &str,
    task: &str,
    id: &str,
) -> Result<Option<CandidateVerification>> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM candidate_verifications v JOIN selected_candidate_revisions c ON c.id = v.candidate_id
         WHERE c.project_id = ?1 AND c.task_id = ?2 AND v.id = ?3)",
        params![project, task, id],
        |row| row.get(0),
    )?;
    exists.then(|| load_verification(db, id)).transpose()
}

pub(crate) fn load_scoped_candidate_acceptance(
    db: &Connection,
    project: &str,
    task: &str,
    id: &str,
) -> Result<Option<CandidateAcceptance>> {
    let row: Option<(String, String, String)> = db.query_row(
        "SELECT a.candidate_id, a.verification_id, a.accepted_at FROM candidate_acceptances a
         JOIN selected_candidate_revisions c ON c.id = a.candidate_id WHERE c.project_id = ?1 AND c.task_id = ?2 AND a.id = ?3",
        params![project, task, id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
    row.map(|(candidate, verification, accepted_at)| {
        Ok(CandidateAcceptance {
            id: id.into(),
            candidate: load_selected_candidate(db, project, task, &candidate)?,
            verification: load_verification(db, &verification)?,
            accepted_at,
        })
    })
    .transpose()
}

pub(crate) fn load_current_candidate_acceptance(
    db: &Connection,
    project: &str,
    task: &str,
    id: &str,
) -> Result<CandidateAcceptance> {
    let acceptance = load_scoped_candidate_acceptance(db, project, task, id)?
        .context("candidate acceptance is outside the requested work scope")?;
    require_current(db, &acceptance.candidate)?;
    let current = current_verification(db, &acceptance.candidate)?;
    ensure!(
        current.id == acceptance.verification.id && current.allows_acceptance(),
        "verification or acceptance was superseded"
    );
    Ok(acceptance)
}

impl CandidateSelection {
    fn normalize(&mut self) -> Result<()> {
        for paths in self
            .tracked_paths
            .iter_mut()
            .chain(std::iter::once(&mut self.untracked_paths))
        {
            ensure!(
                paths
                    .iter()
                    .all(|path| !path.contains(['\\', ':', '\r', '\n', '\0'])
                        && path.split('/').all(|part| !part.is_empty()
                            && part != "."
                            && part != ".."
                            && !part.eq_ignore_ascii_case(".git"))),
                "candidate selection contains a path outside the checkout"
            );
            paths.sort_unstable();
            paths.dedup();
        }
        Ok(())
    }
}

fn pin_policy(gates: Vec<CandidateGate>) -> Result<Vec<PinnedGate>> {
    let mut names = HashSet::new();
    gates
        .into_iter()
        .map(|mut gate| {
            ensure!(
                !gate.name.trim().is_empty() && names.insert(gate.name.clone()),
                "gate names must be non-empty and unique"
            );
            let executable_revision = match &mut gate.action {
                CandidateGateAction::Command {
                    executable,
                    timeout_seconds,
                    ..
                } => {
                    ensure!(
                        executable.is_absolute(),
                        "a gate command needs an absolute executable path"
                    );
                    ensure!(
                        *timeout_seconds != Some(0),
                        "a configured timeout must be positive"
                    );
                    if let Ok(path) = dunce::canonicalize(&executable) {
                        *executable = path;
                    }
                    command::pin_executable(executable)
                        .ok()
                        .map(|(_, revision)| revision)
                }
                CandidateGateAction::Unavailable { reason } => {
                    ensure!(
                        !reason.trim().is_empty(),
                        "an unavailable gate needs a reason"
                    );
                    None
                }
            };
            Ok(PinnedGate {
                gate,
                executable_revision,
            })
        })
        .collect()
}

fn fingerprint<T: Serialize>(value: &T) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn decode<T: DeserializeOwned>(value: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(value)?)?)
}
