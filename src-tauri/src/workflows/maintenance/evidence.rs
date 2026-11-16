use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::repository::{
    verification::{
        CandidateGateOutcome, load_scoped_candidate_acceptance, require_candidate_review_dispatch,
    },
    verified_git::{VerifiedCommit, load_verified_delivery_commit},
};

use crate::workflows::{
    ApprovalDecisionSource, ArtifactKind, ArtifactValue, BuiltinNode, Executor, NodeInputs,
    WorkflowArtifact, WorkflowRevision,
    approval::workflow_release_authorized,
    artifacts::load_artifact,
    control,
    job_inputs::load_attempt_input,
    runtime::{load_definition, load_pinned_node, load_workflow_run},
    units::{load_delivery_units, validate_unit_capture},
};

use super::{
    MaintenanceHandoff, MaintenanceHandoffRequest, MaintenanceSettings, load_settings,
    package::BuiltPackage, validate_activation_wires,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CandidateEvidence {
    pub settings: MaintenanceSettings,
    pub git_common_dir: PathBuf,
    pub commit: VerifiedCommit,
    pub git_artifact: WorkflowArtifact,
    workflow: WorkflowRevision,
    workflow_run_id: String,
    node_id: String,
    plan_revision: String,
    approval_artifact_id: String,
    approval_source: ApprovalDecisionSource,
    acceptance_artifact_id: String,
    review_artifact_id: String,
    review_run_id: String,
}

impl CandidateEvidence {
    pub(super) fn handoff(
        &self,
        id: String,
        recorded_at: String,
        build: &BuiltPackage,
    ) -> MaintenanceHandoff {
        MaintenanceHandoff {
            id,
            project_id: self.commit.project_id.clone(),
            workflow_run_id: self.workflow_run_id.clone(),
            workflow: self.workflow.clone(),
            node_id: self.node_id.clone(),
            git_artifact_id: self.git_artifact.id.clone(),
            task_id: self.commit.task_id.clone(),
            unit_id: self.commit.unit_id.clone(),
            plan_revision: self.plan_revision.clone(),
            settings_revision: self.settings.revision.clone(),
            version: build.version.clone(),
            source_revision: self.commit.sha.clone(),
            source_tree: self.commit.tree.clone(),
            candidate_revision: self.commit.candidate_revision.clone(),
            sha256: build.sha256.clone(),
            size: build.size,
            package_name: build.package_name.clone(),
            commit_evidence_id: self.commit.id.clone(),
            verification_id: self.commit.verification_id.clone(),
            checks: self.settings.configuration.required_checks.clone(),
            approval_artifact_id: self.approval_artifact_id.clone(),
            approval_source: self.approval_source,
            acceptance_artifact_id: self.acceptance_artifact_id.clone(),
            review_artifact_id: self.review_artifact_id.clone(),
            review_run_id: self.review_run_id.clone(),
            build_executable_revision: self.settings.build_executable_revision.clone(),
            recorded_at,
            activation: None,
        }
    }
}

pub(super) fn resolve(
    db: &Connection,
    request: &MaintenanceHandoffRequest,
) -> Result<CandidateEvidence> {
    let (project, task, run) = (
        &request.project_id,
        &request.task_id,
        &request.workflow_run_id,
    );
    let settings =
        load_settings(db, project)?.context("Register this source repository for maintenance")?;
    ensure!(
        settings.revision == request.expected_settings_revision,
        "Maintenance settings changed"
    );
    let workflow =
        load_workflow_run(db, project, run)?.context("Maintenance workflow run is unavailable")?;
    ensure!(
        workflow.version.id == settings.configuration.workflow_id,
        "This run is outside the configured maintenance workflow"
    );
    let units = load_delivery_units(db, project, task, run)?;
    let unit = units
        .last()
        .context("No active Task delivery is linked to this maintenance run")?;
    ensure!(
        unit.commit_evidence_id.as_ref() == Some(&request.commit_evidence_id)
            && units.iter().all(|unit| unit.commit_evidence_id.is_some()),
        "A handoff requires the final verified commit of the approved Task delivery"
    );
    let commit = load_verified_delivery_commit(db, project, task, &request.commit_evidence_id)?
        .context("Verified candidate commit is unavailable")?;
    ensure!(
        commit.unit_id == unit.unit.id && commit.checkout_id == unit.checkout_id,
        "Verified commit belongs to a different Task unit or checkout"
    );
    let node = load_pinned_node(db, project, run, &request.node_id)?
        .context("This workflow revision has no selected maintenance activation stage")?;
    ensure!(
        node.definition.executor == Executor::ActivateMaintenanceFixture,
        "Select the pinned maintenance activation stage"
    );
    let definition = load_definition(db, project, &workflow.version)?
        .context("Maintenance workflow revision is unavailable")?;
    validate_activation_wires(&definition.nodes, &definition.wires)?;
    let git_wire = definition
        .wires
        .iter()
        .find(|wire| wire.to.node == request.node_id && wire.to.port == "git")
        .context("Maintenance activation has no pinned commit wire")?;
    let git_artifact = load_artifact(
        db,
        project,
        run,
        &request.git_artifact_id,
        ArtifactKind::Git,
    )?
    .context("The selected commit artifact is unavailable for this maintenance run")?;
    ensure!(
        git_artifact.node_instance_id == git_wire.from.node
            && git_artifact.port == git_wire.from.port
            && git_artifact.unit_id.as_deref() == Some(unit.unit.id.as_str())
            && matches!(&git_artifact.value, ArtifactValue::Git(reference)
                if reference.id == commit.id && reference.task.project_id == *project
                    && reference.task.task_id == *task),
        "Maintenance activation requires the pinned Verify commit output for the exact accepted candidate"
    );
    control::require_dependencies(
        db,
        &definition,
        &workflow,
        &request.node_id,
        Some(&unit.unit.id),
        &NodeInputs::from([("git".into(), git_artifact.id.clone())]),
    )?;
    validate_unit_capture(db, unit, &commit.capture_run_id)?;
    let (release_id, approval_artifact_id, git_common_dir): (String, String, String) = db
        .query_row(
            "SELECT release.id, binding.decision_artifact_id, p.git_common_dir
         FROM job_attempts capture
         JOIN job_pipeline_origins origin ON origin.job_id = capture.job_id
         JOIN job_pipeline_releases release ON release.id = origin.release_id
         JOIN workflow_release_bindings binding ON binding.release_id = release.id
         JOIN projects p ON p.id = release.project_id
         WHERE capture.run_id = ?1 AND release.project_id = ?2 AND release.pipeline_run_id = ?3
           AND release.task_id = ?4 AND release.plan_revision = ?5 AND release.checkout_id = ?6
           AND release.revoked = 0",
            params![
                commit.capture_run_id,
                project,
                run,
                task,
                unit.plan_revision,
                unit.checkout_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .context("The exact candidate has no active implementation release")?;
    ensure!(
        workflow_release_authorized(db, project, run, &release_id)? == Some(true),
        "Implementation approval for this candidate was superseded"
    );
    let approval = load_artifact(
        db,
        project,
        run,
        &approval_artifact_id,
        ArtifactKind::Approval,
    )?
    .context("Implementation approval evidence is unavailable")?;
    let ArtifactValue::Approval(approval) = approval.value else {
        anyhow::bail!("The release requires an implementation approval");
    };
    let approval_node = load_pinned_node(db, project, run, &approval.node_instance_id)?
        .context("Implementation approval node is unavailable")?;
    ensure!(
        approval_node.definition.executor == Executor::RecordImplementationRelease
            && approval.source_checkout_id == settings.configuration.source_checkout_id,
        "Approval must release work from the registered maintenance source checkout"
    );
    let acceptance_artifact_id = commit
        .acceptance_artifact_id
        .clone()
        .context("The verified commit has no workflow candidate acceptance")?;
    super::super::unit_execution::validate_unit_acceptance(
        db,
        unit,
        &commit.acceptance_id,
        &acceptance_artifact_id,
    )?;
    let acceptance_artifact = load_artifact(
        db,
        project,
        run,
        &acceptance_artifact_id,
        ArtifactKind::Acceptance,
    )?
    .context("Workflow candidate acceptance is unavailable")?;
    let acceptance_node =
        load_pinned_node(db, project, run, &acceptance_artifact.node_instance_id)?
            .context("Candidate acceptance node is unavailable")?;
    ensure!(
        acceptance_node.definition.executor == Executor::RecordCandidateAcceptance,
        "Candidate acceptance must come from its pinned workflow node"
    );
    let ArtifactValue::Acceptance(reference) = acceptance_artifact.value else {
        anyhow::bail!("Candidate acceptance artifact has the wrong evidence type");
    };
    let acceptance = load_scoped_candidate_acceptance(db, project, task, &commit.acceptance_id)?
        .context("Selected candidate acceptance is unavailable")?;
    let candidate = &acceptance.candidate;
    let verification = &acceptance.verification;
    ensure!(
        candidate.id == commit.candidate_revision
            && candidate.target.unit_id == unit.unit.id
            && candidate.target.run_id == commit.capture_run_id
            && candidate.checkout_id == commit.checkout_id
            && candidate.tree == commit.tree
            && candidate.base_commit == commit.base_commit
            && verification.id == commit.verification_id
            && verification.candidate_id == candidate.id
            && verification.allows_acceptance(),
        "The verified commit must match the exact accepted and tested candidate"
    );
    for name in &settings.configuration.required_checks {
        ensure!(
            verification.gates.iter().any(|gate| gate.gate.name == *name
                && matches!(gate.outcome, Some(CandidateGateOutcome::Passed { .. }))),
            "The exact candidate is missing a passing check named {name}"
        );
    }
    let review = load_artifact(
        db,
        project,
        run,
        &reference.review_artifact_id,
        ArtifactKind::Review,
    )?
    .context("Accepted candidate review is unavailable")?;
    let review_node = load_pinned_node(db, project, run, &review.node_instance_id)?
        .context("Candidate review node is unavailable")?;
    let ArtifactValue::Review {
        run_id: review_run_id,
    } = review.value
    else {
        anyhow::bail!("The accepted candidate requires its workflow review");
    };
    let review_input = load_attempt_input(db, &review_run_id)?
        .context("Candidate review inputs are unavailable")?;
    ensure!(
        review_node.instance.kind == BuiltinNode::ReviewCandidate
            && review.unit_id.as_deref() == Some(unit.unit.id.as_str())
            && review_input.unit_id.as_deref() == Some(unit.unit.id.as_str()),
        "Review does not belong to the accepted candidate's logical unit"
    );
    require_candidate_review_dispatch(db, &review_run_id, candidate)?;
    Ok(CandidateEvidence {
        settings,
        git_common_dir: git_common_dir.into(),
        commit,
        git_artifact,
        workflow: workflow.version,
        workflow_run_id: workflow.id,
        node_id: request.node_id.clone(),
        plan_revision: unit.plan_revision.clone(),
        approval_artifact_id,
        approval_source: approval.source,
        acceptance_artifact_id,
        review_artifact_id: reference.review_artifact_id,
        review_run_id,
    })
}
