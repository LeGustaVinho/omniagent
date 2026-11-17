use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    jobs::{
        GoalCriterionEvidence, GoalCriterionStatus, GoalDefinition, GoalVerification,
        GoalVerificationStatus, load_goal_verification, native_goal_candidate,
        require_workflow_goal_release,
    },
    providers::{
        Provider, SessionMode,
        goal::{NativeGoalEvidence, NativeGoalSupport},
    },
    repository::verification::{CandidateVerification, SelectedCandidate, require_current},
    store::Store,
};

use super::{
    ArtifactValue, BuiltinNode, Executor, NodeConfiguration, NodeInstance, PinnedNode,
    authoring::package::{PackageArtifactVersion, roles},
    builtin_registry, digest,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageGoalConfiguration {
    pub objective: String,
    pub criteria: Vec<StageGoalCriterion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageGoalCriterion {
    pub text: String,
    pub evidence: StageGoalEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageGoalEvidence {
    OperatorReview,
    CandidateGate,
}

impl StageGoalConfiguration {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(!self.objective.trim().is_empty(), "Enter a goal objective");
        ensure!(
            !self.criteria.is_empty()
                && self
                    .criteria
                    .iter()
                    .all(|criterion| !criterion.text.trim().is_empty()),
            "A workflow goal needs an evidence requirement for every criterion"
        );
        ensure!(
            self.max_turns.is_some() || self.max_seconds.is_some() || self.token_budget.is_some(),
            "A workflow goal needs at least one execution limit"
        );
        ensure!(
            self.max_turns != Some(0)
                && self.max_seconds != Some(0)
                && self.token_budget != Some(0),
            "Goal limits must be positive"
        );
        ensure!(
            self.token_budget
                .is_none_or(|budget| i64::try_from(budget).is_ok()),
            "The goal token budget exceeds the supported range"
        );
        Ok(())
    }

    pub(crate) fn definition(&self) -> GoalDefinition {
        GoalDefinition {
            objective: self.objective.clone(),
            criteria: self
                .criteria
                .iter()
                .map(|criterion| criterion.text.clone())
                .collect(),
            max_turns: self.max_turns,
            max_seconds: self.max_seconds,
            token_budget: self.token_budget,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageGoalPin {
    pub stage_revision: String,
    pub configuration_revision: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_revision: Option<PackageArtifactVersion>,
    pub configuration: StageGoalConfiguration,
}

impl StageGoalPin {
    pub(super) fn for_stage(stage: &PinnedNode) -> Result<Option<Self>> {
        let NodeConfiguration::AgentRole { requirements, .. } = &stage.instance.configuration
        else {
            return Ok(None);
        };
        requirements
            .goal
            .as_ref()
            .map(|goal| Self::new(stage, goal.clone()))
            .transpose()
    }

    pub(super) fn new(stage: &PinnedNode, configuration: StageGoalConfiguration) -> Result<Self> {
        configuration.validate()?;
        ensure!(
            matches!(
                stage.definition.executor,
                Executor::PlanAgent | Executor::ReadOnlyAgent | Executor::WriteAgent
            ),
            "Select Goal Mode on an agent stage"
        );
        ensure!(
            stage.definition.kind != BuiltinNode::ReviewCandidate,
            "This isolated candidate reviewer cannot start a Goal with a run-scoped bridge"
        );
        ensure!(
            stage.definition.executor == Executor::WriteAgent
                || configuration
                    .criteria
                    .iter()
                    .all(|criterion| criterion.evidence != StageGoalEvidence::CandidateGate),
            "A candidate gate criterion requires a workflow writing stage"
        );
        let NodeConfiguration::AgentRole {
            role, requirements, ..
        } = &stage.instance.configuration
        else {
            anyhow::bail!("The goal stage lost its pinned role");
        };
        Ok(Self {
            stage_revision: stage.revision.clone(),
            configuration_revision: digest(&serde_json::to_vec(&configuration)?),
            role: role.clone(),
            role_revision: requirements.bound_role.clone(),
            configuration,
        })
    }

    pub(super) fn require_criterion_evidence(
        &self,
        ordinal: usize,
        evidence: &GoalCriterionEvidence,
    ) -> Result<()> {
        let requirement = ordinal
            .checked_sub(1)
            .and_then(|index| self.configuration.criteria.get(index));
        let actual = match evidence {
            GoalCriterionEvidence::OperatorReview { .. } => Some(StageGoalEvidence::OperatorReview),
            GoalCriterionEvidence::CandidateGate { .. } => Some(StageGoalEvidence::CandidateGate),
            GoalCriterionEvidence::CandidateAcceptance { .. } => None,
        };
        ensure!(
            requirement.is_some_and(|criterion| Some(criterion.evidence) == actual),
            "The workflow goal criterion {ordinal} requires a different evidence type"
        );
        Ok(())
    }

    pub(super) fn require_verified_result(
        &self,
        goal_revision: &str,
        attempt: &str,
        candidate: Option<&str>,
        verification: &GoalVerification,
    ) -> Result<()> {
        ensure!(
            verification.revision_id == goal_revision && verification.run_id == attempt,
            "The verification belongs to a different workflow goal attempt"
        );
        ensure!(
            verification.status == GoalVerificationStatus::Completed,
            "The workflow goal is waiting for verified result evidence"
        );
        ensure!(
            verification
                .candidate_id
                .as_deref()
                .is_none_or(|verified| Some(verified) == candidate),
            "The workflow goal verified a different candidate"
        );
        ensure!(
            self.configuration.criteria.len() == verification.criteria.len(),
            "The goal result differs from the pinned criteria"
        );
        for (index, (expected, criterion)) in self
            .configuration
            .criteria
            .iter()
            .zip(&verification.criteria)
            .enumerate()
        {
            ensure!(
                criterion.ordinal == index + 1
                    && criterion.text == expected.text
                    && criterion.status == GoalCriterionStatus::Passed,
                "The workflow goal criterion {} has no matching verified result",
                index + 1
            );
            let accepted = match (expected.evidence, criterion.evidence.as_ref()) {
                (
                    StageGoalEvidence::OperatorReview,
                    Some(GoalCriterionEvidence::OperatorReview { approved: true, .. }),
                ) => true,
                (
                    StageGoalEvidence::CandidateGate,
                    Some(GoalCriterionEvidence::CandidateGate { .. }),
                ) => candidate.is_some() && verification.candidate_id.as_deref() == candidate,
                _ => false,
            };
            ensure!(
                accepted,
                "The workflow goal criterion {} requires a different evidence type",
                index + 1
            );
        }
        Ok(())
    }
}

pub(crate) fn load_stage_goal(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
) -> Result<Option<StageGoalPin>> {
    let stage = super::runtime::load_pinned_node(db, project, run, node)?
        .context("The workflow goal stage is outside this project or run")?;
    StageGoalPin::for_stage(&stage)
}

struct AttemptGoal {
    pin: StageGoalPin,
    goal_revision: String,
    project: String,
    workflow: String,
    node: String,
}

fn attempt_goal(db: &Connection, attempt: &str) -> Result<Option<AttemptGoal>> {
    let binding: Option<(String, String, String, Option<String>)> = db
        .query_row(
            "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id, g.revision_id
         FROM job_attempts a JOIN job_pipeline_origins o ON o.job_id = a.job_id
         JOIN workflow_job_inputs w ON w.job_id = a.job_id
         LEFT JOIN goal_continuations g ON g.job_id = a.job_id WHERE a.run_id = ?1",
            [attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((project, workflow, node, revision)) = binding else {
        return Ok(None);
    };
    let Some(pin) = load_stage_goal(db, &project, &workflow, &node)? else {
        return Ok(None);
    };
    let goal_revision = revision.context("The workflow attempt lost its pinned goal revision")?;
    Ok(Some(AttemptGoal {
        pin,
        goal_revision,
        project,
        workflow,
        node,
    }))
}

pub(crate) fn require_stage_criterion(
    db: &Connection,
    run: &str,
    ordinal: usize,
    evidence: &GoalCriterionEvidence,
) -> Result<()> {
    if let Some(goal) = attempt_goal(db, run)? {
        goal.pin.require_criterion_evidence(ordinal, evidence)?;
    }
    Ok(())
}

pub(super) fn require_stage_goal_result(
    db: &Connection,
    project: &str,
    workflow: &str,
    node: &str,
    attempt: &str,
    candidate: Option<&SelectedCandidate>,
) -> Result<()> {
    let Some(goal) = attempt_goal(db, attempt)? else {
        return Ok(());
    };
    ensure!(
        goal.project == project && goal.workflow == workflow && goal.node == node,
        "The goal result belongs to a different pinned workflow stage"
    );
    if let Some(candidate) = candidate {
        require_current(db, candidate)?;
        require_workflow_goal_release(db, attempt)?;
    }
    let verification = load_goal_verification(db, project, attempt)?
        .context("The workflow goal has not reached a result ready for verification")?;
    goal.pin.require_verified_result(
        &goal.goal_revision,
        attempt,
        candidate.map(|selection| selection.id.as_str()),
        &verification,
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageGoalAssessment {
    pub pin: StageGoalPin,
    pub route: StageGoalRoute,
}

impl Store {
    pub async fn verify_workflow_goal_candidate(
        &self,
        project: String,
        run: String,
        node: String,
        task: String,
        selection: String,
    ) -> Result<CandidateVerification> {
        let candidate = self
            .selected_candidate(project.clone(), task.clone(), selection.clone())
            .await?
            .context("The workflow goal needs a candidate from this task")?;
        self.check_artifact_source(
            &project,
            &run,
            &node,
            &ArtifactValue::Candidate(candidate.clone()),
        )
        .await?;
        let attempt = candidate.target.run_id.clone();
        let required = self.execute({
            let (project, run, attempt) = (project.clone(), run.clone(), attempt.clone());
            move |db| {
                require_current(db, &candidate)?;
                require_workflow_goal_release(db, &attempt)?;
                let goal = attempt_goal(db, &attempt)?.context("The candidate attempt does not belong to a pinned workflow goal")?;
                ensure!(goal.project == project && goal.workflow == run && goal.node == node,
                    "The candidate belongs to a different workflow goal stage");
                ensure!(goal.pin.configuration.criteria.iter().any(|criterion| criterion.evidence == StageGoalEvidence::CandidateGate),
                    "This workflow goal has no candidate check criterion");
                load_goal_verification(db, &project, &attempt)?
                    .context("Finish the workflow goal turn before checking the candidate")?;
                let workflow = super::runtime::load_workflow_run(db, &project, &run)?.context("The workflow run is unavailable")?;
                let definition = super::runtime::load_definition(db, &project, &workflow.version)?.context("The pinned workflow definition is unavailable")?;
                let required = definition.wires.iter()
                    .filter(|wire| wire.from.node == node && wire.from.port == "candidate" && wire.to.port == "candidate")
                    .filter_map(|wire| definition.nodes.iter().find(|target| target.instance.id == wire.to.node))
                    .filter_map(|target| match &target.instance.configuration {
                        NodeConfiguration::RequiredChecks { names } if target.definition.kind == BuiltinNode::VerifyCandidate => Some(names),
                        _ => None,
                    }).flatten().cloned().collect::<std::collections::BTreeSet<_>>();
                Ok(required.into_iter().collect())
            }
        }).await?;
        let verification = self
            .verify_selected_candidate_with_requirements(project, task, selection, required)
            .await?;
        self.execute(move |db| require_workflow_goal_release(db, &attempt))
            .await?;
        Ok(verification)
    }

    pub async fn preview_workflow_stage_goal(
        &self,
        project: String,
        checkout: String,
        node: NodeInstance,
        scope: SessionScope,
        bridge_available: bool,
    ) -> Result<Option<StageGoalAssessment>> {
        let definition = builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == node.kind)
            .context("The goal stage kind is outside the workflow catalog")?;
        let revision = digest(&serde_json::to_vec(&(&node, &definition))?);
        let stage = PinnedNode {
            instance: node,
            revision,
            definition,
        };
        self.execute(move |db| {
            assess_pinned_goal(db, &project, &checkout, &stage, &scope, bridge_available)
        })
        .await
    }

    pub async fn workflow_stage_goal_assessment(
        &self,
        project: String,
        run: String,
        node: String,
        checkout: String,
        scope: SessionScope,
        bridge_available: bool,
    ) -> Result<Option<StageGoalAssessment>> {
        self.execute(move |db| {
            let stage = super::runtime::load_pinned_node(db, &project, &run, &node)?
                .context("The workflow goal stage is outside this project or run")?;
            assess_pinned_goal(db, &project, &checkout, &stage, &scope, bridge_available)
        })
        .await
    }
}

fn assess_pinned_goal(
    db: &Connection,
    project: &str,
    checkout: &str,
    stage: &PinnedNode,
    scope: &SessionScope,
    bridge_available: bool,
) -> Result<Option<StageGoalAssessment>> {
    let root: Option<String> = db
        .query_row(
            "SELECT root FROM checkouts WHERE project_id = ?1 AND id = ?2",
            params![project, checkout],
            |row| row.get(0),
        )
        .optional()?;
    ensure!(
        root.as_ref()
            .is_some_and(|root| scope.checkout == std::path::Path::new(root)),
        "The Goal profile is outside the selected project checkout"
    );
    let Some(pin) = StageGoalPin::for_stage(stage)? else {
        return Ok(None);
    };
    let route = assess_stage_profile(db, project, checkout, stage, &pin, scope, bridge_available)?;
    Ok(Some(StageGoalAssessment { pin, route }))
}

fn assess_stage_profile(
    db: &Connection,
    project: &str,
    checkout: &str,
    stage: &PinnedNode,
    pin: &StageGoalPin,
    scope: &SessionScope,
    bridge_available: bool,
) -> Result<StageGoalRoute> {
    let NodeConfiguration::AgentRole { model, .. } = &stage.instance.configuration else {
        anyhow::bail!("The goal stage lost its pinned role");
    };
    let configured_plan = super::stage_plan::StagePlanConfiguration::for_stage(stage)?;
    let mode_matches = matches!(
        (stage.definition.executor, scope.mode),
        (
            Executor::PlanAgent,
            SessionMode::Plan | SessionMode::ReadOnly
        ) | (Executor::ReadOnlyAgent, SessionMode::ReadOnly)
            | (Executor::WriteAgent, SessionMode::ApprovedWrite)
    ) && configured_plan.is_none_or(|plan| scope.mode == plan.mode());
    let model_matches = model.as_ref().is_none_or(|selection| {
        selection.provider == scope.provider
            && selection.profile_id == scope.profile_id
            && scope.model.as_deref() == Some(selection.model_id.as_str())
    });
    if !mode_matches || !model_matches {
        return Ok(StageGoalRoute::Blocked {
            reason: "The profile does not match the pinned stage mode and model",
        });
    }
    if let Some(version) = &pin.role_revision {
        let role = roles::load_role(db, project, version)?
            .context("The pinned workflow role is unavailable")?;
        if roles::qualify_bound_role(&role, model.as_ref(), scope).is_err() {
            return Ok(StageGoalRoute::Blocked {
                reason: "The profile is outside the pinned stage role",
            });
        }
    }
    let candidate = if configured_plan.is_some() {
        None
    } else {
        native_goal_candidate(db, checkout, scope, &pin.configuration.definition())?
    };
    Ok(assess_route(
        &pin.configuration,
        scope,
        bridge_available,
        candidate.as_ref(),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "route",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StageGoalRoute {
    NativeCandidate { evidence_id: String },
    Managed { reason: &'static str },
    Blocked { reason: &'static str },
}

/// The native candidate comes from the registered qualification for this exact launch
/// scope. Dispatch still verifies the profile actually reported by the provider.
pub(super) fn assess_route(
    configuration: &StageGoalConfiguration,
    scope: &SessionScope,
    bridge_available: bool,
    native_candidate: Option<&NativeGoalEvidence>,
) -> StageGoalRoute {
    if !bridge_available {
        return StageGoalRoute::Blocked {
            reason: "Goal Mode needs the run-scoped tool bridge",
        };
    }
    if scope.provider == Provider::Codex
        && scope.mode != SessionMode::Plan
        && configuration.max_turns.is_none()
        && let Some(candidate) = native_candidate
        && let NativeGoalSupport::Qualified { evidence_id, .. } =
            candidate.resolve(&candidate.scope_key, configuration.token_budget.is_some())
    {
        return StageGoalRoute::NativeCandidate { evidence_id };
    }
    if configuration.token_budget.is_some() {
        return StageGoalRoute::Blocked {
            reason: "The effective Goal route cannot enforce this token budget",
        };
    }
    let reason = if configuration.max_turns.is_some() {
        "The scheduler enforces the requested turn limit"
    } else if scope.mode == SessionMode::Plan {
        "Goal continuation uses the read-only planning profile"
    } else {
        "The selected launch scope has no verified native Goal controls"
    };
    StageGoalRoute::Managed { reason }
}

#[cfg(test)]
mod requalification_tests;
#[cfg(test)]
mod tests;
