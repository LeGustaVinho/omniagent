use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};

use crate::store::Store;

use super::{
    ArtifactValue, PinnedNode, PortType, WorkflowArtifact, approval::require_active_decision,
    registry::supports_pinned_execution,
};

pub type NodeInputs = BTreeMap<String, String>;

impl Store {
    pub(super) async fn workflow_execution_inputs(
        &self,
        project: &str,
        run: &str,
        node: &str,
        unit_id: Option<&str>,
        artifacts: &NodeInputs,
    ) -> Result<(PinnedNode, BTreeMap<String, WorkflowArtifact>)> {
        let workflow = self
            .workflow_run(project.into(), run.into())
            .await?
            .context("workflow run is outside this project")?;
        let definition = self
            .workflow_definition(project.into(), workflow.version.clone())
            .await?
            .context("workflow definition is unavailable")?;
        definition.ensure_supported_nodes()?;
        let pinned = self
            .pinned_workflow_node(project.into(), run.into(), node.into())
            .await?
            .context("workflow node is outside this run")?;
        ensure!(
            supports_pinned_execution(&pinned.definition),
            "the pinned executor definition is unavailable"
        );
        let unit_nodes = super::control::unit_nodes(&definition);
        let agent_stage = pinned.definition.kind == super::BuiltinNode::Agent;
        ensure!(
            (if agent_stage {
                unit_nodes.contains(node)
            } else {
                pinned.definition.kind.per_unit()
            }) == unit_id.is_some(),
            "workflow node has the wrong logical unit scope"
        );
        let control_checked = if super::control::uses_control(&definition) {
            self.refresh_control(project.into(), run.into()).await?;
            let (node, unit, supplied) = (
                node.to_owned(),
                unit_id.map(str::to_owned),
                artifacts.clone(),
            );
            let (definition, workflow) = (definition.clone(), workflow.clone());
            self.execute(move |db| {
                super::control::require_dependencies(
                    db,
                    &definition,
                    &workflow,
                    &node,
                    unit.as_deref(),
                    &supplied,
                )
            })
            .await?
        } else {
            false
        };
        let mut resolved = BTreeMap::new();
        for port in &pinned.definition.inputs {
            let source = definition
                .wires
                .iter()
                .find(|wire| wire.to.node == node && wire.to.port == port.name);
            if source.is_none() && agent_stage && super::generic_agent::optional_input(&port.name) {
                continue;
            }
            let source = source.context("pinned workflow input has no source")?;
            let producer = definition
                .nodes
                .iter()
                .find(|pinned| pinned.instance.id == source.from.node)
                .context("pinned workflow input producer is unavailable")?;
            let producer_unit = if unit_nodes.contains(producer.instance.id.as_str()) {
                unit_id
            } else {
                None
            };
            match port.value {
                PortType::Control => {
                    if control_checked {
                        continue;
                    }
                    let (run, node, unit) = (
                        run.to_owned(),
                        source.from.node.clone(),
                        producer_unit.map(str::to_owned),
                    );
                    let plan_revision = workflow.plan_revision.clone();
                    let complete: bool = self.execute(move |db| Ok(db.query_row(
                        "SELECT EXISTS (
                             SELECT 1 FROM workflow_artifacts a
                             WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.unit_id IS ?3
                             UNION ALL
                             SELECT 1 FROM workflow_command_results c
                             JOIN job_attempts a ON a.run_id = c.run_id
                             JOIN job_pipeline_origins p ON p.job_id = a.job_id
                             WHERE c.pipeline_run_id = ?1 AND c.node_instance_id = ?2 AND ?3 IS NULL
                               AND p.plan_revision IS ?4
                         )", params![run, node, unit, plan_revision], |row| row.get(0),
                    )?)).await?;
                    ensure!(
                        complete,
                        "control input {} is waiting for its source",
                        port.name
                    );
                }
                PortType::Data(kind) => {
                    let id = artifacts.get(&port.name).with_context(|| {
                        format!("workflow input {} needs an artifact", port.name)
                    })?;
                    let artifact = self
                        .resolve_workflow_artifact(project.into(), run.into(), id.clone(), kind)
                        .await?
                        .with_context(|| {
                            format!("artifact for workflow input {} is unavailable", port.name)
                        })?;
                    ensure!(
                        artifact.node_instance_id == source.from.node
                            && artifact.port == source.from.port
                            && artifact.unit_id.as_deref() == producer_unit,
                        "artifact for workflow input {} is outside its pinned wire",
                        port.name
                    );
                    resolved.insert(port.name.clone(), artifact);
                }
                PortType::Evidence => {
                    anyhow::bail!("Evidence inputs are handled by the workflow scheduler")
                }
            }
        }
        ensure!(
            resolved.len() == artifacts.len(),
            "workflow submission contains an unknown data input"
        );
        if let Some(plan) = resolved.get("plan") {
            ensure!(
                workflow.plan_revision.as_ref() == Some(&plan.id),
                "the pinned wire must receive the currently selected plan"
            );
        }
        if let Some(approval) = resolved.get("decision")
            && let ArtifactValue::Approval(decision) = &approval.value
        {
            let (decision, id) = (decision.clone(), approval.id.clone());
            self.execute(move |db| require_active_decision(db, &decision, &id))
                .await?;
        }
        Ok((pinned, resolved))
    }
}

pub(super) fn record_host_completion(
    db: &Connection,
    artifact: &WorkflowArtifact,
    execution_key: &str,
    request_revision: &str,
) -> Result<()> {
    db.execute(
        "INSERT INTO workflow_host_completions VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            artifact.workflow_run_id,
            artifact.node_instance_id,
            execution_key,
            request_revision,
            artifact.id
        ],
    )?;
    Ok(())
}
