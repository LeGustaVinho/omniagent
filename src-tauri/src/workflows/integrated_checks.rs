use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    diagnostics, protection,
    repository::verification::{
        integrated_checks::{IntegratedCheckResult, IntegratedGatePlan},
        test_phase::TestVerdict,
    },
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, NodeConfiguration, NodeInputs, PinnedNode, WorkflowRun,
    artifacts::{insert_artifact, load_artifact},
    control::{refresh, require_stage},
    digest,
    inspection::record_workflow_input_ids,
    runtime::{load_pinned_node, load_workflow_run},
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegratedRevision {
    pub destination_branch: String,
    pub sha: String,
    pub integration_shas: Vec<String>,
}

impl IntegratedRevision {
    fn valid(&self) -> bool {
        let len = self.sha.len();
        let oid = |value: &str| {
            value.len() == len
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        !self.destination_branch.trim().is_empty()
            && matches!(len, 40 | 64)
            && oid(&self.sha)
            && !self.integration_shas.is_empty()
            && self.integration_shas.iter().all(|sha| oid(sha))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegratedChecksConfiguration {
    pub checks: Vec<String>,
}

impl IntegratedChecksConfiguration {
    pub(crate) fn valid(&self) -> bool {
        let mut names = HashSet::new();
        (1..=32).contains(&self.checks.len())
            && self.checks.iter().all(|name| {
                !name.trim().is_empty()
                    && name.len() <= 256
                    && !name.chars().any(char::is_control)
                    && names.insert(name)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratedChecksEvidence {
    pub integrated_artifact_id: String,
    pub revision: IntegratedRevision,
    pub policy_revision: String,
    pub tree: Option<String>,
    pub verdict: TestVerdict,
    pub reason: Option<String>,
    pub checks: Vec<IntegratedCheckResult>,
}

fn request_key(node: &PinnedNode, inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(&node.revision, inputs))?))
}

fn integrated_input(
    db: &Connection,
    project: &str,
    run: &str,
    inputs: &NodeInputs,
) -> Result<(String, IntegratedRevision)> {
    let id = inputs
        .get("integrated")
        .context("Select a pinned integrated revision")?;
    let artifact = load_artifact(db, project, run, id, ArtifactKind::IntegratedRevision)?
        .context("The integrated revision is unavailable in this workflow")?;
    let ArtifactValue::IntegratedRevision(revision) = artifact.value else {
        unreachable!()
    };
    ensure!(
        artifact.unit_id.is_none() && revision.valid(),
        "Integrated checks need a project-scoped, exact commit and its merge evidence"
    );
    Ok((artifact.id, revision))
}

pub(super) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<&'static str>> {
    ensure!(unit.is_none(), "Integrated checks run at project scope");
    let automatic: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [&run.id],
        |row| row.get(0),
    )?;
    if !automatic {
        return Ok(Some("blocked"));
    }
    let plan = run.plan_revision.as_deref().unwrap_or_default();
    let key = request_key(node, inputs)?;
    let recorded: Option<Option<String>> = db.query_row(
        "SELECT output_id FROM workflow_integrated_check_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND request_key = ?4",
        params![run.id, node.instance.id, plan, key], |row| row.get(0),
    ).optional()?;
    if let Some(output) = recorded {
        let Some(id) = output else {
            return Ok(None);
        };
        let artifact = load_artifact(
            db,
            &run.project_id,
            &run.id,
            &id,
            ArtifactKind::IntegratedChecksEvidence,
        )?
        .context("The recorded integrated checks evidence is unavailable")?;
        let ArtifactValue::IntegratedChecksEvidence(evidence) = artifact.value else {
            unreachable!()
        };
        return Ok(Some(evidence.verdict.port()));
    }
    let NodeConfiguration::IntegratedChecks(config) = &node.instance.configuration else {
        anyhow::bail!("The pinned node does not verify integrated checks");
    };
    ensure!(
        config.valid(),
        "Choose the distinct registered checks to run"
    );
    let (_, revision) = integrated_input(db, &run.project_id, &run.id, inputs)?;
    let gate_plan = IntegratedGatePlan::load(db, &run.project_id, &config.checks)?;
    db.execute(
        "INSERT INTO workflow_integrated_check_actions
         (pipeline_run_id, node_instance_id, plan_key, request_key, integration_sha, inputs, gate_plan)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
        params![
            run.id, node.instance.id, plan, key, revision.sha,
            protection::protect(&serde_json::to_vec(inputs)?)?,
            protection::protect(&serde_json::to_vec(&gate_plan)?)?,
        ],
    )?;
    Ok(None)
}

pub(super) fn stage_reason(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    inputs: &NodeInputs,
) -> Result<Option<String>> {
    let reason: Option<Option<String>> = db.query_row(
        "SELECT reason FROM workflow_integrated_check_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND request_key = ?4",
        params![run.id, node.instance.id, run.plan_revision.as_deref().unwrap_or_default(), request_key(node, inputs)?],
        |row| row.get(0),
    ).optional()?;
    Ok(reason.unwrap_or_else(|| {
        Some("Enable automatic dispatch when launching a workflow with integrated checks".into())
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegratedChecksWait {
    node_id: String,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<IntegratedChecksWait>> {
    let mut statement = db.prepare(
        "SELECT node_instance_id, reason FROM workflow_integrated_check_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND output_id IS NULL
         ORDER BY node_instance_id, request_key",
    )?;
    Ok(statement
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| {
                Ok(IntegratedChecksWait {
                    node_id: row.get(0)?,
                    reason: row.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Clone)]
pub(crate) struct PendingIntegratedChecks {
    project: String,
    run: String,
    node: String,
    plan: String,
    key: String,
    sha: String,
    inputs: NodeInputs,
    gate_plan: IntegratedGatePlan,
}

impl Store {
    pub(crate) async fn next_workflow_integrated_checks(
        &self,
    ) -> Result<Option<PendingIntegratedChecks>> {
        self.execute(|db| {
            let row = db.query_row(
                "SELECT pin.project_id, a.pipeline_run_id, a.node_instance_id, a.plan_key, a.request_key,
                        a.integration_sha, a.inputs, a.gate_plan
                 FROM workflow_integrated_check_actions a
                 JOIN workflow_run_pins pin ON pin.pipeline_run_id = a.pipeline_run_id
                 JOIN job_pipeline_runs r ON r.id = a.pipeline_run_id AND coalesce(r.plan_revision, '') = a.plan_key
                 WHERE a.output_id IS NULL AND a.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id, a.request_key LIMIT 1",
                [], |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?,
                    row.get::<_, Vec<u8>>(6)?, row.get::<_, Vec<u8>>(7)?,
                )),
            ).optional()?;
            row.map(|(project, run, node, plan, key, sha, inputs, gate_plan)| Ok(PendingIntegratedChecks {
                project, run, node, plan, key, sha,
                inputs: serde_json::from_slice(&protection::unprotect(&inputs)?)?,
                gate_plan: serde_json::from_slice(&protection::unprotect(&gate_plan)?)?,
            })).transpose()
        }).await
    }
}

impl PendingIntegratedChecks {
    fn still_pending(&self, db: &Connection) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_integrated_check_actions
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3
               AND request_key = ?4 AND output_id IS NULL)",
            params![self.run, self.node, self.plan, self.key],
            |row| row.get(0),
        )?)
    }

    fn bound_revision(&self, db: &Connection) -> Result<(String, IntegratedRevision)> {
        let run = load_workflow_run(db, &self.project, &self.run)?
            .context("The integrated checks workflow is unavailable")?;
        ensure!(
            run.plan_revision.as_deref().unwrap_or_default() == self.plan,
            "The integrated checks plan was superseded"
        );
        require_stage(db, &self.project, &self.run, &self.node, None, &self.inputs)?;
        let node = load_pinned_node(db, &self.project, &self.run, &self.node)?
            .context("The integrated checks stage is unavailable")?;
        ensure!(
            request_key(&node, &self.inputs)? == self.key,
            "The integrated checks inputs differ from their selected pins"
        );
        let NodeConfiguration::IntegratedChecks(config) = node.instance.configuration else {
            anyhow::bail!("The pinned node does not verify integrated checks");
        };
        ensure!(
            config.valid() && self.gate_plan.matches_names(&config.checks),
            "The stored check commands differ from the pinned configuration"
        );
        let input = integrated_input(db, &self.project, &self.run, &self.inputs)?;
        ensure!(
            input.1.sha == self.sha,
            "The selected integrated revision differs from the pinned check commit"
        );
        Ok(input)
    }

    pub(crate) async fn execute(&self, store: &Store) -> Result<()> {
        let action = self.clone();
        let prepared = store
            .execute(move |db| {
                if !action.still_pending(db)? {
                    return Ok(None);
                }
                let (artifact_id, revision) = action.bound_revision(db)?;
                let current = action.gate_plan.is_current(db, &action.project)?;
                let common: String = db.query_row(
                    "SELECT git_common_dir FROM projects WHERE id = ?1",
                    [&action.project],
                    |row| row.get(0),
                )?;
                Ok(Some((
                    artifact_id,
                    revision,
                    current,
                    PathBuf::from(common),
                )))
            })
            .await?;
        let Some((integrated_artifact_id, revision, current, common_dir)) = prepared else {
            return Ok(());
        };
        let outcome = if current {
            let plan = self.gate_plan.clone();
            let sha = revision.sha.clone();
            tokio::task::spawn_blocking(move || plan.run(&common_dir, &sha)).await?
        } else {
            self.gate_plan
                .blocked("The project check policy changed before the integrated checks ran")
        };
        let mut evidence = IntegratedChecksEvidence {
            integrated_artifact_id,
            revision,
            policy_revision: self.gate_plan.policy_revision.clone(),
            tree: outcome.tree,
            verdict: outcome.verdict,
            reason: outcome.reason,
            checks: outcome.checks,
        };
        let action = self.clone();
        store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                if !action.still_pending(&tx)? {
                    return Ok(());
                }
                let (artifact_id, revision) = action.bound_revision(&tx)?;
                ensure!(
                    artifact_id == evidence.integrated_artifact_id && revision == evidence.revision,
                    "The selected revision changed while running the integrated checks"
                );
                if current && !action.gate_plan.is_current(&tx, &action.project)? {
                    evidence.verdict = TestVerdict::Blocked;
                    evidence.reason = Some(
                        "The project check policy changed during the integrated checks".into(),
                    );
                }
                let reason = evidence.reason.clone();
                let output = insert_artifact(
                    &tx,
                    &action.project,
                    action.run.clone(),
                    action.node.clone(),
                    "evidence".into(),
                    ArtifactValue::IntegratedChecksEvidence(evidence),
                )?;
                record_workflow_input_ids(&tx, &output, &action.plan, &action.inputs)?;
                tx.execute(
                    "UPDATE workflow_integrated_check_actions SET output_id = ?5, reason = ?6
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3
                   AND request_key = ?4 AND output_id IS NULL",
                    params![
                        action.run,
                        action.node,
                        action.plan,
                        action.key,
                        output.id,
                        reason
                    ],
                )?;
                refresh(&tx, &action.project, &action.run)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    pub(crate) async fn defer(&self, store: &Store, error: &anyhow::Error) -> Result<()> {
        let reason = diagnostics::preview(&error.to_string());
        let action = self.clone();
        store.execute(move |db| {
            db.execute(
                "UPDATE workflow_integrated_check_actions SET reason = ?5, retry_after = unixepoch() + 30
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3
                   AND request_key = ?4 AND output_id IS NULL",
                params![action.run, action.node, action.plan, action.key, reason],
            )?;
            Ok(())
        }).await
    }
}
