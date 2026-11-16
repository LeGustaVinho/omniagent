use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{
    context_records::assembly::ContextDecision,
    dispatch::ContextSnapshot,
    protection,
    providers::SessionMode,
    repository::RepositoryMarkdownRevision,
    store::Store,
    tasks::WorkDescriptions,
    workflows::{ArtifactValue, WorkflowArtifact, WorkflowPlanUnit},
};

use super::{
    InspectedGoal, JobKind, JobOrigin, PipelineJobOrigin, goal_inspection::inspect_run_goal,
    pipeline::load_attempt_job, policy::EffectivePolicy,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOrchestration {
    job: Option<OrchestrationJob>,
    route: Option<ProfileRoute>,
    policy: Option<RecordedPolicy>,
    stage_pin: Option<WorkflowStagePin>,
    context: Option<ContextBudget>,
    document: Option<DocumentSource>,
    task_descriptions: Option<WorkDescriptions>,
    goal: Option<InspectedGoal>,
    unit: Option<WorkflowPlanUnit>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestrationJob {
    id: String,
    kind: JobKind,
    task_id: Option<String>,
    owner_task_id: Option<String>,
    origin: JobOrigin,
    operator_requested: bool,
    stage: Option<PipelineJobOrigin>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileRoute {
    mode: SessionMode,
    question_route: crate::providers::questions::QuestionRoute,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedPolicy {
    revision: String,
    evidence: Option<EffectivePolicy>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowStagePin {
    workflow_revision: String,
    instance_revision: String,
    definition_revision: u32,
    scheduler_profile: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextBudget {
    policy_revision: String,
    role: String,
    available_bytes: u64,
    used_bytes: u64,
    decisions: Vec<ContextDecision>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DocumentSource {
    revision_id: String,
    checkout_id: String,
    path: String,
    sha256: String,
    base_commit: Option<String>,
}

impl Store {
    pub async fn run_orchestration(
        &self,
        project: String,
        run: String,
        conversation: Option<String>,
    ) -> Result<Option<RunOrchestration>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let authorized: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM runs r JOIN checkouts c ON c.id = r.checkout_id
                    LEFT JOIN agent_attempts a ON a.run_id = r.id
                    WHERE c.project_id = ?1 AND r.id = ?2
                      AND (?3 IS NULL OR a.conversation_id = ?3)
                 )",
                params![project, run, conversation], |row| row.get(0),
            )?;
            if !authorized {
                return Ok(None);
            }
            let job = load_attempt_job(&tx, &run)?.map(|job| OrchestrationJob {
                id: job.id, kind: job.kind, task_id: job.task_id,
                owner_task_id: job.worktree_owner_task_id, origin: job.origin,
                operator_requested: job.operator_requested.unwrap_or_default(), stage: job.pipeline,
            });
            let stage = job.as_ref().and_then(|job| job.stage.as_ref());
            let stage_pin = stage.map(|stage| {
                tx.query_row(
                    "SELECT p.workflow_revision, n.instance_revision, n.definition_revision, n.scheduler_profile
                     FROM workflow_run_pins p JOIN workflow_node_pins n USING (pipeline_run_id)
                     WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2 AND n.node_instance_id = ?3",
                    params![project, stage.pipeline_run_id, stage.node_instance_id],
                    |row| Ok(WorkflowStagePin {
                        workflow_revision: row.get(0)?, instance_revision: row.get(1)?,
                        definition_revision: row.get(2)?, scheduler_profile: row.get(3)?,
                    }),
                ).optional()
            }).transpose()?.flatten();
            let saved_policy = tx.query_row(
                "SELECT policy_key, evidence FROM job_attempt_policies WHERE run_id = ?1",
                [&run],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
            ).optional()?;
            let policy = saved_policy.map(|(revision, protected)| -> Result<_> {
                Ok(RecordedPolicy {
                    revision,
                    evidence: protected.map(|bytes| decode(&bytes)).transpose()?,
                })
            }).transpose()?;
            let snapshot: Option<ContextSnapshot> = tx.query_row(
                "SELECT payload FROM context_snapshots WHERE run_id = ?1",
                [&run], |row| row.get::<_, Vec<u8>>(0),
            ).optional()?.map(|bytes| decode(&bytes)).transpose()?;
            let (route, context, document, task_descriptions) = if let Some(saved) = snapshot {
                (
                    Some(ProfileRoute { mode: saved.scope.mode, question_route: saved.question_route }),
                    saved.role_context.map(|role| ContextBudget {
                        policy_revision: role.policy_revision_id,
                        role: role.selection.role,
                        available_bytes: role.selection.available_bytes,
                        used_bytes: role.selection.used_bytes,
                        decisions: role.selection.decisions,
                    }),
                    saved.repository_markdown.map(|document| document.revision),
                    saved.task_context.map(|task| task.descriptions)
                        .filter(|descriptions| descriptions != &WorkDescriptions::default()),
                )
            } else {
                (None, None, None, None)
            };
            let document = match (document, stage) {
                (None, Some(stage)) => {
                    let payload = tx.query_row(
                        "SELECT a.payload FROM workflow_host_completions c
                         JOIN workflow_artifacts a ON a.id = c.artifact_id
                         JOIN workflow_run_pins p ON p.pipeline_run_id = a.pipeline_run_id
                         WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2
                           AND c.node_instance_id = 'start' AND c.execution_key = '' AND a.kind = 'markdown'",
                        params![project, stage.pipeline_run_id],
                        |row| row.get::<_, Vec<u8>>(0),
                    ).optional()?;
                    let source: Option<WorkflowArtifact> = payload.map(|bytes| decode(&bytes)).transpose()?;
                    source.and_then(|artifact| match artifact.value {
                        ArtifactValue::Markdown(revision) => Some(revision),
                        _ => None,
                    })
                }
                (source, _) => source,
            }.map(DocumentSource::from);
            let goal = inspect_run_goal(&tx, &run)?;
            let unit = tx.query_row(
                "SELECT u.unit FROM job_attempts a
                 JOIN workflow_job_inputs i ON i.job_id = a.job_id
                 JOIN workflow_delivery_units u ON u.unit_id = i.unit_id
                 WHERE a.run_id = ?1 AND u.project_id = ?2",
                params![run, project], |row| row.get::<_, Vec<u8>>(0),
            ).optional()?.map(|bytes| decode(&bytes)).transpose()?;
            Ok(Some(RunOrchestration { job, route, policy, stage_pin, context, document, task_descriptions, goal, unit }))
        }).await
    }
}

impl From<RepositoryMarkdownRevision> for DocumentSource {
    fn from(revision: RepositoryMarkdownRevision) -> Self {
        Self {
            revision_id: revision.id,
            checkout_id: revision.checkout_id,
            path: revision.relative_path,
            sha256: revision.sha256,
            base_commit: revision.base_commit,
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}
