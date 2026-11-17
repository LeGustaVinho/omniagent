use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    protection, repository::verification::load_scoped_candidate_verification, store::Store,
};

use super::{
    super::{
        ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, PinnedNode, WorkflowArtifact,
        WorkflowRun, WorkflowUnitOutcome, WorkflowUnitRequest, WorkflowUnitSubmission,
        control::{self, ControlStageState},
        digest,
        generic_agent::AgentStageOutcome,
        runtime::{load_pinned_node, load_workflow_run},
        units::{WorkflowDeliveryUnit, load_delivery_unit},
    },
    AutomaticAgentWait, enabled,
};

fn ports(kind: BuiltinNode) -> Option<(&'static str, &'static str)> {
    match kind {
        BuiltinNode::VerifyCandidate => Some(("verified", "checks")),
        BuiltinNode::AcceptCandidate => Some(("accepted", "acceptance")),
        BuiltinNode::VerifyCommit => Some(("verified", "git")),
        _ => None,
    }
}

pub(in crate::workflows) fn enabled_for(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
) -> Result<bool> {
    Ok(ports(node.definition.kind).is_some()
        && node
            .definition
            .outputs
            .iter()
            .any(|port| port.name == "failed")
        && enabled(db, &run.id)?)
}

pub(in crate::workflows) fn evaluate(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<Option<AgentStageOutcome>> {
    let (Some(unit), Some(plan), Some((control, output))) =
        (unit, &run.plan_revision, ports(node.definition.kind))
    else {
        return Ok(None);
    };
    if !enabled_for(db, run, node)? {
        return Ok(None);
    }
    let Some(ArtifactValue::Candidate(candidate)) =
        inputs.get("candidate").map(|artifact| &artifact.value)
    else {
        return Ok(None);
    };
    let task = &candidate.target.task_id;
    ensure!(
        candidate.target.project_id == run.project_id && candidate.target.unit_id == unit,
        "The selected candidate belongs to a different work scope"
    );
    let selected: NodeInputs = inputs
        .iter()
        .map(|(port, artifact)| (port.clone(), artifact.id.clone()))
        .collect();
    let encoded = serde_json::to_vec(&selected)?;
    let key = request_key(node, plan, unit, &selected)?;
    let node_id = &node.instance.id;
    let state: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT state, diagnostic FROM workflow_automatic_unit_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3",
            params![run.id, node_id, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((state, reason)) = state.as_ref()
        && state == "failed"
    {
        return Ok(Some(AgentStageOutcome {
            state: ControlStageState::Failed,
            port: "failed",
            reason: reason.clone(),
            source_run_id: None,
        }));
    }
    let saved: Option<Vec<u8>> = db.query_row(
        "SELECT a.payload FROM workflow_artifacts a JOIN workflow_completion_inputs c ON c.output_artifact_id = a.id
         WHERE a.pipeline_run_id = ?1 AND a.node_instance_id = ?2 AND a.unit_id = ?3 AND a.port_name = ?4
           AND c.execution_key = ?5 AND c.input_revision = ?6 ORDER BY a.id LIMIT 1",
        params![run.id, node_id, unit, output, plan, digest(&encoded)], |row| row.get(0),
    ).optional()?;
    let selected_output: Option<WorkflowArtifact> = saved
        .map(|bytes| -> Result<_> { Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?) })
        .transpose()?;
    let (next, diagnostic) = if let Some(artifact) = &selected_output {
        let rejected = if let ArtifactValue::Checks(checks) = &artifact.value {
            let verification =
                load_scoped_candidate_verification(db, &run.project_id, task, &checks.id)?
                    .context("The recorded candidate verification is unavailable")?;
            ensure!(
                verification.candidate_id == candidate.id,
                "The check result is for a different candidate"
            );
            !verification.passes_stage_checks()
        } else {
            false
        };
        let diagnostic =
            rejected.then_some("One or more required candidate checks failed or could not run");
        ("published", diagnostic)
    } else if state.is_some() {
        return Ok(None);
    } else if allowed(db, run, node, inputs.get("acceptance"))? {
        ("pending", None)
    } else {
        (
            "operator",
            Some("The selected stage requires an operator action"),
        )
    };
    db.execute(
        "INSERT INTO workflow_automatic_unit_actions(pipeline_run_id, node_instance_id, request_key,
            plan_key, unit_id, task_id, inputs, state, output_id, diagnostic)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(pipeline_run_id, node_instance_id, request_key) DO UPDATE
            SET state = excluded.state, output_id = excluded.output_id, diagnostic = excluded.diagnostic",
        params![run.id, node_id, key, plan, unit, task, protection::protect(&encoded)?, next,
            selected_output.as_ref().map(|artifact| &artifact.id), diagnostic],
    )?;
    Ok(selected_output.map(|_| AgentStageOutcome {
        // A negative check verdict is still an evidence-bearing result that can feed repair.
        state: ControlStageState::Completed,
        port: if diagnostic.is_some() {
            "failed"
        } else {
            control
        },
        reason: diagnostic.map(str::to_owned),
        source_run_id: None,
    }))
}

