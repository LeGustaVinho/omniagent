mod attached;
mod git;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    protection,
    store::Store,
    tasks::{
        WorkStateChanges, delivery_merge_candidate, load_delivery_milestone_policy,
        pipeline_work_origin, record_verified_task_merge,
    },
};

use super::{
    destination_drift::{DestinationDriftOutcome, authorized_destination_drift},
    verified_git::{DeliveryGitPolicy, VerifiedCommit, load_policy, load_verified_delivery_commit},
    writers::ensure_merge_destination_available,
};
use attached::{AttachedMergePlan, AttachedRecovery};
use git::{AttachedPublication, MergeRepository, Plan, Publication, validate_oid};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryMergeRequest {
    pub project_id: String,
    pub task_id: String,
    pub workflow_run_id: String,
    pub unit_id: String,
    pub commit_evidence_id: String,
    pub expected_source_branch: String,
    pub expected_source_sha: String,
    pub destination_branch: String,
    pub expected_destination_sha: String,
    pub external_sha: Option<String>,
    pub drift_acceptance_id: Option<String>,
    #[serde(default)]
    pub prepare_drift: bool,
    #[serde(default)]
    pub perform: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryMergePreview {
    pub source_branch: String,
    pub source_sha: String,
    pub destination_branch: String,
    pub expected_destination_sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeSource {
    Automatic,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedMerge {
    pub id: String,
    pub project_id: String,
    pub task_id: String,
    pub workflow_run_id: String,
    pub unit_id: String,
    pub commit_evidence_id: String,
    pub commit_evidence_ids: Vec<String>,
    pub source_branch: String,
    pub source_sha: String,
    pub destination_branch: String,
    pub previous_destination_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_destination_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_acceptance_id: Option<String>,
    pub integration_sha: String,
    pub verified_at: String,
    pub source: MergeSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DeliveryMergeOutcome {
    WaitingForMerge,
    DestinationMoved {
        actual_sha: String,
    },
    DestinationDrift {
        recovery: DestinationDriftOutcome,
    },
    DestinationCheckedOut {
        checkout: String,
    },
    RecoveryRequired {
        checkout: String,
        recovery_directory: String,
        message: String,
    },
    Conflicted,
    Verified {
        evidence: VerifiedMerge,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MergeIntent {
    source_branch: String,
    source_sha: String,
    destination_branch: String,
    expected_destination_sha: String,
    destination_sha: String,
    drift_acceptance_id: Option<String>,
    integration_sha: String,
    #[serde(default)]
    attached: Option<AttachedMergePlan>,
}

struct MergeScope {
    commits: Vec<VerifiedCommit>,
    destination_branch: String,
    repo: MergeRepository,
}

enum Preparation {
    Return(DeliveryMergeOutcome),
    Settle(Option<MergeIntent>),
}

#[derive(Clone, Copy)]
enum Invocation {
    User,
    Workflow,
}

impl Store {
    pub async fn delivery_merge_preview(
        &self,
        project: String,
        task: String,
        workflow_run: String,
        unit: String,
        commit_evidence_id: String,
    ) -> Result<DeliveryMergePreview> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(saved) =
                load_verified_delivery_merge(&tx, &project, &task, &commit_evidence_id)?
            {
                ensure!(
                    saved.workflow_run_id == workflow_run && saved.unit_id == unit,
                    "merge evidence belongs to another delivery"
                );
                return Ok(DeliveryMergePreview {
                    source_branch: saved.source_branch,
                    source_sha: saved.source_sha,
                    destination_branch: saved.destination_branch,
                    expected_destination_sha: saved
                        .original_destination_sha
                        .unwrap_or(saved.previous_destination_sha),
                });
            }
            let scope = load_scope(
                &tx,
                &project,
                &task,
                &workflow_run,
                &unit,
                &commit_evidence_id,
            )?;
            let last = scope
                .commits
                .last()
                .context("delivery has no verified commits")?;
            if let Some(preview) = load_preview(&tx, &commit_evidence_id)? {
                ensure!(
                    preview.destination_branch == scope.destination_branch,
                    "the configured destination changed after the delivery was previewed"
                );
                return Ok(preview);
            }
            let preview = DeliveryMergePreview {
                source_branch: last.branch.clone(),
                source_sha: last.sha.clone(),
                expected_destination_sha: scope.repo.branch_tip(&scope.destination_branch)?,
                destination_branch: scope.destination_branch,
            };
            tx.execute(
                "INSERT INTO delivery_merge_previews(commit_evidence_id, payload) VALUES (?1, ?2)",
                params![commit_evidence_id, encode(&preview)?],
            )?;
            tx.commit()?;
            Ok(preview)
        })
        .await
    }

    pub async fn verified_delivery_merge(
        &self,
        project: String,
        task: String,
        commit_evidence_id: String,
    ) -> Result<Option<VerifiedMerge>> {
        self.execute(move |db| {
            load_verified_delivery_merge(db, &project, &task, &commit_evidence_id)
        })
        .await
    }

    pub async fn verified_delivery_merge_for_workflow(
        &self,
        project: String,
        task: String,
        workflow_run: String,
    ) -> Result<Option<VerifiedMerge>> {
        self.execute(move |db| {
            let payload: Option<Vec<u8>> = db
                .query_row(
                    "SELECT m.payload FROM workflow_verified_merges m
                 WHERE m.project_id = ?1 AND m.task_id = ?2 AND m.workflow_run_id = ?3
                 ORDER BY m.verified_at DESC, m.id DESC LIMIT 1",
                    params![project, task, workflow_run],
                    |row| row.get(0),
                )
                .optional()?;
            payload.map(|data| decode(&data)).transpose()
        })
        .await
    }

    pub async fn verify_delivery_merge(
        &self,
        request: DeliveryMergeRequest,
    ) -> Result<DeliveryMergeOutcome> {
        self.verify_merge(request, Invocation::User).await
    }

    pub(crate) async fn verify_automatic_delivery_merge(
        &self,
        request: DeliveryMergeRequest,
    ) -> Result<DeliveryMergeOutcome> {
        ensure!(
            request.external_sha.is_none() && !request.prepare_drift,
            "automatic merge requires local Git and the configured recovery policy"
        );
        self.verify_merge(request, Invocation::Workflow).await
    }

    async fn verify_merge(
        &self,
        mut request: DeliveryMergeRequest,
        invocation: Invocation,
    ) -> Result<DeliveryMergeOutcome> {
        for sha in [
            &mut request.expected_source_sha,
            &mut request.expected_destination_sha,
        ]
        .into_iter()
        .chain(request.external_sha.iter_mut())
        {
            validate_oid(sha)?;
            sha.make_ascii_lowercase();
        }
        let mut outcome = self
            .verify_at_destination(&request, invocation, None)
            .await?;
        if let DeliveryMergeOutcome::DestinationMoved { actual_sha } = &outcome {
            if request.external_sha.is_none() && actual_sha != &request.expected_destination_sha {
                let recovery = self.merge_recovery(&request, actual_sha).await?;
                outcome = match recovery {
                    DestinationDriftOutcome::Accepted {
                        preview,
                        ref acceptance_id,
                        ..
                    } if request.perform
                        && request.drift_acceptance_id.as_ref() == Some(acceptance_id) =>
                    {
                        self.verify_at_destination(
                            &request,
                            invocation,
                            Some(preview.destination_sha),
                        )
                        .await?
                    }
                    _ => DeliveryMergeOutcome::DestinationDrift { recovery },
                };
                if let DeliveryMergeOutcome::DestinationMoved { actual_sha } = &outcome
                    && actual_sha != &request.expected_destination_sha
                {
                    outcome = DeliveryMergeOutcome::DestinationDrift {
                        recovery: self.merge_recovery(&request, actual_sha).await?,
                    };
                }
            }
        }
        if matches!(outcome, DeliveryMergeOutcome::Verified { .. }) {
            self.job_signal.send_replace(());
            self.event_signal.send_replace(());
        }
        Ok(outcome)
    }

    async fn merge_recovery(
        &self,
        request: &DeliveryMergeRequest,
        actual_sha: &str,
    ) -> Result<DestinationDriftOutcome> {
        let source = self
            .verified_delivery_commit(
                request.project_id.clone(),
                request.task_id.clone(),
                request.commit_evidence_id.clone(),
            )
            .await?
            .context("destination update requires the verified final delivery commit")?;
        self.prepare_delivery_destination_drift(
            &source,
            &request.workflow_run_id,
            &request.destination_branch,
            &request.expected_destination_sha,
            actual_sha,
            request.prepare_drift,
        )
        .await
    }

    async fn verify_at_destination(
        &self,
        request: &DeliveryMergeRequest,
        invocation: Invocation,
        approved_tip: Option<String>,
    ) -> Result<DeliveryMergeOutcome> {
        let preparation = self
            .execute({
                let request = request.clone();
                move |db| {
                    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let preparation =
                        match prepare_merge(&tx, &request, invocation, approved_tip.as_deref()) {
                            Ok(preparation) => preparation,
                            Err(error) => return recovery_outcome(error).map(Preparation::Return),
                        };
                    tx.commit()?;
                    Ok(preparation)
                }
            })
            .await?;
        let intent = match preparation {
            Preparation::Return(outcome) => return Ok(outcome),
            Preparation::Settle(intent) => intent,
        };
        let request = request.clone();
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut attached = None;
            let outcome =
                match settle_merge(&tx, &request, intent.as_ref(), invocation, &mut attached) {
                    Ok(outcome) => outcome,
                    Err(error) => return finish_attachment_error(intent.as_ref(), error),
                };
            if let Err(error) = tx.commit() {
                return finish_attachment_error(intent.as_ref(), error.into());
            }
            drop(attached);
            if matches!(&outcome, DeliveryMergeOutcome::Verified { .. })
                && let Some(plan) = intent.as_ref().and_then(|intent| intent.attached.as_ref())
            {
                // The receipt is durable; an unavailable cleanup cannot undo it.
                let _ = plan.cleanup_after_receipt();
            }
            Ok(outcome)
        })
        .await
    }
}

fn load_scope(
    db: &Connection,
    project: &str,
    task: &str,
    workflow: &str,
    unit: &str,
    commit: &str,
) -> Result<MergeScope> {
    let candidate = delivery_merge_candidate(db, project, task, workflow)?
        .context("merge requires the committed chain of the active approved delivery")?;
    ensure!(
        candidate.unit_id == unit && candidate.commit_evidence_id == commit,
        "merge requires the final verified unit of this delivery"
    );
    let commits = candidate
        .commit_evidence_ids
        .iter()
        .map(|id| {
            load_verified_delivery_commit(db, project, task, id)?
                .context("verified delivery commit is missing")
        })
        .collect::<Result<Vec<_>>>()?;
    let last = commits.last().context("delivery has no verified commits")?;
    ensure!(
        last.id == commit
            && last.branch == candidate.source_branch
            && last.sha == candidate.source_sha,
        "final verified commit does not match the task delivery"
    );
    let (root, common): (String, String) = db
        .query_row(
            "SELECT root, git_common_dir FROM projects WHERE id = ?1",
            [project],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("load the delivery repository")?;
    ensure!(
        commits.iter().all(|item| item.repository == common),
        "approved commits belong to a different repository"
    );
    let repo = MergeRepository::open(PathBuf::from(root), &common)?;
    let destination_branch = match candidate
        .destination_branch
        .or(load_delivery_milestone_policy(db, project)?.destination_branch)
        .or(load_preview(db, commit)?.map(|preview| preview.destination_branch))
    {
        Some(branch) => branch,
        None => repo.primary_branch()?,
    };
    Ok(MergeScope {
        commits,
        destination_branch,
        repo,
    })
}

impl MergeScope {
    fn load(db: &Connection, request: &DeliveryMergeRequest) -> Result<Self> {
        let scope = load_scope(
            db,
            &request.project_id,
            &request.task_id,
            &request.workflow_run_id,
            &request.unit_id,
            &request.commit_evidence_id,
        )?;
        let last = scope
            .commits
            .last()
            .context("delivery has no verified commits")?;
        ensure!(
            last.branch == request.expected_source_branch
                && last.sha == request.expected_source_sha,
            "working branch or SHA differs from the approved delivery"
        );
        ensure!(
            scope.destination_branch == request.destination_branch,
            "destination branch differs from the configured delivery destination"
        );
        ensure!(
            last.branch != scope.destination_branch,
            "source and destination branches must differ"
        );
        let preview = load_preview(db, &request.commit_evidence_id)?
            .context("preview the approved delivery before verifying its merge")?;
        ensure!(
            preview.source_branch == request.expected_source_branch
                && preview.source_sha == request.expected_source_sha
                && preview.destination_branch == request.destination_branch
                && preview.expected_destination_sha == request.expected_destination_sha,
            "merge request differs from the previewed source or destination"
        );
        Ok(scope)
    }
}

fn find_saved(
    db: &Connection,
    request: &DeliveryMergeRequest,
) -> Result<Option<DeliveryMergeOutcome>> {
    let Some(evidence) = load_verified_delivery_merge(
        db,
        &request.project_id,
        &request.task_id,
        &request.commit_evidence_id,
    )?
    else {
        return Ok(None);
    };
    ensure!(
        evidence.workflow_run_id == request.workflow_run_id
            && evidence.unit_id == request.unit_id
            && evidence.source_branch == request.expected_source_branch
            && evidence.source_sha == request.expected_source_sha
            && evidence.destination_branch == request.destination_branch
            && evidence
                .original_destination_sha
                .as_ref()
                .unwrap_or(&evidence.previous_destination_sha)
                == &request.expected_destination_sha
            && request
                .external_sha
                .as_ref()
                .is_none_or(|sha| sha == &evidence.integration_sha)
            && request
                .drift_acceptance_id
                .as_ref()
                .is_none_or(|id| evidence.drift_acceptance_id.as_ref() == Some(id)),
        "verified merge receipt has different approval pins"
    );
    Ok(Some(DeliveryMergeOutcome::Verified { evidence }))
}

fn prepare_merge(
    db: &Connection,
    request: &DeliveryMergeRequest,
    invocation: Invocation,
    approved_tip: Option<&str>,
) -> Result<Preparation> {
    if let Some(saved) = find_saved(db, request)? {
        return Ok(Preparation::Return(saved));
    }
    let scope = MergeScope::load(db, request)?;
    let previous = load_intent(db, &request.commit_evidence_id)?;
    let Some(actual_sha) = scope.repo.find_branch_tip(&request.destination_branch)? else {
        require_unmodified_attachment(
            previous.as_ref(),
            "destination disappeared after the worktree update began",
        )?;
        return Ok(Preparation::Return(missing_destination(request)));
    };
    if let Some(external_sha) = &request.external_sha {
        if let Some(intent) = &previous
            && actual_sha == intent.integration_sha
            && *external_sha == actual_sha
            && intent.attached.is_some()
        {
            ensure!(
                intent.matches(request),
                "attached merge has different approval pins"
            );
            return Ok(Preparation::Settle(Some(intent.clone())));
        }
        require_unmodified_attachment(
            previous.as_ref(),
            "resume the attached checkout update before linking a different integration",
        )?;
        return Ok(Preparation::Settle(None));
    }
    if let Some(intent) = &previous {
        ensure!(
            intent.matches(request),
            "prepared merge has different approval pins"
        );
        if actual_sha == intent.integration_sha {
            return Ok(Preparation::Settle(Some(intent.clone())));
        }
        if actual_sha != intent.destination_sha {
            require_unmodified_attachment(
                Some(intent),
                "destination changed after the worktree update began",
            )?;
        }
    }
    if Some(actual_sha.as_str()) != approved_tip && actual_sha != request.expected_destination_sha {
        return Ok(Preparation::Return(
            DeliveryMergeOutcome::DestinationMoved { actual_sha },
        ));
    }
    if !local_merge_allowed(db, request, invocation)? {
        require_unmodified_attachment(
            previous.as_ref(),
            "local merge is no longer authorized for the pending worktree update",
        )?;
        return Ok(Preparation::Return(DeliveryMergeOutcome::WaitingForMerge));
    }
    let approved_tree = verify_drift_approval(
        db,
        &scope,
        request,
        &actual_sha,
        request.drift_acceptance_id.as_deref(),
    )?;
    let attached_checkout = scope
        .repo
        .attached_checkout(&request.destination_branch)?
        .map(dunce::canonicalize)
        .transpose()?;
    if let Some(intent) = previous {
        if intent.destination_sha == actual_sha
            && intent.drift_acceptance_id == request.drift_acceptance_id
            && reusable_attachment(intent.attached.as_ref(), attached_checkout.as_deref())?
        {
            return Ok(Preparation::Settle(Some(intent)));
        }
    }
    scope.repo.verify_chain(&scope.commits)?;
    ensure!(
        scope.repo.branch_tip(&request.expected_source_branch)? == request.expected_source_sha,
        "approved working branch moved before merge"
    );
    let mut publication = request.clone();
    publication.expected_destination_sha.clone_from(&actual_sha);
    let integration_sha = match scope.repo.prepare(&publication, &scope.commits)? {
        Plan::Conflicted => return Ok(Preparation::Return(DeliveryMergeOutcome::Conflicted)),
        Plan::Ready(sha) => sha,
    };
    scope
        .repo
        .require_tree(&integration_sha, approved_tree.as_deref())?;
    let attached =
        if let Some(checkout) = attached_checkout.filter(|_| integration_sha != actual_sha) {
            ensure_merge_destination_available(db, &request.project_id, &checkout)?;
            Some(scope.repo.prepare_attached(
                &checkout,
                &request.destination_branch,
                &actual_sha,
                &integration_sha,
            )?)
        } else {
            None
        };
    let intent = MergeIntent {
        source_branch: request.expected_source_branch.clone(),
        source_sha: request.expected_source_sha.clone(),
        destination_branch: request.destination_branch.clone(),
        expected_destination_sha: request.expected_destination_sha.clone(),
        destination_sha: actual_sha,
        drift_acceptance_id: request.drift_acceptance_id.clone(),
        integration_sha,
        attached,
    };
    db.execute(
        "INSERT INTO workflow_merge_intents(commit_evidence_id, payload) VALUES (?1, ?2)
         ON CONFLICT(commit_evidence_id) DO UPDATE SET payload = excluded.payload",
        params![request.commit_evidence_id, encode(&intent)?],
    )?;
    Ok(Preparation::Settle(Some(intent)))
}

fn reusable_attachment(plan: Option<&AttachedMergePlan>, current: Option<&Path>) -> Result<bool> {
    match plan {
        Some(plan) if plan.publication_started()? => Ok(true),
        Some(plan) if current == Some(plan.checkout()) => plan.unstarted_matches_current(),
        Some(_) => Ok(false),
        None => Ok(current.is_none()),
    }
}

fn recovery_outcome(error: anyhow::Error) -> Result<DeliveryMergeOutcome> {
    let recovery = error.downcast::<AttachedRecovery>()?;
    Ok(DeliveryMergeOutcome::RecoveryRequired {
        checkout: recovery.checkout.to_string_lossy().into_owned(),
        recovery_directory: recovery.recovery_directory.to_string_lossy().into_owned(),
        message: recovery.message,
    })
}

fn after_attachment_change(plan: &AttachedMergePlan, error: anyhow::Error) -> anyhow::Error {
    if error.is::<AttachedRecovery>() || !plan.publication_started().unwrap_or(true) {
        return error;
    }
    plan.recovery_error(format!("{error:#}")).into()
}

fn finish_attachment_error(
    intent: Option<&MergeIntent>,
    error: anyhow::Error,
) -> Result<DeliveryMergeOutcome> {
    match intent.and_then(|intent| intent.attached.as_ref()) {
        Some(plan) => recovery_outcome(after_attachment_change(plan, error)),
        None => recovery_outcome(error),
    }
}

fn require_unmodified_attachment(intent: Option<&MergeIntent>, message: &str) -> Result<()> {
    if let Some(plan) = intent.and_then(|intent| intent.attached.as_ref())
        && plan.publication_started()?
    {
        return Err(plan.recovery_error(message).into());
    }
    Ok(())
}

fn verify_drift_approval(
    db: &Connection,
    scope: &MergeScope,
    request: &DeliveryMergeRequest,
    destination_sha: &str,
    acceptance_id: Option<&str>,
) -> Result<Option<String>> {
    if destination_sha == request.expected_destination_sha {
        ensure!(
            acceptance_id.is_none(),
            "the destination no longer matches its prepared update"
        );
        return Ok(None);
    }
    let source = scope
        .commits
        .last()
        .context("delivery has no verified commits")?;
    let preview = authorized_destination_drift(
        db,
        source,
        &request.workflow_run_id,
        &request.destination_branch,
        &request.expected_destination_sha,
        destination_sha,
        acceptance_id.context("destination update requires acceptance")?,
    )?;
    Ok(Some(preview.tree))
}

fn missing_destination(request: &DeliveryMergeRequest) -> DeliveryMergeOutcome {
    DeliveryMergeOutcome::DestinationDrift {
        recovery: DestinationDriftOutcome::DestinationMoved {
            expected_destination_sha: request.expected_destination_sha.clone(),
            actual_sha: None,
        },
    }
}

fn local_merge_allowed(
    db: &Connection,
    request: &DeliveryMergeRequest,
    invocation: Invocation,
) -> Result<bool> {
    let policy = load_policy(db, &request.project_id)?;
    Ok(request.perform
        && match invocation {
            Invocation::User => policy != DeliveryGitPolicy::UserCommitsAndMerges,
            Invocation::Workflow => policy == DeliveryGitPolicy::AutomaticLocalCommitAndMerge,
        })
}

impl MergeIntent {
    fn matches(&self, request: &DeliveryMergeRequest) -> bool {
        self.source_branch == request.expected_source_branch
            && self.source_sha == request.expected_source_sha
            && self.destination_branch == request.destination_branch
            && self.expected_destination_sha == request.expected_destination_sha
    }
}

fn settle_merge<'a>(
    db: &Connection,
    request: &DeliveryMergeRequest,
    intent: Option<&'a MergeIntent>,
    invocation: Invocation,
    attached: &mut Option<AttachedPublication<'a>>,
) -> Result<DeliveryMergeOutcome> {
    if let Some(saved) = find_saved(db, request)? {
        return Ok(saved);
    }
    let scope = MergeScope::load(db, request)?;
    scope.repo.verify_chain(&scope.commits)?;
    let Some(actual_sha) = scope.repo.find_branch_tip(&request.destination_branch)? else {
        require_unmodified_attachment(
            intent,
            "destination disappeared after the worktree update began",
        )?;
        return Ok(missing_destination(request));
    };
    let (integration_sha, source, previous_destination_sha, drift_acceptance_id) = match intent {
        Some(intent) => {
            ensure!(
                load_intent(db, &request.commit_evidence_id)?.as_ref() == Some(intent),
                "prepared merge changed before publication"
            );
            if actual_sha != intent.integration_sha {
                if actual_sha != intent.destination_sha {
                    require_unmodified_attachment(
                        Some(intent),
                        "destination changed after the worktree update began",
                    )?;
                    return Ok(DeliveryMergeOutcome::DestinationMoved { actual_sha });
                }
                if !local_merge_allowed(db, request, invocation)? {
                    require_unmodified_attachment(
                        Some(intent),
                        "local merge is no longer authorized for the pending worktree update",
                    )?;
                    return Ok(DeliveryMergeOutcome::WaitingForMerge);
                }
                let approved_tree = verify_drift_approval(
                    db,
                    &scope,
                    request,
                    &intent.destination_sha,
                    intent.drift_acceptance_id.as_deref(),
                )?;
                scope
                    .repo
                    .require_tree(&intent.integration_sha, approved_tree.as_deref())?;
                scope.repo.verify_incorporation(
                    request,
                    &intent.integration_sha,
                    &scope.commits,
                )?;
                let mut publication = request.clone();
                publication
                    .expected_destination_sha
                    .clone_from(&intent.destination_sha);
                let published = if let Some(plan) = &intent.attached {
                    ensure_merge_destination_available(db, &request.project_id, plan.checkout())?;
                    let hold = scope
                        .repo
                        .reserve_attached(plan, &request.destination_branch)?;
                    *attached = Some(hold);
                    scope.repo.publish_with_worktree(
                        &publication,
                        &intent.integration_sha,
                        attached.as_mut(),
                    )?
                } else {
                    scope.repo.publish(&publication, &intent.integration_sha)?
                };
                if !matches!(&published, Publication::Published) {
                    require_unmodified_attachment(
                        Some(intent),
                        "the expected refs changed after the worktree update began",
                    )?;
                }
                match published {
                    Publication::Published => {}
                    Publication::DestinationMoved(Some(actual_sha)) => {
                        return Ok(DeliveryMergeOutcome::DestinationMoved { actual_sha });
                    }
                    Publication::DestinationMoved(None) => return Ok(missing_destination(request)),
                    Publication::DestinationCheckedOut(checkout) => {
                        return Ok(DeliveryMergeOutcome::DestinationCheckedOut { checkout });
                    }
                }
            } else if let Some(plan) = &intent.attached {
                ensure_merge_destination_available(db, &request.project_id, plan.checkout())?;
                *attached = Some(
                    scope
                        .repo
                        .reserve_attached(plan, &request.destination_branch)?,
                );
            }
            (
                intent.integration_sha.clone(),
                MergeSource::Automatic,
                intent.destination_sha.clone(),
                intent.drift_acceptance_id.clone(),
            )
        }
        None => {
            let sha = request
                .external_sha
                .as_ref()
                .context("no proposed Git integration result")?;
            if *sha != actual_sha {
                return Ok(DeliveryMergeOutcome::DestinationMoved { actual_sha });
            }
            let previous = if let Some(acceptance_id) = request.drift_acceptance_id.as_deref() {
                let parent = scope.repo.external_update_parent(request, sha)?;
                let tree =
                    verify_drift_approval(db, &scope, request, &parent, Some(acceptance_id))?;
                scope.repo.require_tree(sha, tree.as_deref())?;
                parent
            } else {
                scope.repo.verify_external(request, sha)?;
                request.expected_destination_sha.clone()
            };
            (
                sha.clone(),
                MergeSource::External,
                previous,
                request.drift_acceptance_id.clone(),
            )
        }
    };
    scope
        .repo
        .verify_incorporation(request, &integration_sha, &scope.commits)?;
    let Some(actual_sha) = scope.repo.find_branch_tip(&request.destination_branch)? else {
        if let Some(plan) = intent.and_then(|intent| intent.attached.as_ref()) {
            return Err(plan
                .recovery_error("destination disappeared before recording the integration")
                .into());
        }
        return Ok(missing_destination(request));
    };
    if actual_sha != integration_sha {
        if let Some(plan) = intent.and_then(|intent| intent.attached.as_ref()) {
            return Err(plan
                .recovery_error("destination moved before recording the integration")
                .into());
        }
        return Ok(DeliveryMergeOutcome::DestinationMoved { actual_sha });
    }
    if let Some(held) = attached.as_ref()
        && let Some(plan) = intent.and_then(|intent| intent.attached.as_ref())
    {
        let revalidated = (|| {
            ensure!(
                scope.repo.branch_tip(&request.expected_source_branch)?
                    == request.expected_source_sha,
                "approved working branch moved before recording the integration"
            );
            scope
                .repo
                .verify_attached_updated(held, &request.destination_branch)
        })();
        revalidated.map_err(|error| after_attachment_change(plan, error))?;
    }
    let verified_at = db.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
        row.get(0)
    })?;
    let evidence = VerifiedMerge {
        id: Uuid::new_v4().to_string(),
        project_id: request.project_id.clone(),
        task_id: request.task_id.clone(),
        workflow_run_id: request.workflow_run_id.clone(),
        unit_id: request.unit_id.clone(),
        commit_evidence_id: request.commit_evidence_id.clone(),
        commit_evidence_ids: scope.commits.into_iter().map(|commit| commit.id).collect(),
        source_branch: request.expected_source_branch.clone(),
        source_sha: request.expected_source_sha.clone(),
        destination_branch: request.destination_branch.clone(),
        original_destination_sha: (previous_destination_sha != request.expected_destination_sha)
            .then(|| request.expected_destination_sha.clone()),
        drift_acceptance_id,
        previous_destination_sha,
        integration_sha,
        verified_at,
        source,
    };
    let origin = pipeline_work_origin(db, &evidence.project_id, &evidence.workflow_run_id)?;
    let states = WorkStateChanges::capture(db, &evidence.project_id, &evidence.task_id)?;
    db.execute("INSERT INTO workflow_verified_merges(commit_evidence_id, id, project_id, task_id, workflow_run_id, verified_at, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![evidence.commit_evidence_id, evidence.id, evidence.project_id, evidence.task_id, evidence.workflow_run_id, evidence.verified_at, encode(&evidence)?])?;
    record_verified_task_merge(db, &evidence)?;
    states.publish(db, &evidence.project_id, &origin)?;
    Ok(DeliveryMergeOutcome::Verified { evidence })
}

