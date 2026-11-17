use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    jobs::PipelineDispatch,
    providers::{
        Capability, SessionMode, capabilities::CapabilitySubject, questions::QuestionRoute,
    },
};

use super::{
    Executor, NodeConfiguration, PinnedNode, runtime::load_pinned_node,
    stage_requirements::StageRequirements,
};

pub(crate) const MAX_SUBAGENTS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagePlanConfiguration {
    pub question_route: QuestionRoute,
    pub max_subagents: u32,
}

impl StagePlanConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        self.max_subagents <= MAX_SUBAGENTS
    }

    pub fn mode(&self) -> SessionMode {
        match self.question_route {
            QuestionRoute::Native => SessionMode::Plan,
            QuestionRoute::StructuredTurn => SessionMode::ReadOnly,
        }
    }

    pub fn from_node(configuration: &NodeConfiguration) -> Option<Self> {
        match configuration {
            NodeConfiguration::AgentRole { requirements, .. } => requirements.plan,
            _ => None,
        }
    }

    pub(super) fn for_stage(stage: &PinnedNode) -> Result<Option<Self>> {
        let plan = Self::from_node(&stage.instance.configuration);
        if let Some(plan) = plan {
            ensure!(
                stage.definition.executor == Executor::PlanAgent,
                "Select Plan Mode on a Plan stage"
            );
            ensure!(
                plan.is_valid(),
                "The planning stage subagent limit exceeds {MAX_SUBAGENTS}"
            );
        }
        Ok(plan)
    }

    pub(super) fn require_capabilities(&self, requirements: &mut StageRequirements) {
        if self.question_route == QuestionRoute::Native {
            for capability in [
                Capability::NativePlanMode,
                Capability::PlanningQuestions,
                Capability::PlanningAnswers,
                Capability::NativePlanOutput,
            ] {
                let subject = CapabilitySubject::Protocol { capability };
                if !requirements.capabilities.contains(&subject) {
                    requirements.capabilities.push(subject);
                }
            }
        }
    }
}

pub(crate) fn load_stage_plan(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
) -> Result<Option<StagePlanConfiguration>> {
    let stage = load_pinned_node(
        db,
        project,
        &binding.pipeline_run_id,
        &binding.node_instance_id,
    )?
    .context("The scheduled planning node is unavailable")?;
    StagePlanConfiguration::for_stage(&stage)
}

pub(crate) fn bind_conversation(
    db: &Connection,
    job: &str,
    conversation: &str,
    binding: &PipelineDispatch,
    plan: StagePlanConfiguration,
) -> Result<()> {
    ensure!(
        binding.plan_revision.is_none() && binding.release_id.is_none(),
        "A Plan stage cannot receive a release or use a plan as an authorization"
    );
    let inserted = db.execute(
        "INSERT INTO workflow_planning_conversations (job_id, conversation_id, expected_plan_revision, max_subagents)
         SELECT ?1, ?2, plan_revision, ?4 FROM job_pipeline_runs WHERE id = ?3",
        params![job, conversation, binding.pipeline_run_id, plan.max_subagents],
    )?;
    ensure!(inserted == 1, "The pinned workflow scope is unavailable");
    Ok(())
}

pub(crate) fn plan_head_is_current(db: &Connection, job: &str) -> Result<Option<bool>> {
    Ok(db
        .query_row(
            "SELECT w.plan_revision IS p.expected_plan_revision
         FROM jobs j JOIN job_pipeline_origins o ON o.job_id = j.id
         JOIN job_pipeline_runs w ON w.id = o.pipeline_run_id AND w.project_id = o.project_id
         JOIN workflow_planning_conversations p ON p.conversation_id = j.conversation_id
         JOIN job_pipeline_origins root ON root.job_id = p.job_id
             AND root.project_id = o.project_id AND root.pipeline_run_id = o.pipeline_run_id
             AND root.node_instance_id = o.node_instance_id
         WHERE j.id = ?1",
            [job],
            |row| row.get(0),
        )
        .optional()?)
}
