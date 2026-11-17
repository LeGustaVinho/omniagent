use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    jobs::{JobRequest, PipelineDispatch},
    protection,
    providers::profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    store::Store,
};

use super::{
    NodeConfiguration, NodeInputs, PinnedNode, WorkflowAgentSubmission, WorkflowArtifact,
    WorkflowRun, digest, generic_agent::stage_job,
};

pub(crate) mod agents_delivery;
pub(crate) mod preparation;
pub(crate) mod unit_actions;

pub(super) fn enabled(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [run],
        |row| row.get(0),
    )?)
}

#[derive(Deserialize, Serialize)]
struct AgentInputs {
    unit: Option<String>,
    inputs: NodeInputs,
}

pub(crate) struct PendingWorkflowAgent {
    project: String,
    run: String,
    node: String,
    key: String,
    request: AgentInputs,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticAgentWait {
    node_id: String,
    unit_id: Option<String>,
    reason: Option<String>,
}

pub(super) fn agent_waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<AutomaticAgentWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, payload, reason FROM workflow_automatic_agents
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND job_id IS NULL ORDER BY node_instance_id, request_key",
    )?;
    query
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or_default()],
            |row| Ok((row.get(0)?, row.get::<_, Vec<u8>>(1)?, row.get(2)?)),
        )?
        .map(|row| {
            let (node_id, payload, reason) = row?;
            let request: AgentInputs = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            Ok(AutomaticAgentWait {
                node_id,
                unit_id: request.unit,
                reason,
            })
        })
        .collect()
}

pub(super) fn note_ready_agent(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<()> {
    if !enabled(db, &run.id)? {
        return Ok(());
    }
    if matches!(
        node.definition.kind,
        super::BuiltinNode::Agent | super::BuiltinNode::ReviewCandidate
    ) && matches!(
        super::launch::load_workflow_launch(db, &run.project_id, &run.id)?
            .map(|receipt| receipt.origin),
        Some(super::WorkflowLaunchOrigin::Conversation { .. })
    ) {
        // Conversation turns select the agent's profile and attach the reviewed chat context.
        return Ok(());
    }
    let plan = run.plan_revision.as_deref().unwrap_or_default();
    let request = AgentInputs {
        unit: unit.map(str::to_owned),
        inputs: inputs
            .iter()
            .map(|(port, value)| (port.clone(), value.id.clone()))
            .collect(),
    };
    if stage_job(
        db,
        &run.project_id,
        &run.id,
        &node.instance.id,
        unit,
        &request.inputs,
    )?
    .is_some()
    {
        return Ok(());
    }
    let key = digest(&serde_json::to_vec(&(&node.revision, plan, &request))?);
    let payload = protection::protect(&serde_json::to_vec(&request)?)?;
    db.execute(
        "INSERT INTO workflow_automatic_agents(pipeline_run_id, node_instance_id, request_key, plan_key, payload)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING",
        params![run.id, node.instance.id, key, plan, payload],
    )?;
    Ok(())
}

impl Store {
    pub(crate) async fn next_workflow_agent(&self) -> Result<Option<PendingWorkflowAgent>> {
        self.execute(|db| {
            let row = db.query_row(
                "SELECT r.project_id, a.pipeline_run_id, a.node_instance_id, a.request_key, a.payload
                 FROM workflow_automatic_agents a JOIN job_pipeline_runs r ON r.id = a.pipeline_run_id
                 WHERE a.job_id IS NULL AND a.retry_after <= unixepoch()
                   AND a.plan_key = coalesce(r.plan_revision, '')
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Vec<u8>>(4)?)),
            ).optional()?;
            row.map(|(project, run, node, key, payload)| Ok(PendingWorkflowAgent {
                project,
                run,
                node,
                key,
                request: serde_json::from_slice(&protection::unprotect(&payload)?)?,
            })).transpose()
        }).await
    }
}

impl PendingWorkflowAgent {
    pub(crate) async fn prepare(
        &self,
        store: &Store,
        runner: String,
    ) -> Result<WorkflowAgentSubmission> {
        let target = store
            .workflow_operator_agent_target(
                &self.project,
                &self.run,
                &self.node,
                self.request.unit.as_deref(),
                &self.request.inputs,
            )
            .await?;
        let pinned = store
            .pinned_workflow_node(self.project.clone(), self.run.clone(), self.node.clone())
            .await?
            .context("The automatic stage revision is unavailable")?;
        let NodeConfiguration::AgentRole {
            model: Some(selection),
            requirements,
            ..
        } = pinned.instance.configuration
        else {
            anyhow::bail!("Set the Agent model on the published workflow for automatic dispatch");
        };
        let effort = if let Some(role) = requirements.bound_role {
            let project = self.project.clone();
            store
                .execute(move |db| {
                    let template =
                        super::authoring::package::roles::load_role(db, &project, &role)?
                            .context("The pinned Agent role is unavailable")?;
                    Ok(template.effort)
                })
                .await?
        } else {
            None
        };
        let configured = store.configured_executable(selection.provider).await?;
        let model_id = selection.model_id.clone();
        let profile =
            tokio::task::spawn_blocking(move || desktop_profile(configured, Some(model_id)))
                .await??;
        let model = profile
            .models
            .iter()
            .find(|model| model.option.selection == selection && model.option.selectable)
            .context("The saved Agent model or profile is unavailable")?;
        ensure!(
            effort
                .as_ref()
                .is_none_or(|effort| model.efforts.contains(effort)),
            "The saved thinking level is unavailable"
        );
        let scope = SessionScope {
            provider: selection.provider,
            profile_id: selection.profile_id,
            account_binding: profile.account_binding,
            checkout: crate::repository::checkout_root(store, target.checkout_id.clone()).await?,
            executable: profile.executable,
            cli_version: profile.version,
            runner,
            permission_revision: DESKTOP_PERMISSION_REVISION.into(),
            mode: target.mode,
            model: Some(selection.model_id),
            effort,
        };
        Ok(self.submission(target, scope))
    }

