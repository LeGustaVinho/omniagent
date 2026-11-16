use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::watch};
use uuid::Uuid;

use crate::{
    jobs::{Job, LaunchGate, PipelineDispatch, queue_pipeline_mcp, record_launch_intent},
    providers::SessionMode,
    runner::Runner,
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{
    McpToolConfiguration, McpToolContract, SavedServer, WorkflowArtifact, WorkflowMcpToolResult,
    binding, destination, digest, protocol::Client, seal, unseal,
};
use crate::workflows::{ArtifactValue, NodeConfiguration, NodeInputs, WorkflowJobInput};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct McpInvocation {
    config: McpToolConfiguration,
    server: SavedServer,
    contract: McpToolContract,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowMcpToolSubmission {
    pub project_id: String,
    pub run_id: String,
    pub node_id: String,
    pub checkout_id: String,
    pub inputs: NodeInputs,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpJobReview {
    pub job_id: String,
    pub server_name: String,
    pub tool_name: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    pub destination: Option<String>,
    pub decision: String,
    pub outbound_network: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAttemptEvidence {
    pub run_id: String,
    pub state: String,
    pub artifact: Option<WorkflowArtifact>,
}

impl Store {
    pub async fn submit_workflow_mcp_tool(
        &self,
        submission: WorkflowMcpToolSubmission,
    ) -> Result<Job> {
        let WorkflowMcpToolSubmission {
            project_id,
            run_id,
            node_id,
            checkout_id,
            inputs,
        } = submission;
        let (node, resolved) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, None, &inputs)
            .await?;
        let NodeConfiguration::McpTool(config) = node.instance.configuration else {
            anyhow::bail!("The pinned workflow node is not a local MCP tool")
        };
        let outcome = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let config = config.with_resolved_arguments(resolved.get("arguments"))?;
            let (server, contract) = binding(&tx, &project_id, &config)?;
            let workflow = super::super::runtime::load_workflow_run(&tx, &project_id, &run_id)?
                .context("The MCP tool workflow run is unavailable")?;
            let pipeline = PipelineDispatch { pipeline_run_id: run_id, node_instance_id: node_id, plan_revision: workflow.plan_revision, release_id: None };
            let execution = digest(&serde_json::to_vec(&(&checkout_id, &pipeline.plan_revision, &inputs, &config))?);
            let previous: Option<String> = tx.query_row("SELECT job_id FROM workflow_mcp_jobs WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_digest = ?3",
                params![pipeline.pipeline_run_id, pipeline.node_instance_id, execution], |row| row.get(0)).optional()?;
            if let Some(id) = previous {
                return crate::jobs::load_job(&tx, &project_id, &id)?.context("The existing MCP tool job is unavailable");
            }
            ensure!(!unresolved_effect(&tx, &pipeline.pipeline_run_id, &pipeline.node_instance_id, None)?,
                "This workflow node has an unresolved external effect");
            let grant = grant_id(&tx, &project_id, &config)?;
            let invocation = McpInvocation { config, server, contract };
            let job = queue_pipeline_mcp(&tx, &project_id, &pipeline, checkout_id,
                &WorkflowJobInput { unit_id: None, artifacts: inputs, plan_revision_request: None, comparison: None },
                &invocation.server.summary.executable, &invocation.server.executable_revision)?;
            tx.execute("INSERT INTO workflow_mcp_jobs VALUES (?1,?2,?3,?4,?5,?6,?7)", params![job.id,
                pipeline.pipeline_run_id, pipeline.node_instance_id, execution,
                if grant.is_some() { "preauthorized" } else { "pending" }, grant, seal(&invocation)?])?;
            if grant.is_none() { tx.execute("UPDATE jobs SET queue_state = 'paused' WHERE id = ?1", [&job.id])?; }
            let loaded = crate::jobs::load_job(&tx, &project_id, &job.id)?.context("The MCP tool job is unavailable")?;
            tx.commit()?;
            Ok(loaded)
        }).await?;
        self.job_signal.send_replace(());
        Ok(outcome)
    }

    pub async fn set_workflow_mcp_grant(
        &self,
        project: String,
        configuration: McpToolConfiguration,
        allowed: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            ensure!(
                configuration.argument_mappings.is_empty(),
                "Review the resolved tool call before setting a grant"
            );
            binding(db, &project, &configuration)?;
            write_grant(db, &project, &configuration, allowed)
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn set_workflow_mcp_job_grant(
        &self,
        project: String,
        job: String,
        allowed: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let (invocation, _, _) = job_for_review(db, &project, &job)?;
            write_grant(db, &project, &invocation.config, allowed)
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn workflow_mcp_job_review(
        &self,
        project: String,
        job: String,
    ) -> Result<McpJobReview> {
        self.execute(move |db| {
            let (invocation, decision, _) = job_for_review(db, &project, &job)?;
            Ok(McpJobReview {
                job_id: job,
                server_name: invocation.server.summary.name,
                outbound_network: invocation.server.summary.outbound_network,
                tool_name: invocation.config.tool_name.clone(),
                destination: destination(&invocation.config)?,
                arguments: invocation.config.arguments,
                decision,
            })
        })
        .await
    }

    pub async fn decide_workflow_mcp_job(
        &self,
        project: String,
        job: String,
        approve: bool,
    ) -> Result<()> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (_, decision, _) = job_for_review(&tx, &project, &job)?;
            ensure!(decision == "pending" || decision == "preauthorized", "The MCP tool approval is already settled");
            let changed = tx.execute("UPDATE jobs SET queue_state = ?2 WHERE id = ?1 AND queue_state IN ('paused','queued')
                AND NOT EXISTS(SELECT 1 FROM job_attempts WHERE job_id = ?1)", params![job, if approve {"queued"} else {"cancelled"}])?;
            ensure!(changed == 1, "The MCP tool job no longer awaits approval");
            tx.execute("UPDATE workflow_mcp_jobs SET decision = ?2, grant_id = NULL WHERE job_id = ?1",
                params![job, if approve {"approved"} else {"denied"}])?;
            tx.commit()?;
            Ok(())
        }).await?;
        self.job_signal.send_replace(());
        Ok(())
    }

    pub async fn workflow_mcp_attempt_evidence(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<McpAttemptEvidence>> {
        self.execute(move |db| {
            let row = db.query_row("SELECT a.state, a.payload FROM workflow_mcp_attempts a JOIN jobs j ON j.id = a.job_id
                WHERE j.project_id = ?1 AND a.run_id = ?2", params![project, run], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))).optional()?;
            row.map(|(state, payload)| Ok(McpAttemptEvidence { run_id: run, state,
                artifact: payload.as_deref().map(unseal).transpose()? })).transpose()
        }).await
    }

    pub async fn reconcile_workflow_mcp_attempt(&self, project: String, run: String) -> Result<()> {
        self.execute(move |db| {
            let changed = db.execute("UPDATE workflow_mcp_attempts SET state = 'reconciled' WHERE run_id = ?1 AND state = 'uncertain'
                AND EXISTS(SELECT 1 FROM jobs j WHERE j.id = workflow_mcp_attempts.job_id AND j.project_id = ?2)", params![run, project])?;
            ensure!(changed == 1, "This MCP effect is not awaiting reconciliation in the selected project");
            Ok(())
        }).await
    }
}

fn job_for_review(
    db: &Connection,
    project: &str,
    job: &str,
) -> Result<(McpInvocation, String, Option<String>)> {
    let (payload, decision, grant): (Vec<u8>, String, Option<String>) = db.query_row(
        "SELECT m.payload, m.decision, m.grant_id FROM workflow_mcp_jobs m JOIN jobs j ON j.id = m.job_id WHERE j.project_id = ?1 AND m.job_id = ?2",
        params![project, job], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional()?.context("The MCP tool job is unavailable in this project")?;
    Ok((unseal(&payload)?, decision, grant))
}

fn destination_key(configuration: &McpToolConfiguration) -> Result<Option<String>> {
    destination(configuration)?
        .map(|scope| {
            Ok(digest(&serde_json::to_vec(&(
                &configuration.destination_pointer,
                scope,
            ))?))
        })
        .transpose()
}

fn write_grant(
    db: &Connection,
    project: &str,
    config: &McpToolConfiguration,
    allowed: bool,
) -> Result<()> {
    let key = destination_key(config)?
        .context("Pre-authorization needs an explicit mapped destination")?;
    if allowed {
        db.execute(
            "INSERT INTO workflow_mcp_grants VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT DO NOTHING",
            params![
                project,
                config.server_id,
                config.server_revision,
                config.tool_name,
                config.contract_revision,
                key,
                Uuid::new_v4().to_string()
            ],
        )?;
    } else {
        db.execute("DELETE FROM workflow_mcp_grants WHERE project_id = ?1 AND server_id = ?2 AND server_revision = ?3 AND tool_name = ?4 AND contract_revision = ?5 AND destination_digest = ?6",
            params![project, config.server_id, config.server_revision, config.tool_name, config.contract_revision, key])?;
    }
    Ok(())
}

fn unresolved_effect(
    db: &Connection,
    pipeline: &str,
    node: &str,
    except_job: Option<&str>,
) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM workflow_mcp_jobs m JOIN workflow_mcp_attempts a ON a.job_id = m.job_id
      WHERE m.pipeline_run_id = ?1 AND m.node_instance_id = ?2 AND (?3 IS NULL OR m.job_id <> ?3)
      AND a.state IN ('sent','uncertain','reconciled'))", params![pipeline, node, except_job], |row| row.get(0))?)
}

fn grant_id(
    db: &Connection,
    project: &str,
    config: &McpToolConfiguration,
) -> Result<Option<String>> {
    let Some(destination) = destination_key(config)? else {
        return Ok(None);
    };
    Ok(db.query_row("SELECT id FROM workflow_mcp_grants WHERE project_id = ?1 AND server_id = ?2 AND server_revision = ?3 AND tool_name = ?4 AND contract_revision = ?5 AND destination_digest = ?6",
        params![project, config.server_id, config.server_revision, config.tool_name, config.contract_revision, destination], |row| row.get(0)).optional()?)
}

pub(crate) fn mcp_admission_blocker(db: &Connection, job: &Job) -> Result<Option<&'static str>> {
    let origin: Option<(String, String)> = db
        .query_row(
            "SELECT pipeline_run_id, node_instance_id FROM workflow_mcp_jobs WHERE job_id = ?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((pipeline, node)) = origin else {
        return Ok(None);
    };
    if unresolved_effect(db, &pipeline, &node, Some(&job.id))? {
        return Ok(Some("This MCP node has an unresolved external effect"));
    }
    let current = super::super::runtime::load_workflow_run(db, &job.project_id, &pipeline)?
        .context("The MCP tool workflow run is unavailable")?;
    if job
        .pipeline
        .as_ref()
        .is_none_or(|origin| origin.plan_revision != current.plan_revision)
    {
        return Ok(Some(
            "The selected workflow plan changed after MCP call preparation",
        ));
    }
    let (invocation, decision, saved_grant) = job_for_review(db, &job.project_id, &job.id)?;
    if decision == "approved"
        || (decision == "preauthorized"
            && saved_grant.is_some()
            && grant_id(db, &job.project_id, &invocation.config)? == saved_grant)
    {
        Ok(None)
    } else {
        Ok(Some("This MCP tool invocation needs explicit confirmation"))
    }
}

pub(crate) fn is_mcp_job(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_mcp_jobs WHERE job_id = ?1)",
        [id],
        |row| row.get(0),
    )?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_isolated_mcp(
    app: &Handle,
    store: &Store,
    runner: &dyn Runner,
    run: &RunRecord,
    admission: &LaunchGate,
    interrupt: &watch::Receiver<bool>,
    output: &mut Option<WorkflowMcpToolResult>,
) -> Result<RunStatus> {
    let run_id = run.id.clone();
    let project = run.project_id.clone();
    let (job, invocation) = app.block_on(store.execute(move |db| {
        let job_id: String = db.query_row(
            "SELECT job_id FROM job_attempts WHERE run_id = ?1",
            [&run_id],
            |row| row.get(0),
        )?;
        let job = crate::jobs::load_job(db, &project, &job_id)?.context("Missing MCP job")?;
        ensure!(
            job.mode == SessionMode::ReadOnly && mcp_admission_blocker(db, &job)?.is_none(),
            "MCP authorization changed"
        );
        let (invocation, _, _) = job_for_review(db, &project, &job_id)?;
        Ok((job, invocation))
    }))?;
    let stopping = || admission.is_closed() || *interrupt.borrow();
    if stopping() {
        return Ok(RunStatus::Interrupted);
    }
    {
        let Some(_boundary) = app.block_on(admission.enter()) else {
            return Ok(RunStatus::Interrupted);
        };
        let id = run.id.clone();
        let launch_job = job.clone();
        let allowed = app.block_on(store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                mcp_admission_blocker(&tx, &launch_job)?.is_none(),
                "MCP authorization changed before launch"
            );
            let allowed = record_launch_intent(&tx, &id)?;
            if allowed == Some(true) {
                tx.execute(
                    "INSERT INTO workflow_mcp_attempts VALUES (?1,?2,'preparing',NULL,NULL)",
                    params![id, launch_job.id],
                )?;
            }
            tx.commit()?;
            Ok(allowed == Some(true))
        }))?;
        ensure!(allowed, "MCP launch authorization changed");
    }
    let mut client = Client::connect(
        &invocation.server,
        runner,
        invocation.server.summary.outbound_network,
        &stopping,
    )?;
    let tools = client.tools(&stopping)?;
    ensure!(
        tools.iter().any(|tool| tool == &invocation.contract),
        "The local MCP tool schema changed. Review and publish an updated binding."
    );
    {
        let Some(_boundary) = app.block_on(admission.enter()) else {
            return Ok(RunStatus::Interrupted);
        };
        if stopping() {
            return Ok(RunStatus::Interrupted);
        }
        let id = run.id.clone();
        app.block_on(store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(mcp_admission_blocker(&tx, &job)?.is_none(), "MCP pre-authorization changed before invocation");
            ensure!(record_launch_intent(&tx, &id)? == Some(true), "MCP effect authorization changed before invocation");
            let changed = tx.execute("UPDATE workflow_mcp_attempts SET state = 'sent' WHERE run_id = ?1 AND job_id = ?2 AND state = 'preparing'", params![id,job.id])?;
            ensure!(changed == 1, "MCP invocation is already settled");
            let run = runs::load_run(&tx, &id)?;
            runs::advance_run(&tx, &run, RunStatus::Running)?;
            tx.commit()?;
            Ok(())
        }))?;
    }
    let response = client.call(
        &invocation.contract.name,
        &invocation.config.arguments,
        &stopping,
    )?;
    let result = invocation.contract.validate_result(response)?;
    let status = if result.rejected {
        RunStatus::Failed
    } else {
        RunStatus::Succeeded
    };
    *output = Some(WorkflowMcpToolResult {
        run_id: run.id.clone(),
        output: result,
    });
    Ok(status)
}

