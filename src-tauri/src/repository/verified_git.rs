mod git;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    protection,
    store::Store,
    workflows::{
        WorkflowDeliveryUnit, bind_delivery_commit, load_delivery_unit, load_delivery_units,
        validate_unit_acceptance, validate_unit_capture,
    },
};

use super::{
    AttemptWorktreeEvidence, ManagedWorktree,
    verification::{CandidateAcceptance, load_current_candidate_acceptance},
    worktrees::load_worktree,
    writers::{clear_pending_changes, release_app_writer, reserve_app_writer, writer_owner},
};
use git::{DeliveryCheckout, validate_oid};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGitPolicy {
    #[default]
    AutomaticCommitUserMerge,
    UserCommitsAndMerges,
}

impl DeliveryGitPolicy {
    fn key(self) -> &'static str {
        match self {
            Self::AutomaticCommitUserMerge => "automatic_commit_user_merge",
            Self::UserCommitsAndMerges => "user_commits_and_merges",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryCommitRequest {
    pub project_id: String,
    pub task_id: String,
    pub unit_id: String,
    pub purpose: String,
    pub acceptance_id: String,
    pub acceptance_artifact_id: String,
    pub external_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitSource {
    Automatic,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedCommit {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub unit_id: String,
    pub acceptance_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_artifact_id: Option<String>,
    pub candidate_revision: String,
    pub verification_id: String,
    pub capture_run_id: String,
    pub checkout_id: String,
    pub branch: String,
    pub repository: String,
    pub base_commit: String,
    pub tree: String,
    pub sha: String,
    pub purpose: String,
    pub verified_at: String,
    pub source: CommitSource,
    pub pending_changes_retained: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryCommitOutcome {
    WaitingForCommit,
    Verified { evidence: VerifiedCommit },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CommitIntent {
    sha: String,
    captured_index_digest: String,
    #[serde(default)]
    acceptance_artifact_id: Option<String>,
}

struct DeliveryState {
    acceptance: CandidateAcceptance,
    unit: WorkflowDeliveryUnit,
    policy: DeliveryGitPolicy,
    managed: (ManagedWorktree, String, String),
    intent: Option<CommitIntent>,
}

enum DeliveryRead {
    Saved(VerifiedCommit),
    Current(DeliveryState),
}

impl Store {
    pub async fn delivery_git_policy(&self, project: String) -> Result<DeliveryGitPolicy> {
        self.execute(move |db| load_policy(db, &project)).await
    }

    pub async fn set_delivery_git_policy(
        &self,
        project: String,
        policy: DeliveryGitPolicy,
    ) -> Result<DeliveryGitPolicy> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_delivery_git_policies VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode",
                params![project, policy.key()],
            )?;
            Ok(policy)
        })
        .await
    }

    pub async fn verified_delivery_commit(
        &self,
        project: String,
        task: String,
        evidence_id: String,
    ) -> Result<Option<VerifiedCommit>> {
        self.execute(move |db| load_verified_delivery_commit(db, &project, &task, &evidence_id))
            .await
    }

    pub async fn verify_delivery_commit(
        &self,
        mut request: DeliveryCommitRequest,
    ) -> Result<DeliveryCommitOutcome> {
        ensure!(
            !request.unit_id.is_empty() && !request.purpose.trim().is_empty(),
            "a logical unit and its purpose are required"
        );
        ensure!(
            !request.purpose.contains('\0'),
            "the purpose cannot contain a NUL character"
        );
        if let Some(sha) = &mut request.external_sha {
            validate_oid(sha)?;
            sha.make_ascii_lowercase();
        }
        let state = self
            .execute({
                let request = request.clone();
                move |db| load_delivery(db, &request)
            })
            .await?;
        let state = match state {
            DeliveryRead::Saved(evidence) => {
                return Ok(DeliveryCommitOutcome::Verified { evidence });
            }
            DeliveryRead::Current(state) => state,
        };
        if state.policy == DeliveryGitPolicy::UserCommitsAndMerges && request.external_sha.is_none()
        {
            return Ok(DeliveryCommitOutcome::WaitingForCommit);
        }
        let policy = state.policy;
        if request.external_sha.is_none() {
            self.prepare_delivery_commit(&request, state).await?;
        }
        // Keep Git publication and attribution serialized against candidate,
        // policy, and scheduler writes. A persisted intent handles an exit
        // between advancing the ref and committing the receipt transaction.
        let evidence = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let state = match load_delivery(&tx, &request)? {
                    DeliveryRead::Saved(evidence) => return Ok(evidence),
                    DeliveryRead::Current(state) => state,
                };
                ensure!(
                    state.policy == policy,
                    "Git delivery policy changed; retry the commit action"
                );
                let evidence = settle_commit(&tx, request, state)?;
                tx.commit()?;
                Ok(evidence)
            })
            .await?;
        self.job_signal.send_replace(());
        self.event_signal.send_replace(());
        Ok(DeliveryCommitOutcome::Verified { evidence })
    }

