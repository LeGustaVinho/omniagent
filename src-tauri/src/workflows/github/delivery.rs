use std::{collections::BTreeSet, path::PathBuf};

pub(super) mod source;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Deserialize;

use crate::{
    repository::{
        github::{
            CiSnapshot, CiState, MergeOutcome, PullRequest, PullRequestRequest, RemoteMergeProof,
        },
        registered_checkout_head,
        verified_git::load_verified_delivery_commit,
        verified_merge::{
            DeliveryMergeRequest, SourceSync, github_delivery_paths, github_effective_paths,
            record_github_delivery_merge,
        },
    },
    store::Store,
    tasks::delivery_merge_candidate,
};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, CiEvidence, GithubDeliveryClient, GithubSettings,
    PendingGithubAction, PullEvidence, WorkflowArtifact, delivery_pin, digest, load_pinned_node,
    open,
    policy::{authorize, checkout_path, load_settings, require_settings},
    seal,
};
use crate::workflows::{
    artifacts::{TaskReference, WorkflowMergeReference, load_artifact},
    decision_nodes::{WorkflowDecisionRequest, WorkflowPendingDecision, WorkflowStageDecision},
    units::load_delivery_unit,
};

struct Delivery {
    revision: String,
    settings: GithubSettings,
    checkout: String,
    path: PathBuf,
    merge: DeliveryMergeRequest,
    git_artifact: String,
    human: bool,
    changed_paths: Vec<String>,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewRisk {
    high_risk: bool,
}

impl PendingGithubAction {
    fn require_native_receipt(&self, db: &Connection, evidence: &WorkflowArtifact) -> Result<()> {
        let recorded: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_github_actions
             WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND unit_key = ?3
               AND node_instance_id = ?4 AND output_id = ?5 AND port IN ('completed', 'passed', 'approval_required'))",
            params![self.run, self.plan, self.unit, evidence.node_instance_id, evidence.id],
            |row| row.get(0),
        )?;
        ensure!(
            recorded,
            "GitHub delivery requires the native receipt of this plan and unit"
        );
        Ok(())
    }

    fn delivery(
        &self,
        db: &Connection,
        git_artifact: &str,
        base: &str,
        review_high: bool,
    ) -> Result<Delivery> {
        let unit_id = self
            .unit()
            .context("GitHub delivery needs an approved logical unit")?;
        let artifact = load_artifact(
            db,
            &self.project,
            &self.run,
            git_artifact,
            ArtifactKind::Git,
        )?
        .context("The verified Git artifact is unavailable")?;
        ensure!(
            artifact.unit_id.as_deref() == Some(unit_id),
            "The Git artifact belongs to another logical unit"
        );
        let source = load_pinned_node(db, &self.project, &self.run, &artifact.node_instance_id)?
            .context("The verified Git artifact lost its native stage")?;
        ensure!(
            source.instance.kind == BuiltinNode::VerifyCommit,
            "GitHub needs a native verified commit"
        );
        let ArtifactValue::Git(git) = artifact.value else {
            unreachable!()
        };
        ensure!(
            git.task.project_id == self.project,
            "The Git artifact belongs to another project"
        );
        let task = git.task.task_id;
        let unit = load_delivery_unit(db, &self.project, &task, unit_id)?
            .context("The GitHub action needs the current verified delivery unit")?;
        ensure!(
            unit.workflow_run_id == self.run
                && unit.plan_revision == self.plan
                && unit.commit_evidence_id.as_ref() == Some(&git.id),
            "The active unit and commit pins changed"
        );
        let commit = load_verified_delivery_commit(db, &self.project, &task, &git.id)?
            .context("The approved Git commit no longer exists")?;
        ensure!(
            commit.checkout_id == unit.checkout_id && commit.unit_id == unit_id,
            "The verified commit belongs to a different unit or checkout"
        );
        let candidate = delivery_merge_candidate(db, &self.project, &task, &self.run)?
            .context("GitHub publishing requires the final verified delivery commit chain")?;
        ensure!(
            candidate.commit_evidence_id == commit.id && candidate.plan_revision == self.plan,
            "GitHub publishing requires the final verified delivery unit"
        );
        let (revision, settings) = load_settings(db, &self.project)?
            .context("Configure the project's GitHub delivery policy")?;
        if let Some((pinned, destination)) = delivery_pin(db, &self.run, &self.plan)? {
            ensure!(
                pinned == revision && destination == settings.destination_branch,
                "Configure a new approved plan after changing the GitHub delivery policy"
            );
        }
        let merge = DeliveryMergeRequest {
            project_id: self.project.clone(),
            task_id: task,
            workflow_run_id: self.run.clone(),
            unit_id: unit_id.to_owned(),
            commit_evidence_id: commit.id,
            expected_source_branch: commit.branch,
            expected_source_sha: commit.sha,
            destination_branch: settings.destination_branch.clone(),
            expected_destination_sha: base.to_owned(),
            external_sha: None,
            drift_acceptance_id: None,
            prepare_drift: false,
            perform: false,
        };
        let (previous_high, previous_paths) =
            self.previous_pull_risk(db, &merge, settings.repository_id)?;
        let changed_paths = github_delivery_paths(db, &merge)?
            .into_iter()
            .chain(previous_paths)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let human = settings.requires_human(&changed_paths, review_high || previous_high);
        let path = checkout_path(db, &self.project, &unit.checkout_id)?;
        let purpose = unit.unit.purpose.trim();
        let first_line = purpose
            .lines()
            .next()
            .context("The verified delivery has no purpose")?;
        let title = first_line
            .char_indices()
            .take_while(|(offset, character)| *offset + character.len_utf8() <= 240)
            .map(|(_, character)| character)
            .collect();
        Ok(Delivery {
            revision,
            settings,
            checkout: unit.checkout_id,
            path,
            merge,
            git_artifact: git_artifact.into(),
            human,
            changed_paths,
            title,
        })
    }

    fn previous_pull_risk(
        &self,
        db: &Connection,
        merge: &DeliveryMergeRequest,
        repository: u64,
    ) -> Result<(bool, BTreeSet<String>)> {
        let mut query = db.prepare(
            "SELECT artifact.id FROM workflow_github_actions action
             JOIN workflow_artifacts artifact ON artifact.id = action.output_id
             WHERE action.pipeline_run_id = ?1 AND action.port = 'completed' AND artifact.port_name = 'pull'",
        )?;
        let ids = query.query_map([&self.run], |row| row.get::<_, String>(0))?;
        let mut escalated = false;
        let mut paths = BTreeSet::new();
        for id in ids {
            let artifact = load_artifact(
                db,
                &self.project,
                &self.run,
                &id?,
                ArtifactKind::GithubPullRequest,
            )?
            .context("The delivery lost a prior pull request receipt")?;
            let ArtifactValue::GithubPullRequest(evidence) = artifact.value else {
                unreachable!()
            };
            if evidence.task_id == merge.task_id
                && evidence.pull.repository_id == repository
                && evidence.pull.head_branch == merge.expected_source_branch
                && evidence.pull.base_branch == merge.destination_branch
            {
                escalated |= evidence.requires_human;
                paths.extend(evidence.changed_paths);
            }
        }
        Ok((escalated, paths))
    }

    fn for_pull(&self, db: &Connection, evidence: &PullEvidence) -> Result<Delivery> {
        let mut delivery = self.delivery(
            db,
            &evidence.git_artifact_id,
            &evidence.pull.base_sha,
            evidence.requires_human,
        )?;
        source::validate_published_pull_source(
            db,
            &self.project,
            &self.run,
            &self.plan,
            evidence,
            &delivery.merge,
        )?;
        delivery.include_source_paths(db, &evidence.source_syncs)?;
        ensure!(
            delivery.revision == evidence.policy_revision
                && delivery.checkout == evidence.checkout_id
                && delivery.merge.task_id == evidence.task_id
                && delivery.merge.commit_evidence_id == evidence.commit_evidence_id
                && delivery.human == evidence.requires_human
                && delivery.matches_refs(&evidence.pull),
            "GitHub evidence no longer matches the active delivery pins"
        );
        Ok(delivery)
    }

    pub(super) async fn publish_pull(
        &self,
        store: &Store,
        gh: &impl GithubDeliveryClient,
    ) -> Result<()> {
        let action = self.clone();
        let mut delivery = store
            .execute(move |db| {
                action.validate(db)?;
                let git = action
                    .inputs
                    .get("git")
                    .context("GitHub publishing needs a verified Git input")?;
                let high = action
                    .inputs
                    .get("risk")
                    .map(|_| {
                        let ArtifactValue::JsonObject(risk) =
                            action.input(db, "risk", ArtifactKind::JsonObject)?.value
                        else {
                            unreachable!()
                        };
                        let risk: ReviewRisk =
                            serde_json::from_value(serde_json::Value::Object(risk))?;
                        Ok::<_, anyhow::Error>(risk.high_risk)
                    })
                    .transpose()?
                    .unwrap_or(false);
                let delivery = action.delivery(db, git, "", high)?;
                ensure!(
                    registered_checkout_head(db, &action.project, &delivery.checkout)?
                        == delivery.merge.expected_source_sha,
                    "The registered delivery checkout moved after commit verification"
                );
                Ok(delivery)
            })
            .await?;
        let slug = delivery.settings.slug();
        authorize(gh, &delivery.settings, &delivery.path).await?;
        let head = delivery.merge.expected_source_branch.clone();
        let base = delivery.merge.destination_branch.clone();
        let destination = gh.branch_head(&slug, &base).await?;
        gh.fetch_branch(&slug, &base, &destination, &delivery.path)
            .await?;
        delivery.merge.expected_destination_sha = destination.clone();
        let task_key = digest(&serde_json::to_vec(&(
            &self.project,
            &delivery.merge.task_id,
        ))?);
        if let Some(existing) = gh.find_pull_request(&slug, &head, &base, &task_key).await? {
            ensure!(
                existing.repository_id == delivery.settings.repository_id,
                "The existing pull request belongs to a different repository"
            );
            if !existing.open {
                return self
                    .complete(
                        store,
                        "failed",
                        None,
                        Some(
                            "The task's GitHub pull request closed before CI evidence was recorded"
                                .into(),
                        ),
                    )
                    .await;
            }
        }
        let observed = gh.branch_head_optional(&slug, &head).await?;

        let action = self.clone();
        let prepared = store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                action.validate(&tx)?;
                require_settings(&tx, &action.project, &delivery.revision)?;
                ensure!(
                    registered_checkout_head(&tx, &action.project, &delivery.checkout)?
                        == delivery.merge.expected_source_sha,
                    "The registered delivery checkout moved before source synchronization"
                );
                let Some(publication) =
                    source::prepare(&tx, &delivery, &action.plan, observed.as_deref())?
                else {
                    return Ok(None);
                };
                delivery.include_source_paths(&tx, &publication.source_syncs)?;
                tx.commit()?;
                Ok(Some((delivery, publication)))
            })
            .await?;
        let Some((delivery, publication)) = prepared else {
            return self.complete(store, "failed", None, Some(
                "The verified GitHub source and destination conflict. Resolve the conflict in a new approved delivery revision.".into()
            )).await;
        };
        if gh.branch_head(&slug, &base).await? != destination {
            return self
                .pending(
                    store,
                    "The GitHub destination advanced before branch publication",
                    None,
                )
                .await;
        }
        gh.publish_branch(
            &slug,
            &head,
            &publication.target,
            publication.previous_head(),
            &delivery.path,
        )
        .await?;

        let action = self.clone();
        let revision = delivery.revision.clone();
        let publication = store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                action.validate(&tx)?;
                require_settings(&tx, &action.project, &revision)?;
                publication.acknowledge(&tx)?;
                tx.commit()?;
                Ok(publication)
            })
            .await?;

        let request = PullRequestRequest {
            head_branch: head.clone(),
            head_sha: publication.target.clone(),
            base_branch: base.clone(),
            task_key,
            title: delivery.title.clone(),
            body: format!(
                "OmniAgent verified delivery commit {}.",
                delivery.merge.expected_source_sha
            ),
        };
        let pull = gh.open_or_update_pull_request(&slug, &request).await?;
        ensure!(
            delivery.matches_refs(&pull) && pull.head_sha == publication.target,
            "GitHub published different pull request pins"
        );
        if pull.base_sha != destination || gh.branch_head(&slug, &base).await? != destination {
            return self
                .pending(
                    store,
                    "The GitHub destination advanced while publishing the pull request",
                    None,
                )
                .await;
        }
        self.complete(
            store,
            "completed",
            Some((
                "pull",
                ArtifactValue::GithubPullRequest(PullEvidence {
                    policy_revision: delivery.revision,
                    task_id: delivery.merge.task_id,
                    checkout_id: delivery.checkout,
                    commit_evidence_id: delivery.merge.commit_evidence_id,
                    git_artifact_id: delivery.git_artifact,
                    requires_human: delivery.human,
                    changed_paths: delivery.changed_paths,
                    source_syncs: publication.source_syncs,
                    pull,
                }),
            )),
            None,
        )
        .await
    }

    pub(super) async fn wait_for_ci(
        &self,
        store: &Store,
        gh: &impl GithubDeliveryClient,
    ) -> Result<()> {
        let action = self.clone();
        let (id, pull, delivery) = store
            .execute(move |db| {
                action.validate(db)?;
                let artifact = action.input(db, "pull", ArtifactKind::GithubPullRequest)?;
                ensure!(
                    artifact.unit_id.as_deref() == action.unit(),
                    "The pull request belongs to another logical unit"
                );
                action.require_native_receipt(db, &artifact)?;
                let ArtifactValue::GithubPullRequest(pull) = artifact.value else {
                    unreachable!()
                };
                let active = load_settings(db, &action.project)?
                    .is_some_and(|(revision, _)| revision == pull.policy_revision);
                let delivery = active.then(|| action.for_pull(db, &pull)).transpose()?;
                Ok((artifact.id, pull, delivery))
            })
            .await?;
        let Some(delivery) = delivery else {
            return self
                .complete(
                    store,
                    "stale",
                    None,
                    Some("The GitHub delivery policy changed".into()),
                )
                .await;
        };
        let slug = delivery.settings.slug();
        authorize(gh, &delivery.settings, &delivery.path).await?;
        let current = gh.pull_request(&slug, pull.pull.number).await?;
        let destination = gh.branch_head(&slug, &pull.pull.base_branch).await?;
        if !same_pull(&pull.pull, &current) || destination != pull.pull.base_sha {
            return self
                .complete(
                    store,
                    "stale",
                    None,
                    Some("The GitHub pull request head or base changed".into()),
                )
                .await;
        }
        if !current.open || current.draft {
            return self
                .complete(
                    store,
                    "failed",
                    None,
                    Some("The pinned pull request must be open and ready for review".into()),
                )
                .await;
        }
        let snapshot = gh
            .inspect_ci(&slug, &current, &delivery.settings.required_checks)
            .await?;
        let reason = snapshot.reason.clone();
        let port = match snapshot.state {
            CiState::Pending | CiState::Unavailable => {
                return self
                    .pending(
                        store,
                        reason
                            .as_deref()
                            .unwrap_or("Waiting for checks for the pinned GitHub revision"),
                        None,
                    )
                    .await;
            }
            CiState::Failed => "failed",
            CiState::Passed if pull.requires_human => "approval_required",
            CiState::Passed => "passed",
        };
        ensure!(
            snapshot_matches(&pull.pull, &snapshot),
            "CI evidence has different pull request pins"
        );
        self.complete(
            store,
            port,
            Some((
                "ci",
                ArtifactValue::GithubCi(CiEvidence {
                    pull_artifact_id: id,
                    pull,
                    snapshot,
                }),
            )),
            reason,
        )
        .await
    }

    fn approved(&self, db: &Connection, ci: &WorkflowArtifact, required: bool) -> Result<bool> {
        let Some(id) = self.inputs.get("approval") else {
            return Ok(!required);
        };
        let approval = self.input(db, "approval", ArtifactKind::JsonObject)?;
        if approval.unit_id != ci.unit_id || approval.port != "decision" {
            return Ok(false);
        }
        let Some(source) =
            load_pinned_node(db, &self.project, &self.run, &approval.node_instance_id)?
        else {
            return Ok(false);
        };
        if source.instance.kind != BuiltinNode::Approval {
            return Ok(false);
        }
        let row = db
            .query_row(
                "SELECT request_id, payload, resolution FROM workflow_stage_decisions WHERE
               pipeline_run_id = ?1 AND unit_key = ?2 AND plan_key = ?3 AND artifact_id = ?4
               AND node_instance_id = ?5 AND resolution IS NOT NULL",
                params![self.run, self.unit, self.plan, id, source.instance.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((request_id, payload, resolution)) = row else {
            return Ok(false);
        };
        let original: WorkflowPendingDecision = open(&payload)?;
        let answer: WorkflowStageDecision = open(&resolution)?;
        Ok(original.request_id == request_id
            && original.node_id == source.instance.id
            && original.node_revision == source.revision
            && original.unit_id == ci.unit_id
            && original.plan_revision.as_deref().unwrap_or("") == self.plan
            && original.inputs.get("evidence") == Some(&ci.id)
            && matches!(original.request, WorkflowDecisionRequest::Approval { .. })
            && matches!(answer, WorkflowStageDecision::Approval { approved: true }))
    }

    fn merge_inputs(&self, db: &Connection) -> Result<Option<(CiEvidence, Delivery, bool)>> {
        self.validate(db)?;
        let artifact = self.input(db, "ci", ArtifactKind::GithubCi)?;
        ensure!(
            artifact.unit_id.as_deref() == self.unit(),
            "The CI evidence belongs to another logical unit"
        );
        self.require_native_receipt(db, &artifact)?;
        let ArtifactValue::GithubCi(ci) = &artifact.value else {
            unreachable!()
        };
        ensure!(
            ci.snapshot.state == CiState::Passed
                && snapshot_matches(&ci.pull.pull, &ci.snapshot)
                && ci.snapshot.tested_tree.is_some(),
            "GitHub merging requires a passed CI result for the pinned integration"
        );
        let current = load_artifact(
            db,
            &self.project,
            &self.run,
            &ci.pull_artifact_id,
            ArtifactKind::GithubPullRequest,
        )?
        .context("The selected CI review lost its pull request evidence")?;
        ensure!(
            current.unit_id == artifact.unit_id
                && current.value == ArtifactValue::GithubPullRequest(ci.pull.clone()),
            "The recorded CI pull request did not match its source artifact"
        );
        self.require_native_receipt(db, &current)?;
        let active = load_settings(db, &self.project)?
            .is_some_and(|(revision, _)| revision == ci.pull.policy_revision);
        if !active {
            return Ok(None);
        }
        let delivery = self.for_pull(db, &ci.pull)?;
        ensure!(
            ci.snapshot.required_checks == delivery.settings.required_checks,
            "The CI evidence used different required GitHub checks"
        );
        let approved = self.approved(db, &artifact, ci.pull.requires_human)?;
        Ok(Some((ci.clone(), delivery, approved)))
    }

    pub(super) async fn merge_pull(
        &self,
        store: &Store,
        gh: &impl GithubDeliveryClient,
    ) -> Result<()> {
        let action = self.clone();
        let Some((ci, delivery, approved)) =
            store.execute(move |db| action.merge_inputs(db)).await?
        else {
            return self
                .complete(
                    store,
                    "stale",
                    None,
                    Some("The GitHub delivery policy changed".into()),
                )
                .await;
        };
        if !approved {
            return self
                .complete(
                    store,
                    "failed",
                    None,
                    Some("The exact CI evidence requires a native operator approval".into()),
                )
                .await;
        }
        let slug = delivery.settings.slug();
        authorize(gh, &delivery.settings, &delivery.path).await?;
        let pinned = &ci.pull.pull;
        let current = gh.pull_request(&slug, pinned.number).await?;
        let outcome = if current.merged {
            match gh.verify_merge(&slug, pinned, &ci.snapshot).await? {
                Some(proof) => MergeOutcome::Verifiable(proof),
                None => {
                    return self
                        .pending(
                            store,
                            "Waiting for evidence of the verified GitHub merge",
                            None,
                        )
                        .await;
                }
            }
        } else if let Some(request) = &self.remote_request {
            gh.poll_merge(&slug, pinned, &ci.snapshot, request).await?
        } else {
            let destination = gh.branch_head(&slug, &pinned.base_branch).await?;
            if !same_pull(pinned, &current) || destination != pinned.base_sha {
                return self
                    .complete(
                        store,
                        "stale",
                        None,
                        Some("The approved GitHub head or base changed".into()),
                    )
                    .await;
            }
            if !current.open || current.draft {
                return self
                    .complete(
                        store,
                        "failed",
                        None,
                        Some("The approved pull request is closed or draft".into()),
                    )
                    .await;
            }
            if !delivery.settings.allow_automatic_merge {
                return self
                    .pending(store, "Waiting for a verified merge in GitHub", None)
                    .await;
            }
            let observed = gh
                .inspect_ci(&slug, &current, &delivery.settings.required_checks)
                .await?;
            if matches!(observed.state, CiState::Pending | CiState::Unavailable) {
                return self
                    .pending(
                        store,
                        observed
                            .reason
                            .as_deref()
                            .unwrap_or("Waiting to revalidate the approved GitHub checks"),
                        None,
                    )
                    .await;
            }
            if !same_ci(&ci.snapshot, &observed) {
                return self
                    .complete(
                        store,
                        "stale",
                        None,
                        Some("CI changed after the GitHub merge was approved".into()),
                    )
                    .await;
            }
            let action = self.clone();
            store
                .execute(move |db| {
                    ensure!(
                        action
                            .merge_inputs(db)?
                            .is_some_and(|(_, _, authorized)| authorized),
                        "The GitHub merge authorization changed before submission"
                    );
                    Ok(())
                })
                .await?;
            gh.merge_pull_request(&slug, pinned, &ci.snapshot).await?
        };
        match outcome {
            MergeOutcome::Pending { request_id } => {
                self.pending(store, "GitHub is processing the merge", request_id)
                    .await
            }
            MergeOutcome::Rejected { reason } => {
                let latest = gh.pull_request(&slug, pinned.number).await?;
                if latest.merged {
                    return self
                        .pending(store, "Waiting to verify the completed GitHub merge", None)
                        .await;
                }
                if latest.open
                    && (!same_pull(pinned, &latest)
                        || gh.branch_head(&slug, &pinned.base_branch).await? != pinned.base_sha)
                {
                    return self.complete(
                        store, "stale", None,
                        Some("The GitHub destination or head advanced before the merge completed".into()),
                    ).await;
                }
                self.complete(store, "failed", None, Some(reason.into()))
                    .await
            }
            MergeOutcome::Verifiable(proof) => {
                ensure!(
                    proof.pull_number == pinned.number
                        && proof.head_sha == pinned.head_sha
                        && proof.base_sha == pinned.base_sha
                        && ci.snapshot.tested_tree.as_ref() == Some(&proof.tree_sha),
                    "The GitHub merge proof differs from the pinned pull request and tested tree"
                );
                gh.fetch_branch(
                    &slug,
                    &pinned.base_branch,
                    &proof.destination_sha,
                    &delivery.path,
                )
                .await?;
                self.record_merge(store, proof).await
            }
        }
    }

    async fn record_merge(&self, store: &Store, proof: RemoteMergeProof) -> Result<()> {
        let action = self.clone();
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (ci, mut delivery, approved) = action.merge_inputs(&tx)?
                .context("The GitHub policy changed before recording the verified merge")?;
            ensure!(approved && ci.snapshot.tested_tree.as_ref() == Some(&proof.tree_sha)
                && ci.pull.pull.number == proof.pull_number, "GitHub merge authorization changed before recording");
            delivery.merge.external_sha = Some(proof.merge_sha.clone());
            let evidence = record_github_delivery_merge(&tx, &delivery.merge, &proof, &ci.pull.source_syncs)?;
            tx.execute(
                "INSERT INTO workflow_github_destinations VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, repository_id, branch) DO UPDATE SET observed_tip = excluded.observed_tip",
                params![action.project, delivery.settings.repository_id.to_string(), delivery.merge.destination_branch, proof.destination_sha],
            )?;
            tx.execute(
                "UPDATE workflow_github_actions SET remote_proof = ?6 WHERE pipeline_run_id = ?1
                 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4 AND request_key = ?5",
                params![action.run, action.node, action.unit, action.plan, action.key, seal(&proof)?],
            )?;
            action.write_result(&tx, "merged", Some(("integration", ArtifactValue::Merge(WorkflowMergeReference {
                id: evidence.id, commit_evidence_id: evidence.commit_evidence_id,
                task: TaskReference { project_id: evidence.project_id, task_id: evidence.task_id },
            }))), None)?;
            tx.commit()?;
            Ok(())
        }).await
    }
}