pub(crate) fn settle_mcp_result(
    db: &Connection,
    run: &str,
    result: WorkflowMcpToolResult,
) -> Result<()> {
    let (pipeline, node): (String, String) = db.query_row(
        "SELECT j.pipeline_run_id, j.node_instance_id FROM workflow_mcp_attempts a
        JOIN workflow_mcp_jobs j ON j.job_id = a.job_id WHERE a.run_id = ?1 AND a.state = 'sent'",
        [run],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let state = if result.output.rejected {
        "rejected"
    } else {
        "confirmed"
    };
    let artifact = WorkflowArtifact {
        id: digest(&serde_json::to_vec(&(run, &pipeline, &node, &result))?),
        workflow_run_id: pipeline,
        node_instance_id: node,
        port: "result".into(),
        unit_id: None,
        value: ArtifactValue::McpToolResult(result),
    };
    db.execute("UPDATE workflow_mcp_attempts SET state = ?2, artifact_id = ?3, payload = ?4 WHERE run_id = ?1",
        params![run, state, artifact.id, seal(&artifact)?])?;
    Ok(())
}

pub(crate) fn settle_mcp_restart(db: &Connection, run: &str) -> Result<()> {
    db.execute("UPDATE workflow_mcp_attempts SET state = CASE state WHEN 'sent' THEN 'uncertain' ELSE 'not_sent' END
        WHERE run_id = ?1 AND state IN ('sent','preparing')", [run])?;
    Ok(())
}

pub(crate) fn load_mcp_artifact(
    db: &Connection,
    project: &str,
    pipeline: &str,
    id: &str,
) -> Result<Option<WorkflowArtifact>> {
    let payload: Option<Vec<u8>> = db.query_row("SELECT a.payload FROM workflow_mcp_attempts a JOIN workflow_mcp_jobs j ON j.job_id = a.job_id
        JOIN workflow_run_pins w ON w.pipeline_run_id = j.pipeline_run_id WHERE a.artifact_id = ?1 AND w.project_id = ?2 AND w.pipeline_run_id = ?3",
        params![id, project, pipeline], |row| row.get(0)).optional()?;
    payload.as_deref().map(unseal).transpose()
}