    async fn prepare_delivery_commit(
        &self,
        request: &DeliveryCommitRequest,
        state: DeliveryState,
    ) -> Result<()> {
        let acceptance = state.acceptance;
        let (worktree, git_dir, common) = state.managed;
        let checkout = tokio::task::spawn_blocking({
            let candidate = acceptance.candidate.clone();
            move || {
                let checkout = DeliveryCheckout::open(&worktree, &git_dir, &common, &candidate)?;
                let head = checkout.head()?;
                Ok::<_, anyhow::Error>((checkout, head))
            }
        })
        .await??;
        let (checkout, head) = checkout;
        if head != acceptance.candidate.base_commit {
            checkout.matches(&head, &acceptance.candidate)?;
            return Ok(());
        }
        if let Some(intent) = state.intent {
            let candidate = acceptance.candidate.clone();
            let existing = tokio::task::spawn_blocking(move || {
                let available = checkout.matches(&intent.sha, &candidate).is_ok();
                (checkout, available)
            })
            .await?;
            let (checkout, available) = existing;
            if available {
                return Ok(());
            }
            return self
                .prepare_delivery_objects(request, acceptance, checkout)
                .await;
        }
        self.prepare_delivery_objects(request, acceptance, checkout)
            .await
    }

    async fn prepare_delivery_objects(
        &self,
        request: &DeliveryCommitRequest,
        acceptance: CandidateAcceptance,
        checkout: DeliveryCheckout,
    ) -> Result<()> {
        let candidate = &acceptance.candidate;
        let lease = self
            .materialize_selected_candidate(
                request.project_id.clone(),
                request.task_id.clone(),
                candidate.id.clone(),
            )
            .await?;
        let Some(AttemptWorktreeEvidence::Saved { artifact }) = self
            .attempt_worktree_evidence(request.project_id.clone(), candidate.target.run_id.clone())
            .await?
        else {
            anyhow::bail!("captured index evidence disappeared");
        };
        ensure!(
            artifact.revision == candidate.target.capture_revision,
            "captured index revision changed"
        );
        let mut intent = tokio::task::spawn_blocking({
            let purpose = request.purpose.clone();
            move || {
                checkout.prepare(
                    lease.path(),
                    &acceptance,
                    &purpose,
                    artifact.candidate.index_digest,
                )
            }
        })
        .await??;
        intent.acceptance_artifact_id = Some(request.acceptance_artifact_id.clone());
        self.execute({
            let request = request.clone();
            move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let state = match load_delivery(&tx, &request)? {
                    DeliveryRead::Saved(_) => return Ok(()),
                    DeliveryRead::Current(state) => state,
                };
                ensure!(state.policy == DeliveryGitPolicy::AutomaticCommitUserMerge, "Git delivery policy changed; retry the commit action");
                if let Some(previous) = state.intent {
                    ensure!(previous.sha == intent.sha && previous.captured_index_digest == intent.captured_index_digest,
                        "a different commit is already prepared for this unit");
                } else {
                    tx.execute(
                        "INSERT INTO delivery_commit_intents(acceptance_id, project_id, task_id, unit_id, payload)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![request.acceptance_id, request.project_id, request.task_id, request.unit_id, encode(&intent)?],
                    )?;
                }
                tx.commit()?;
                Ok(())
            }
        }).await
    }
}

fn load_policy(db: &Connection, project: &str) -> Result<DeliveryGitPolicy> {
    let mode: Option<String> = db.query_row(
        "SELECT m.mode FROM projects p LEFT JOIN project_delivery_git_policies m ON m.project_id = p.id WHERE p.id = ?1",
        [project],
        |row| row.get(0),
    ).optional()?.context("Git delivery requires a registered project")?;
    match mode.as_deref() {
        None | Some("automatic_commit_user_merge") => {
            Ok(DeliveryGitPolicy::AutomaticCommitUserMerge)
        }
        Some("user_commits_and_merges") => Ok(DeliveryGitPolicy::UserCommitsAndMerges),
        Some(_) => anyhow::bail!("invalid Git delivery policy"),
    }
}

