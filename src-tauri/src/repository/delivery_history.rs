use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    protection,
    store::Store,
    workflows::{ArtifactValue, WorkflowArtifact},
};

use super::verified_git::{VerifiedCommit, load_verified_delivery_commit};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAttempt {
    run_id: String,
    conversation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedDeliveryUnitHistory {
    commit: VerifiedCommit,
    workflow_run_id: String,
    plan_revision: String,
    approval_artifact_id: Option<String>,
    implementation: DeliveryAttempt,
    review: Option<DeliveryAttempt>,
}

impl Store {
    pub async fn verified_work_delivery_units(
        &self,
        project: String,
        task: String,
    ) -> Result<Vec<VerifiedDeliveryUnitHistory>> {
        self.execute(move |db| read_history(db, &project, Some(&task), None))
            .await
    }

    pub async fn verified_attempt_deliveries(
        &self,
        project: String,
        run: String,
    ) -> Result<Vec<VerifiedDeliveryUnitHistory>> {
        self.execute(move |db| read_history(db, &project, None, Some(&run)))
            .await
    }
}

fn read_history(
    db: &Connection,
    project: &str,
    task: Option<&str>,
    attempt: Option<&str>,
) -> Result<Vec<VerifiedDeliveryUnitHistory>> {
    let mut statement = db.prepare(
        "SELECT receipt.id, receipt.task_id, unit.pipeline_run_id, unit.plan_revision,
                implementation.conversation_id, release.decision_artifact_id
         FROM verified_delivery_commits AS receipt
         JOIN workflow_delivery_units unit ON unit.unit_id = receipt.unit_id
           AND unit.project_id = receipt.project_id AND unit.task_id = receipt.task_id
           AND unit.commit_evidence_id = receipt.id
         JOIN agent_attempts implementation ON implementation.run_id = receipt.capture_run_id
         JOIN job_attempts capture ON capture.run_id = implementation.run_id
         LEFT JOIN job_pipeline_origins origin ON origin.job_id = capture.job_id
           AND origin.project_id = receipt.project_id AND origin.pipeline_run_id = unit.pipeline_run_id
           AND origin.plan_revision = unit.plan_revision
         LEFT JOIN workflow_release_bindings release ON release.release_id = origin.release_id
         WHERE receipt.project_id = ?1 AND (
              (?2 IS NOT NULL AND receipt.task_id = ?2)
              OR (?3 IS NOT NULL AND (receipt.capture_run_id = ?3 OR unit.unit_id IN (
                  SELECT input.unit_id FROM job_attempts attempt
                  JOIN jobs job ON job.id = attempt.job_id
                  JOIN workflow_job_inputs input ON input.job_id = job.id
                  WHERE job.project_id = ?1 AND attempt.run_id = ?3
              )))
         )
         ORDER BY receipt.verified_at, unit.ordinal, receipt.id",
    )?;
    let rows = statement.query_map(params![project, task, attempt], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut history = Vec::new();
    for row in rows {
        let (
            evidence_id,
            task,
            workflow_run_id,
            plan_revision,
            conversation_id,
            approval_artifact_id,
        ) = row?;
        let commit = load_verified_delivery_commit(db, project, &task, &evidence_id)?
            .context("The saved unit commit is unavailable")?;
        let review = accepted_review(db, &workflow_run_id, &commit)?;
        if attempt.is_some_and(|attempt| {
            attempt != commit.capture_run_id
                && review
                    .as_ref()
                    .is_none_or(|review| review.run_id != attempt)
        }) {
            continue;
        }
        history.push(VerifiedDeliveryUnitHistory {
            implementation: DeliveryAttempt {
                run_id: commit.capture_run_id.clone(),
                conversation_id,
            },
            commit,
            workflow_run_id,
            plan_revision,
            approval_artifact_id,
            review,
        });
    }
    Ok(history)
}

fn accepted_review(
    db: &Connection,
    workflow: &str,
    commit: &VerifiedCommit,
) -> Result<Option<DeliveryAttempt>> {
    let mut query = db.prepare(
        "SELECT payload FROM workflow_artifacts
         WHERE pipeline_run_id = ?1 AND unit_id = ?2 AND kind = 'acceptance'
           AND (?3 IS NULL OR id = ?3)",
    )?;
    let stored = query.query_map(
        params![workflow, commit.unit_id, commit.acceptance_artifact_id],
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut review_id = None;
    for payload in stored {
        let artifact: WorkflowArtifact =
            serde_json::from_slice(&protection::unprotect(&payload?)?)?;
        let ArtifactValue::Acceptance(reference) = artifact.value else {
            continue;
        };
        if reference.evidence.id != commit.acceptance_id
            || reference.evidence.task.project_id != commit.project_id
            || reference.evidence.task.task_id != commit.task_id
        {
            continue;
        }
        if review_id
            .as_ref()
            .is_some_and(|id| id != &reference.review_artifact_id)
        {
            return Ok(None);
        }
        review_id = Some(reference.review_artifact_id);
    }
    let Some(review_id) = review_id else {
        return Ok(None);
    };
    let saved: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM workflow_artifacts
         WHERE pipeline_run_id = ?1 AND unit_id = ?2 AND kind = 'review' AND id = ?3",
            params![workflow, commit.unit_id, review_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(saved) = saved else { return Ok(None) };
    let artifact: WorkflowArtifact = serde_json::from_slice(&protection::unprotect(&saved)?)?;
    let ArtifactValue::Review { run_id } = artifact.value else {
        return Ok(None);
    };
    let conversation_id = db.query_row(
        "SELECT attempt.conversation_id FROM agent_attempts attempt
         JOIN job_attempts run ON run.run_id = attempt.run_id
         JOIN job_pipeline_origins origin ON origin.job_id = run.job_id
         JOIN workflow_job_inputs input ON input.job_id = run.job_id
         WHERE origin.project_id = ?1 AND origin.pipeline_run_id = ?2 AND input.unit_id = ?3 AND attempt.run_id = ?4",
        params![commit.project_id, workflow, commit.unit_id, run_id],
        |row| row.get::<_, String>(0),
    ).optional()?;
    Ok(conversation_id.map(|conversation_id| DeliveryAttempt {
        run_id,
        conversation_id,
    }))
}
