use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{
    repository::{AttemptWorktreeEvidence, verification::SelectedCandidate},
    store::Store,
};

use super::{
    ArtifactValue, BuiltinNode, NodeInputs, ScopedEvidence, TaskReference, WorkflowArtifact,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitInspection {
    pub candidate: SelectedCandidate,
    pub captured_head: String,
    pub status: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffReport {
    pub git_inspection_artifact_id: String,
    pub candidate_id: String,
    pub review_run_id: String,
    pub checks_id: String,
    pub required_checks_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitSelection {
    pub task_id: String,
    pub candidate_id: String,
    pub expected_base: String,
    pub expected_tree: String,
}

impl Store {
    pub async fn workflow_diff_candidates(
        &self,
        project: String,
        run: String,
        node: String,
    ) -> Result<Vec<SelectedCandidate>> {
        let running = self
            .workflow_run(project.clone(), run.clone())
            .await?
            .context("The workflow is unavailable in this project")?;
        let graph = self
            .workflow_definition(project.clone(), running.version)
            .await?
            .context("The pinned diff workflow is unavailable")?;
        ensure!(
            graph
                .nodes
                .iter()
                .any(|entry| entry.instance.id == node
                    && entry.instance.kind == BuiltinNode::ReadGit),
            "Select the pinned Read Git stage"
        );
        let wire = graph
            .wires
            .iter()
            .find(|wire| wire.to.node == node && wire.to.port == "document")
            .context("The Read Git stage has no registered document input")?;
        let current = self
            .current_workflow_host_outputs(project.clone(), run.clone())
            .await?;
        let Some(source) = current.get(&wire.from.node) else {
            return Ok(vec![]);
        };
        let (_, inputs) = self
            .workflow_execution_inputs(
                &project,
                &run,
                &node,
                None,
                &NodeInputs::from([("document".into(), source.clone())]),
            )
            .await?;
        let ArtifactValue::Markdown(document) = &inputs["document"].value else {
            anyhow::bail!("The pinned Read Git input has no registered checkout");
        };
        let checkout = document.checkout_id.clone();
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT s.task_id, s.id FROM selected_candidate_revisions s
                 JOIN job_worktree_starts w ON w.run_id = s.capture_run_id
                 JOIN job_worktree_candidates c ON c.run_id = s.capture_run_id
                 JOIN runs r ON r.id = s.capture_run_id
                 WHERE s.project_id = ?1 AND w.checkout_id = ?2 AND c.capture_confirmed
                   AND NOT EXISTS (SELECT 1 FROM history_deleted_candidate_diffs h WHERE h.candidate_id = s.id)
                 ORDER BY r.created_at DESC, s.id LIMIT 100",
            )?;
            let selected = query.query_map(params![project, checkout], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            selected.map(|row| {
                let (task, id) = row?;
                crate::repository::verification::load_selected_candidate(db, &project, &task, &id)
            }).collect()
        }).await
    }
}

pub(in crate::workflows) async fn capture(
    store: &Store,
    project: &str,
    checkout: &str,
    selection: GitSelection,
) -> Result<GitInspection> {
    let candidate = store
        .selected_candidate(
            project.into(),
            selection.task_id.clone(),
            selection.candidate_id,
        )
        .await?
        .context("The selected candidate is unavailable in the requested work scope")?;
    ensure!(
        candidate.checkout_id == checkout
            && candidate.base_commit == selection.expected_base
            && candidate.tree == selection.expected_tree,
        "The Git selection or registered workflow checkout changed"
    );
    let Some(AttemptWorktreeEvidence::Saved { artifact }) = store
        .attempt_worktree_evidence(project.into(), candidate.target.run_id.clone())
        .await?
    else {
        anyhow::bail!("The selected candidate has no frozen worktree capture");
    };
    ensure!(
        artifact.revision == candidate.target.capture_revision
            && artifact.start.worktree.checkout_id == checkout,
        "The saved worktree capture differs from the selected revision"
    );
    let materialized = store
        .materialize_selected_candidate(project.into(), selection.task_id, candidate.id.clone())
        .await?;
    drop(materialized);
    Ok(GitInspection {
        candidate,
        captured_head: artifact.candidate.head,
        status: artifact.candidate.status,
    })
}

pub(in crate::workflows) fn inspection(artifact: &WorkflowArtifact) -> Result<&GitInspection> {
    let ArtifactValue::GitInspection(git) = &artifact.value else {
        anyhow::bail!("The stage requires the frozen Git inspection");
    };
    Ok(git)
}

pub(in crate::workflows) async fn verify(
    store: &Store,
    project: &str,
    git: &WorkflowArtifact,
    required: Vec<String>,
    parallel_groups: Vec<Vec<String>>,
) -> Result<ScopedEvidence> {
    let candidate = &inspection(git)?.candidate;
    let checks = store
        .verify_frozen_selected_candidate(
            project.into(),
            candidate.clone(),
            required,
            parallel_groups,
        )
        .await?;
    Ok(ScopedEvidence {
        task: TaskReference {
            project_id: project.into(),
            task_id: candidate.target.task_id.clone(),
        },
        id: checks.id,
    })
}

pub(in crate::workflows) async fn report(
    store: &Store,
    project: &str,
    git: &WorkflowArtifact,
    review: &WorkflowArtifact,
    checks: &WorkflowArtifact,
) -> Result<DiffReport> {
    let candidate = &inspection(git)?.candidate;
    let ArtifactValue::Review { run_id } = &review.value else {
        anyhow::bail!("The report requires a saved read-only review");
    };
    let ArtifactValue::Checks(checks) = &checks.value else {
        anyhow::bail!("The report requires the candidate checks");
    };
    ensure!(
        checks.task.project_id == project && checks.task.task_id == candidate.target.task_id,
        "The checks belong to a different work scope"
    );
    let input = store
        .workflow_attempt_input(run_id.clone())
        .await?
        .context("The review attempt has no pinned inputs")?;
    ensure!(
        input.unit_id.is_none() && input.artifacts.get("git") == Some(&git.id),
        "The review did not assess the selected Git inspection"
    );
    let review_run = run_id.clone();
    let frozen = candidate.clone();
    let evidence = checks.id.clone();
    let verified = store
        .execute(move |db| {
            crate::repository::verification::require_candidate_review_dispatch(
                db,
                &review_run,
                &frozen,
            )?;
            crate::repository::verification::load_scoped_candidate_verification(
                db,
                &frozen.target.project_id,
                &frozen.target.task_id,
                &evidence,
            )?
            .context("The pinned verification receipt is unavailable")
        })
        .await?;
    ensure!(
        verified.candidate_id == candidate.id && verified.finished_at.is_some(),
        "The checks did not complete on the selected Git candidate"
    );
    Ok(DiffReport {
        git_inspection_artifact_id: git.id.clone(),
        candidate_id: candidate.id.clone(),
        review_run_id: run_id.clone(),
        checks_id: checks.id.clone(),
        required_checks_passed: verified.allows_acceptance(),
    })
}