fn load_delivery(db: &Connection, request: &DeliveryCommitRequest) -> Result<DeliveryRead> {
    let receipt: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM verified_delivery_commits WHERE project_id = ?1 AND task_id = ?2 AND unit_id = ?3",
        params![request.project_id, request.task_id, request.unit_id],
        |row| row.get(0),
    ).optional()?;
    if let Some(receipt) = receipt {
        let evidence: VerifiedCommit = decode(&receipt)?;
        ensure!(
            evidence.acceptance_id == request.acceptance_id
                && evidence
                    .acceptance_artifact_id
                    .as_ref()
                    .is_none_or(|id| id == &request.acceptance_artifact_id)
                && evidence.purpose == request.purpose
                && request
                    .external_sha
                    .as_ref()
                    .is_none_or(|sha| sha == &evidence.sha),
            "the logical unit already has different verified commit evidence"
        );
        return Ok(DeliveryRead::Saved(evidence));
    }
    let acceptance = load_current_candidate_acceptance(
        db,
        &request.project_id,
        &request.task_id,
        &request.acceptance_id,
    )?;
    ensure!(
        acceptance.candidate.target.unit_id == request.unit_id,
        "candidate acceptance belongs to a different logical unit"
    );
    let unit = load_delivery_unit(db, &request.project_id, &request.task_id, &request.unit_id)?
        .context("logical unit does not belong to the current approved plan")?;
    ensure!(
        unit.unit.purpose == request.purpose
            && unit.checkout_id == acceptance.candidate.checkout_id,
        "the candidate or requested purpose differs from the approved logical unit"
    );
    validate_unit_capture(db, &unit, &acceptance.candidate.target.run_id)?;
    validate_unit_acceptance(db, &unit, &acceptance.id, &request.acceptance_artifact_id)?;
    check_predecessor(db, &unit, &acceptance)?;
    let owner = writer_owner(
        db,
        &request.project_id,
        &request.task_id,
        &acceptance.candidate.checkout_id,
    )?
    .context("the accepted candidate is outside its managed worktree scope")?;
    let managed = load_worktree(db, &request.project_id, &owner)?
        .context("the managed worktree is unavailable")?;
    let policy = load_policy(db, &request.project_id)?;
    let intent: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM delivery_commit_intents WHERE acceptance_id = ?1 AND project_id = ?2 AND task_id = ?3 AND unit_id = ?4",
        params![request.acceptance_id, request.project_id, request.task_id, request.unit_id],
        |row| row.get(0),
    ).optional()?;
    let intent: Option<CommitIntent> = intent.as_deref().map(decode).transpose()?;
    ensure!(
        intent
            .as_ref()
            .and_then(|intent| intent.acceptance_artifact_id.as_ref())
            .is_none_or(|id| id == &request.acceptance_artifact_id),
        "the prepared commit records a different acceptance artifact"
    );
    Ok(DeliveryRead::Current(DeliveryState {
        acceptance,
        unit,
        policy,
        managed,
        intent,
    }))
}

fn check_predecessor(
    db: &Connection,
    unit: &WorkflowDeliveryUnit,
    acceptance: &CandidateAcceptance,
) -> Result<()> {
    if unit.unit.ordinal == 1 {
        return Ok(());
    }
    let previous = load_delivery_units(db, &unit.project_id, &unit.task_id, &unit.workflow_run_id)?
        .into_iter()
        .find(|previous| previous.unit.ordinal == unit.unit.ordinal - 1)
        .and_then(|previous| previous.commit_evidence_id)
        .context("the preceding logical unit must have a verified commit")?;
    let previous = load_verified_delivery_commit(db, &unit.project_id, &unit.task_id, &previous)?
        .context("the preceding logical unit's commit evidence disappeared")?;
    let candidate = &acceptance.candidate;
    ensure!(
        candidate.base_commit == previous.sha
            && candidate.checkout_id == previous.checkout_id
            && candidate.branch == previous.branch,
        "the accepted candidate does not continue the previous commit in the same worktree"
    );
    Ok(())
}