impl Delivery {
    fn matches_refs(&self, pull: &PullRequest) -> bool {
        pull.repository_id == self.settings.repository_id
            && pull.head_branch == self.merge.expected_source_branch
            && pull.base_branch == self.merge.destination_branch
    }

    fn include_source_paths(&mut self, db: &Connection, syncs: &[SourceSync]) -> Result<()> {
        self.changed_paths
            .extend(github_effective_paths(db, &self.merge, syncs)?);
        self.changed_paths.sort_unstable();
        self.changed_paths.dedup();
        self.human = self
            .settings
            .requires_human(&self.changed_paths, self.human);
        Ok(())
    }
}

fn same_pull(pinned: &PullRequest, current: &PullRequest) -> bool {
    pinned.id == current.id
        && pinned.number == current.number
        && pinned.repository_id == current.repository_id
        && pinned.head_branch == current.head_branch
        && pinned.head_sha == current.head_sha
        && pinned.base_branch == current.base_branch
        && pinned.base_sha == current.base_sha
}

fn snapshot_matches(pull: &PullRequest, snapshot: &CiSnapshot) -> bool {
    snapshot.pull_number == pull.number
        && snapshot.head_sha == pull.head_sha
        && snapshot.base_sha == pull.base_sha
}

fn same_ci(approved: &CiSnapshot, current: &CiSnapshot) -> bool {
    current.state == CiState::Passed
        && approved.pull_number == current.pull_number
        && approved.head_sha == current.head_sha
        && approved.base_sha == current.base_sha
        && approved.tested_sha == current.tested_sha
        && approved.tested_tree == current.tested_tree
        && approved.required_checks == current.required_checks
        && approved.checks == current.checks
}