pub(crate) fn load_verified_delivery_merge(
    db: &Connection,
    project: &str,
    task: &str,
    commit_evidence_id: &str,
) -> Result<Option<VerifiedMerge>> {
    let payload: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM workflow_verified_merges WHERE commit_evidence_id = ?1 AND project_id = ?2 AND task_id = ?3",
        params![commit_evidence_id, project, task], |row| row.get(0),
    ).optional()?;
    payload.map(|data| decode(&data)).transpose()
}

#[cfg(test)]
impl Store {
    pub(crate) async fn suspend_test_attached_merge(
        &self,
        request: DeliveryMergeRequest,
        publish_ref: bool,
    ) -> Result<String> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let preparation = prepare_merge(&tx, &request, Invocation::User, None)?;
            tx.commit()?;
            let Preparation::Settle(Some(intent)) = preparation else {
                anyhow::bail!("test did not prepare an integration intent");
            };
            let plan = intent
                .attached
                .as_ref()
                .context("test destination is not attached")?;
            let scope = MergeScope::load(db, &request)?;
            let mut attached = scope
                .repo
                .reserve_attached(plan, &request.destination_branch)?;
            if publish_ref {
                ensure!(
                    matches!(
                        scope.repo.publish_with_worktree(
                            &request,
                            &intent.integration_sha,
                            Some(&mut attached)
                        )?,
                        Publication::Published
                    ),
                    "test did not publish the integration ref"
                );
            } else {
                // Reproduce an exit after updating the checkout and index,
                // before committing the held Git ref transaction.
                attached.apply_test_worktree()?;
            }
            Ok(intent.integration_sha)
        })
        .await
    }
}

fn load_intent(db: &Connection, commit_evidence_id: &str) -> Result<Option<MergeIntent>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_merge_intents WHERE commit_evidence_id = ?1",
            [commit_evidence_id],
            |row| row.get(0),
        )
        .optional()?;
    payload.map(|data| decode(&data)).transpose()
}

fn load_preview(db: &Connection, commit_evidence_id: &str) -> Result<Option<DeliveryMergePreview>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM delivery_merge_previews WHERE commit_evidence_id = ?1",
            [commit_evidence_id],
            |row| row.get(0),
        )
        .optional()?;
    payload.map(|data| decode(&data)).transpose()
}

fn encode(payload: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(payload)?)?)
}

fn decode<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}