fn allowed(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    acceptance: Option<&WorkflowArtifact>,
) -> Result<bool> {
    match node.definition.kind {
        BuiltinNode::VerifyCandidate => Ok(true),
        BuiltinNode::AcceptCandidate => Ok(matches!(
            node.instance.configuration,
            NodeConfiguration::CandidateAcceptance(_)
        )),
        BuiltinNode::VerifyCommit => {
            let Some(acceptance) = acceptance else {
                return Ok(false);
            };
            let source =
                load_pinned_node(db, &run.project_id, &run.id, &acceptance.node_instance_id)?;
            Ok(source.is_some_and(|source| {
                matches!(
                    source.instance.configuration,
                    NodeConfiguration::CandidateAcceptance(_)
                )
            }))
        }
        _ => Ok(false),
    }
}

fn request_key(node: &PinnedNode, plan: &str, unit: &str, inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(
        &node.revision,
        plan,
        unit,
        inputs,
    ))?))
}

pub(in crate::workflows) fn sealed_output(
    db: &Connection,
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
    port: &str,
) -> Result<Option<WorkflowArtifact>> {
    let bytes: Option<Vec<u8>> = db.query_row(
        "SELECT a.payload FROM workflow_automatic_unit_actions x JOIN workflow_artifacts a ON a.id = x.output_id
         WHERE x.pipeline_run_id = ?1 AND x.node_instance_id = ?2 AND x.unit_id IS ?3
           AND x.plan_key = ?4 AND x.state = 'published' AND a.port_name = ?5",
        params![run.id, node, unit, run.plan_revision, port], |row| row.get(0),
    ).optional()?;
    bytes
        .map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?))
        .transpose()
}