fn settle_commit(
    db: &Connection,
    request: DeliveryCommitRequest,
    state: DeliveryState,
) -> Result<VerifiedCommit> {
    let candidate = &state.acceptance.candidate;
    let operation = Uuid::new_v4().to_string();
    let managed = reserve_app_writer(
        db,
        &request.project_id,
        &candidate.checkout_id,
        &request.task_id,
        &operation,
    )?;
    ensure!(
        managed == state.managed,
        "managed checkout changed during commit admission"
    );
    ensure_owned_pending(db, &request, &candidate.checkout_id)?;
    let checkout = DeliveryCheckout::open(&managed.0, &managed.1, &managed.2, candidate)?;
    let actual_head = checkout.head()?;
    let mut update_branch = false;
    let sha = if let Some(external) = &request.external_sha {
        ensure!(
            external == &actual_head,
            "supplied commit is not the head of the registered working branch"
        );
        external.clone()
    } else if actual_head == candidate.base_commit {
        ensure!(
            state.policy == DeliveryGitPolicy::AutomaticCommitUserMerge,
            "manual Git delivery requires a commit SHA"
        );
        update_branch = true;
        state
            .intent
            .as_ref()
            .context("automatic commit object has not been prepared")?
            .sha
            .clone()
    } else {
        actual_head
    };
    checkout.matches(&sha, candidate)?;
    let existing: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM verified_delivery_commits WHERE project_id = ?1 AND sha = ?2)",
        params![request.project_id, sha],
        |row| row.get(0),
    )?;
    ensure!(!existing, "this SHA was already verified for another unit");
    if update_branch {
        checkout.advance(&candidate.base_commit, &sha)?;
    }
    let automatic = state.intent.as_ref().filter(|intent| intent.sha == sha);
    if let Some(intent) = automatic {
        checkout.align_index(intent, &candidate.base_commit)?;
    }
    ensure!(
        checkout.head()? == sha,
        "working branch moved before commit verification finished"
    );
    let pending_changes_retained = !checkout.clean(&sha);
    if pending_changes_retained {
        db.execute(
            "UPDATE pending_worktree_changes SET discard_starting_head = ?4
             WHERE project_id = ?1 AND checkout_id = ?2 AND scope_task_id = ?3",
            params![
                request.project_id,
                candidate.checkout_id,
                request.task_id,
                sha
            ],
        )?;
    } else {
        clear_pending_changes(
            db,
            &request.project_id,
            &candidate.checkout_id,
            &request.task_id,
        )?;
    }
    let evidence = VerifiedCommit {
        id: Uuid::new_v4().to_string(),
        project_id: request.project_id,
        task_id: request.task_id,
        unit_id: request.unit_id,
        acceptance_id: state.acceptance.id,
        acceptance_artifact_id: Some(request.acceptance_artifact_id),
        candidate_revision: candidate.id.clone(),
        verification_id: state.acceptance.verification.id,
        capture_run_id: candidate.target.run_id.clone(),
        checkout_id: candidate.checkout_id.clone(),
        branch: candidate.branch.clone(),
        repository: checkout.repository().to_owned(),
        base_commit: candidate.base_commit.clone(),
        tree: candidate.tree.clone(),
        sha,
        purpose: request.purpose,
        verified_at: db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
            row.get(0)
        })?,
        source: if automatic.is_some() {
            CommitSource::Automatic
        } else {
            CommitSource::External
        },
        pending_changes_retained,
    };
    db.execute(
        "INSERT INTO verified_delivery_commits(
            id, project_id, task_id, unit_id, checkout_id, acceptance_id, candidate_revision,
            verification_id, capture_run_id, sha, verified_at, payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            evidence.id,
            evidence.project_id,
            evidence.task_id,
            evidence.unit_id,
            evidence.checkout_id,
            evidence.acceptance_id,
            evidence.candidate_revision,
            evidence.verification_id,
            evidence.capture_run_id,
            evidence.sha,
            evidence.verified_at,
            encode(&evidence)?
        ],
    )?;
    bind_delivery_commit(db, &state.unit, &evidence.id)?;
    db.execute(
        "DELETE FROM delivery_commit_intents WHERE acceptance_id = ?1",
        [&evidence.acceptance_id],
    )?;
    release_app_writer(db, &evidence.checkout_id, &operation)?;
    Ok(evidence)
}

fn ensure_owned_pending(
    db: &Connection,
    request: &DeliveryCommitRequest,
    checkout: &str,
) -> Result<()> {
    let pending: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_worktree_changes WHERE project_id = ?1 AND scope_task_id = ?2 AND checkout_id = ?3)",
        params![request.project_id, request.task_id, checkout],
        |row| row.get(0),
    )?;
    ensure!(
        pending,
        "the producing scope no longer owns pending changes for this accepted candidate"
    );
    Ok(())
}

pub(crate) fn load_verified_delivery_commit(
    db: &Connection,
    project: &str,
    task: &str,
    evidence_id: &str,
) -> Result<Option<VerifiedCommit>> {
    let payload: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM verified_delivery_commits WHERE project_id = ?1 AND task_id = ?2 AND id = ?3",
        params![project, task, evidence_id],
        |row| row.get(0),
    ).optional()?;
    payload.as_deref().map(decode).transpose()
}

fn encode(data: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(data)?)?)
}

fn decode<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(data)?)?)
}
