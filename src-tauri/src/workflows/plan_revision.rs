use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    jobs::{Job, PipelineDispatch},
    providers::SessionMode,
    runs::RunStatus,
    store::Store,
};

use super::{ArtifactKind, ArtifactValue, WorkflowAgentSubmission, artifacts::load_artifact};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanRevisionRequest {
    pub base_revision_id: String,
    pub expected_plan_revision: Option<String>,
    pub feedback: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanHistory {
    pub request: Option<WorkflowPlanRevisionRequest>,
    pub versions: Vec<WorkflowPlanVersion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPlanVersion {
    pub revision_id: String,
    pub source_run_id: String,
    pub event_sequence: String,
    pub text: String,
    pub current: bool,
    pub approval_artifact_id: Option<String>,
    pub release_id: Option<String>,
    pub release_revoked: Option<bool>,
}

impl Store {
    pub async fn request_workflow_plan_revision(
        &self,
        mut submission: WorkflowAgentSubmission,
        mut revision: WorkflowPlanRevisionRequest,
    ) -> Result<Job> {
        ensure!(
            submission.dispatch.node_instance_id == "plan"
                && submission.scope.mode == SessionMode::Plan,
            "A revision requires the workflow's read-only planning stage"
        );
        revision.feedback = revision.feedback.trim().to_owned();
        ensure!(
            !revision.feedback.is_empty(),
            "Add feedback for the planner"
        );
        let workflow = &submission.dispatch.pipeline_run_id;
        let base = self
            .resolve_workflow_artifact(
                submission.project_id.clone(),
                workflow.clone(),
                revision.base_revision_id.clone(),
                ArtifactKind::Plan,
            )
            .await?
            .context("The selected plan revision is outside this workflow")?;
        let ArtifactValue::Plan {
            run_id,
            event_sequence,
        } = base.value
        else {
            anyhow::bail!("The selected artifact is not a plan");
        };
        let source = self
            .workflow_attempt(submission.project_id.clone(), run_id.clone())
            .await?
            .context("The selected planning attempt is unavailable")?;
        ensure!(
            source.status == RunStatus::Succeeded && source.pin.workflow_run_id == *workflow,
            "A revision requires a completed planning attempt in this workflow"
        );
        let plan = self
            .workflow_plan_proposals(submission.project_id.clone(), workflow.clone(), run_id)
            .await?
            .into_iter()
            .find(|proposal| proposal.sequence == event_sequence.to_string())
            .context("The exact saved proposal is unavailable")?;
        submission.request.instructions = format!(
            "Revise the saved implementation plan in response to the operator feedback. Use the pinned repository document and ask for any remaining decisions.\n\nSaved plan revision {}\n{}\n\nRevision feedback\n{}",
            revision.base_revision_id, plan.text, revision.feedback,
        );
        self.submit_workflow_agent_with_revision(submission, Some(revision))
            .await
    }

    pub async fn workflow_plan_history(
        &self,
        project: String,
        run_id: String,
    ) -> Result<Option<WorkflowPlanHistory>> {
        let Some(attempt) = self
            .workflow_attempt(project.clone(), run_id.clone())
            .await?
        else {
            return Ok(None);
        };
        let workflow = attempt.pin.workflow_run_id;
        let selected = self
            .workflow_run(project.clone(), workflow.clone())
            .await?
            .context("The recorded workflow is unavailable")?
            .plan_revision;
        let request = self
            .workflow_attempt_input(run_id.clone())
            .await?
            .and_then(|input| input.plan_revision_request);
        let artifacts = self
            .workflow_run_artifacts(project.clone(), workflow.clone())
            .await?;
        let active_outputs = self.current_workflow_host_outputs(project.clone(), workflow.clone()).await?;
        let attempt_decision = artifacts.iter().find_map(|artifact| match &artifact.value {
            ArtifactValue::Release(reference) if attempt.pin.release_id.as_ref() == Some(&reference.release.id) =>
                Some(&reference.decision_artifact_id),
            _ => None,
        });
        let mut versions = Vec::new();
        for artifact in &artifacts {
            let ArtifactValue::Plan {
                run_id: source,
                event_sequence,
            } = &artifact.value
            else {
                continue;
            };
            if source != &run_id
                && attempt.pin.plan_revision.as_ref() != Some(&artifact.id)
                && request.as_ref().map(|request| &request.base_revision_id) != Some(&artifact.id)
            {
                continue;
            }
            let mut approvals = artifacts.iter().filter(|item| {
                matches!(&item.value, ArtifactValue::Approval(decision) if decision.plan_revision == artifact.id)
            });
            let approval = approvals.clone().find(|item| Some(&item.id) == attempt_decision)
                .or_else(|| approvals.clone().find(|item| active_outputs.values().any(|id| id == &item.id)))
                .or_else(|| approvals.next());
            let release = approval.and_then(|approval| {
                artifacts.iter().find_map(|item| match &item.value {
                    ArtifactValue::Release(reference)
                        if reference.decision_artifact_id == approval.id =>
                    {
                        Some(&reference.release)
                    }
                    _ => None,
                })
            });
            let release_revoked = if let Some(release) = release {
                let (project, workflow, id, revision) = (
                    project.clone(),
                    workflow.clone(),
                    release.id.clone(),
                    artifact.id.clone(),
                );
                Some(self.execute(move |db| Ok(db.query_row(
                    "SELECT revoked FROM job_pipeline_releases
                     WHERE project_id = ?1 AND pipeline_run_id = ?2 AND id = ?3 AND plan_revision = ?4",
                    params![project, workflow, id, revision], |row| row.get::<_, bool>(0),
                )?)).await?)
            } else {
                None
            };
            versions.push((
                *event_sequence,
                WorkflowPlanVersion {
                    revision_id: artifact.id.clone(),
                    source_run_id: source.clone(),
                    event_sequence: event_sequence.to_string(),
                    text: self
                        .workflow_plan_text(source.clone(), *event_sequence)
                        .await?,
                    current: selected.as_ref() == Some(&artifact.id),
                    approval_artifact_id: approval.map(|approval| approval.id.clone()),
                    release_revoked,
                    release_id: release.map(|release| release.id.clone()),
                },
            ));
        }
        versions.sort_unstable_by_key(|(sequence, _)| *sequence);
        Ok(
            (request.is_some() || !versions.is_empty()).then(|| WorkflowPlanHistory {
                request,
                versions: versions.into_iter().map(|(_, version)| version).collect(),
            }),
        )
    }
}

pub(super) fn check_revision_admission(
    db: &Connection,
    project: &str,
    binding: &PipelineDispatch,
    revision: &WorkflowPlanRevisionRequest,
) -> Result<()> {
    let selected: Option<String> = db.query_row(
        "SELECT plan_revision FROM job_pipeline_runs WHERE project_id = ?1 AND id = ?2",
        params![project, binding.pipeline_run_id],
        |row| row.get(0),
    )?;
    ensure!(
        selected == revision.expected_plan_revision,
        "The selected plan changed before the revision was queued"
    );
    load_artifact(
        db,
        project,
        &binding.pipeline_run_id,
        &revision.base_revision_id,
        ArtifactKind::Plan,
    )?
    .context("The base plan is unavailable in this workflow")?;
    let pending: bool = db.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM job_pipeline_origins o JOIN jobs j ON j.id = o.job_id
            WHERE o.project_id = ?1 AND o.pipeline_run_id = ?2 AND o.node_instance_id = ?3
              AND ((j.queue_state <> 'cancelled' AND NOT EXISTS (SELECT 1 FROM job_attempts a WHERE a.job_id = j.id))
                OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                           WHERE a.job_id = j.id AND r.status IN ('starting', 'running', 'waiting_for_input'))
                OR EXISTS (SELECT 1 FROM job_recoveries retry WHERE retry.job_id = j.id))
        )",
        params![project, binding.pipeline_run_id, binding.node_instance_id],
        |row| row.get(0),
    )?;
    ensure!(!pending, "A planning attempt is already queued or running");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dispatch::EventSource,
        jobs::AdmissionEnvironment,
        providers::{ProviderEvent, ProviderEventData, ProviderLocation, planning::PlanSource},
        workflows::{
            NodeInputs,
            tests::{approve_delivery_fixture_for_work, approved_delivery_repository, submission},
        },
    };

    #[tokio::test]
    async fn requesting_revision_seals_feedback_and_leaves_the_release_until_publication()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("workflow.sqlite");
        let store = Store::open(database.clone())?;
        let repo = approved_delivery_repository(&store, directory.path()).await?;
        let fixture =
            approve_delivery_fixture_for_work(&store, directory.path(), repo.clone(), None).await?;
        let artifact = &fixture.plan;
        let ArtifactValue::Plan {
            run_id: original_run,
            ..
        } = &artifact.value
        else {
            anyhow::bail!("fixture omitted its plan");
        };
        let ArtifactValue::Release(release) = &fixture.release.value else {
            anyhow::bail!("fixture omitted its release");
        };
        let context = store
            .workflow_run_artifacts(fixture.project.clone(), fixture.run.id.clone())
            .await?
            .into_iter()
            .find(|artifact| artifact.node_instance_id == "context")
            .context("fixture omitted its context")?;
        let make_submission = || {
            let mut planner = submission(&repo, &fixture.run, "plan", None, SessionMode::Plan);
            planner.artifacts = NodeInputs::from([("context".into(), context.id.clone())]);
            planner
        };
        let request = WorkflowPlanRevisionRequest {
            base_revision_id: artifact.id.clone(),
            expected_plan_revision: Some(artifact.id.clone()),
            feedback: "Require a smoke test for the first change".into(),
        };
        let mut stale = request.clone();
        stale.expected_plan_revision = None;
        assert!(
            store
                .request_workflow_plan_revision(make_submission(), stale)
                .await
                .is_err()
        );
        let job = store
            .request_workflow_plan_revision(make_submission(), request.clone())
            .await?;
        assert!(
            store
                .request_workflow_plan_revision(make_submission(), request.clone())
                .await
                .is_err()
        );
        assert_eq!(
            store
                .workflow_run(fixture.project.clone(), fixture.run.id.clone())
                .await?
                .unwrap()
                .plan_revision,
            Some(artifact.id.clone()),
        );
        assert_eq!(
            store
                .workflow_approval_reference(
                    fixture.project.clone(),
                    fixture.run.id.clone(),
                    release.release.id.clone()
                )
                .await?,
            *release,
        );

        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-runner".into(),
                bridge_available: false,
            })
            .await?
            .context("revision request was not admitted")?;
        assert_eq!(attempt.scope.mode, SessionMode::Plan);
        let run = attempt.run.id.clone();
        let command = &attempt
            .input
            .last()
            .context("revision instructions are missing")?
            .text;
        assert!(
            command.contains("Do each change separately.") && command.contains(&request.feedback)
        );
        let revision_attempt = store
            .workflow_attempt(fixture.project.clone(), run.clone())
            .await?
            .unwrap();
        assert_eq!(revision_attempt.job_id, job.id);
        assert_eq!(revision_attempt.pin.release_id, None);
        let historical = store
            .workflow_plan_history(fixture.project.clone(), run.clone())
            .await?
            .unwrap();
        assert_eq!(historical.request.as_ref(), Some(&request));
        assert_eq!(historical.versions[0].revision_id, artifact.id);
        assert_eq!(historical.versions[0].release_revoked, Some(false));

        store.mark_agent_running(run.clone()).await?;
        let revised = "Add a smoke test.\n\n```omniagent-plan\n{\"units\":[{\"purpose\":\"First change\",\"completionCriteria\":[\"Smoke test passes\"]}]}\n```";
        store
            .record_agent_observation(
                run.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::PlanProposed {
                        text: revised.into(),
                        source: PlanSource::CodexPlanItem,
                    },
                },
            )
            .await?;
        store
            .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
            .await?;
        let proposal = store
            .workflow_plan_proposals(fixture.project.clone(), fixture.run.id.clone(), run.clone())
            .await?
            .into_iter()
            .next()
            .context("revised proposal is missing")?;
        let version = store
            .publish_workflow_artifact(
                fixture.project.clone(),
                fixture.run.id.clone(),
                "plan".into(),
                "plan".into(),
                ArtifactValue::Plan {
                    run_id: run.clone(),
                    event_sequence: proposal.sequence.parse()?,
                },
            )
            .await?;
        store
            .set_job_pipeline_plan(
                fixture.project.clone(),
                fixture.run.id.clone(),
                Some(artifact.id.clone()),
                version.id.clone(),
            )
            .await?;
        drop(store);

        let store = Store::open(database)?;
        assert!(
            store
                .workflow_plan_history("different-project".into(), run.clone())
                .await?
                .is_none()
        );
        let history = store
            .workflow_plan_history(fixture.project.clone(), run)
            .await?
            .unwrap();
        assert_eq!(history.request, Some(request));
        assert_eq!(history.versions[0].source_run_id, *original_run);
        assert_eq!(history.versions[0].release_revoked, Some(true));
        assert!(history.versions[0].approval_artifact_id.is_some());
        assert!(!history.versions[0].current);
        assert_eq!(history.versions[1].revision_id, version.id);
        assert_eq!(history.versions[1].text, revised);
        assert!(history.versions[1].current);
        assert_eq!(history.versions[1].approval_artifact_id, None);
        assert_eq!(history.versions[1].release_id, None);
        Ok(())
    }
}