pub(in crate::workflows) fn waits(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<AutomaticAgentWait>> {
    let mut statement = db.prepare(
        "SELECT node_instance_id, unit_id, coalesce(diagnostic, 'Waiting for the delivery action')
         FROM workflow_automatic_unit_actions WHERE pipeline_run_id = ?1 AND plan_key = ?2
           AND state IN ('pending', 'operator') ORDER BY node_instance_id, unit_id",
    )?;
    Ok(statement
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(AutomaticAgentWait {
                node_id: row.get(0)?,
                unit_id: Some(row.get(1)?),
                reason: Some(row.get(2)?),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) struct PendingUnitAction {
    project: String,
    run: String,
    node: String,
    key: String,
    plan: String,
    unit: String,
    task: String,
    inputs: NodeInputs,
}

impl Store {
    pub(crate) async fn next_workflow_unit_action(&self) -> Result<Option<PendingUnitAction>> {
        self.execute(|db| {
            let row = db.query_row(
                "SELECT w.project_id, a.pipeline_run_id, a.node_instance_id, a.request_key, a.plan_key,
                        a.unit_id, a.task_id, a.inputs
                 FROM workflow_automatic_unit_actions a JOIN job_pipeline_runs w
                   ON w.id = a.pipeline_run_id AND w.plan_revision = a.plan_key
                 WHERE a.state = 'pending' AND a.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id, a.unit_id LIMIT 1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get::<_, Vec<u8>>(7)?)),
            ).optional()?;
            row.map(|(project, run, node, key, plan, unit, task, bytes)| Ok(PendingUnitAction {
                project, run, node, key, plan, unit, task,
                inputs: serde_json::from_slice(&protection::unprotect(&bytes)?)?,
            })).transpose()
        }).await
    }
}

impl PendingUnitAction {
    fn validate(&self, db: &Connection) -> Result<Option<(PinnedNode, WorkflowDeliveryUnit)>> {
        let current: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_automatic_unit_actions WHERE pipeline_run_id = ?1
               AND node_instance_id = ?2 AND request_key = ?3 AND state = 'pending'
               AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1))",
            params![self.run, self.node, self.key],
            |row| row.get(0),
        )?;
        if !current {
            return Ok(None);
        }
        let run = load_workflow_run(db, &self.project, &self.run)?
            .context("The delivery workflow is unavailable")?;
        let node = load_pinned_node(db, &self.project, &self.run, &self.node)?
            .context("The delivery stage is unavailable")?;
        ensure!(
            run.plan_revision.as_ref() == Some(&self.plan)
                && enabled_for(db, &run, &node)?
                && request_key(&node, &self.plan, &self.unit, &self.inputs)? == self.key,
            "The selected delivery action was superseded"
        );
        control::require_stage(
            db,
            &self.project,
            &self.run,
            &self.node,
            Some(&self.unit),
            &self.inputs,
        )?;
        let unit = load_delivery_unit(db, &self.project, &self.task, &self.unit)?
            .context("The selected delivery unit is unavailable")?;
        ensure!(
            unit.workflow_run_id == self.run && unit.plan_revision == self.plan,
            "The delivery unit belongs to a different active plan"
        );
        Ok(Some((node, unit)))
    }

    pub(crate) async fn execute(self, store: &Store) -> Result<()> {
        let pending = std::sync::Arc::new(self);
        let prepared = store
            .execute({
                let pending = pending.clone();
                move |db| pending.validate(db)
            })
            .await;
        let (node, unit) = match prepared {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return Ok(()),
            Err(error) => {
                return pending
                    .settle(store, "pending", Some(error.to_string()))
                    .await;
            }
        };
        let request = match node.definition.kind {
            BuiltinNode::VerifyCandidate => WorkflowUnitRequest::VerifyCandidate,
            BuiltinNode::AcceptCandidate => WorkflowUnitRequest::AcceptCandidate {
                completion_criteria: unit.unit.completion_criteria,
            },
            BuiltinNode::VerifyCommit => WorkflowUnitRequest::VerifyCommit { external_sha: None },
            _ => anyhow::bail!("Unsupported automatic delivery action"),
        };
        let outcome = store
            .execute_workflow_unit(WorkflowUnitSubmission {
                project_id: pending.project.clone(),
                run_id: pending.run.clone(),
                node_id: pending.node.clone(),
                unit_id: pending.unit.clone(),
                task_id: pending.task.clone(),
                inputs: pending.inputs.clone(),
                request,
            })
            .await;
        match outcome {
            Ok(
                WorkflowUnitOutcome::Artifact { .. } | WorkflowUnitOutcome::CommitWithMerge { .. },
            ) => pending.settle(store, "pending", None).await,
            Ok(WorkflowUnitOutcome::WaitingForCommit) => {
                pending
                    .settle(
                        store,
                        "pending",
                        Some("The Git policy requires the operator's commit".into()),
                    )
                    .await
            }
            Ok(WorkflowUnitOutcome::CommitBlocked { reason }) => {
                pending.settle(store, "failed", Some(reason)).await
            }
            Ok(_) => {
                pending
                    .settle(
                        store,
                        "pending",
                        Some("The delivery action is awaiting operator evidence".into()),
                    )
                    .await
            }
            Err(error) => {
                let retry = node.definition.kind == BuiltinNode::VerifyCommit
                    || error.is::<rusqlite::Error>()
                    || error.is::<std::io::Error>()
                    || error.is::<tokio::task::JoinError>();
                pending
                    .settle(
                        store,
                        if retry { "pending" } else { "failed" },
                        Some(error.to_string()),
                    )
                    .await
            }
        }
    }

    async fn settle(
        self: &std::sync::Arc<Self>,
        store: &Store,
        state: &'static str,
        diagnostic: Option<String>,
    ) -> Result<()> {
        let pending = self.clone();
        let diagnostic = diagnostic.as_deref().map(crate::diagnostics::preview);
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE workflow_automatic_unit_actions SET state = ?4, diagnostic = ?5, retry_after = unixepoch() + 30
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND request_key = ?3 AND state = 'pending'
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)",
                params![pending.run, pending.node, pending.key, state, diagnostic],
            )?;
            control::refresh(&tx, &pending.project, &pending.run)?;
            tx.commit()?;
            Ok(())
        }).await
    }
}