    fn submission(
        &self,
        target: super::operator::WorkflowAgentTarget,
        scope: SessionScope,
    ) -> WorkflowAgentSubmission {
        WorkflowAgentSubmission {
            project_id: self.project.clone(),
            dispatch: PipelineDispatch {
                pipeline_run_id: self.run.clone(),
                node_instance_id: self.node.clone(),
                plan_revision: target.plan_revision,
                release_id: target.release_id,
            },
            scope,
            request: JobRequest {
                checkout_id: Some(target.checkout_id),
                task_id: target.task_id,
                mode: target.mode,
                instructions: agents_delivery::instructions(target.kind),
                dependencies: Vec::new(),
                requires_bridge: false,
            },
            input: Default::default(),
            artifacts: self.request.inputs.clone(),
            unit_id: self.request.unit.clone(),
        }
    }

    pub(crate) async fn reconcile(&self, store: &Store) -> Result<bool> {
        let (project, run, node, key, unit, inputs) = (
            self.project.clone(),
            self.run.clone(),
            self.node.clone(),
            self.key.clone(),
            self.request.unit.clone(),
            self.request.inputs.clone(),
        );
        store
            .execute(move |db| {
                let Some(job) = stage_job(db, &project, &run, &node, unit.as_deref(), &inputs)?
                else {
                    return Ok(false);
                };
                confirm_agent(db, &run, &node, &key, &job)?;
                Ok(true)
            })
            .await
    }

    pub(crate) async fn enqueue(
        &self,
        store: &Store,
        submission: WorkflowAgentSubmission,
    ) -> Result<()> {
        if store
            .try_reuse_automatic_review(&submission, &self.key)
            .await?
        {
            return Ok(());
        }
        store
            .submit_automatic_workflow_agent(submission, self.key.clone())
            .await?;
        Ok(())
    }

    pub(crate) async fn defer(&self, store: &Store, error: &anyhow::Error) -> Result<()> {
        if self.reconcile(store).await? {
            return Ok(());
        }
        let (run, node, key, reason) = (
            self.run.clone(),
            self.node.clone(),
            self.key.clone(),
            crate::diagnostics::preview(&error.to_string()),
        );
        store.execute(move |db| {
            db.execute(
                "UPDATE workflow_automatic_agents SET reason = ?4, retry_after = unixepoch() + 30
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3 AND job_id IS NULL
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)",
                params![run, node, key, reason],
            )?;
            Ok(())
        }).await
    }
}

pub(crate) fn require_pending_agent(
    db: &Connection,
    binding: &PipelineDispatch,
    key: &str,
    input: &super::job_inputs::WorkflowJobInput,
) -> Result<()> {
    let payload: Vec<u8> = db
        .query_row(
            "SELECT a.payload FROM workflow_automatic_agents a
         JOIN job_pipeline_runs r ON r.id = a.pipeline_run_id
         WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.request_key = ?3
           AND a.plan_key = coalesce(r.plan_revision, '') AND a.job_id IS NULL
           AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)",
            params![binding.pipeline_run_id, binding.node_instance_id, key],
            |row| row.get(0),
        )
        .optional()?
        .context("Automatic dispatch is paused or its selected inputs changed")?;
    let request: AgentInputs = serde_json::from_slice(&protection::unprotect(&payload)?)?;
    ensure!(
        request.unit == input.unit_id && request.inputs == input.artifacts,
        "Automatic dispatch inputs do not match the selected artifacts"
    );
    Ok(())
}

pub(crate) fn confirm_agent(
    db: &Connection,
    run: &str,
    node: &str,
    key: &str,
    job: &str,
) -> Result<()> {
    ensure!(
        db.execute(
            "UPDATE workflow_automatic_agents SET job_id = ?4, reason = NULL
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3
           AND (job_id IS NULL OR job_id = ?4)",
            params![run, node, key, job],
        )? == 1,
        "The automatic stage lost its dispatch identity"
    );
    Ok(())
}
