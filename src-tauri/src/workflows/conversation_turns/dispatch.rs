use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    conversations::{SessionScope, load_messages},
    jobs::{JobRequest, LaunchGate, PipelineDispatch, QueueState},
    model_catalog::ModelSelection,
    providers::{
        SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    runs::RunStatus,
    store::Store,
};

use super::{Turn, load_turn, stage_label};
use crate::workflows::{
    ArtifactValue, BuiltinNode, Executor, NodeConfiguration, NodeInputs, PinnedNode,
    WorkflowAgentSubmission, WorkflowHostRequest, WorkflowHostSubmission, WorkflowRun,
    authoring::package::roles,
    control::{
        ControlStageState, StageAvailability, conversation_stage_availability, recorded_control,
        unit_nodes,
    },
    inspection::{WorkflowExecutionJob, load_jobs, load_outputs, load_units},
    runtime::{load_definition, load_workflow_run},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TurnWait {
    node_id: String,
    label: String,
    kind: &'static str,
    run_id: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TurnArtifact {
    id: String,
    node_id: String,
    port: String,
    kind: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActionOutcome {
    node_id: String,
    label: Option<String>,
    state: &'static str,
}

impl ActionOutcome {
    pub(super) fn not_executed(node: &str) -> Self {
        Self {
            node_id: node.to_owned(),
            label: None,
            state: "not_executed",
        }
    }
}

pub(super) enum TurnState {
    Running,
    Failed(String),
    Completed(Option<String>),
}

pub(super) struct RunningTurn {
    pub state: TurnState,
    pub waiting: Option<TurnWait>,
    pub artifacts: Vec<TurnArtifact>,
    pub action_outcome: Option<ActionOutcome>,
    ready: Vec<ReadyStage>,
}

struct ReadyStage {
    node: PinnedNode,
    unit_id: Option<String>,
    inputs: NodeInputs,
    model: Option<ModelSelection>,
    effort: Option<String>,
}

impl RunningTurn {
    pub(super) fn read(db: &Connection, turn: &Turn, run: &WorkflowRun) -> Result<Self> {
        let definition = load_definition(db, &run.project_id, &run.version)?
            .context("The pinned conversation pipeline definition is unavailable")?;
        let units = load_units(db, run)?;
        let unit_nodes = unit_nodes(&definition);
        let jobs = load_jobs(db, run)?;
        let outputs = load_outputs(db, run, &jobs, &units)?;
        let control = recorded_control(db, run)?;
        let current = outputs
            .iter()
            .filter(|output| output.current)
            .map(|output| &output.artifact)
            .collect::<Vec<_>>();
        let artifacts = current
            .iter()
            .filter(|artifact| {
                !matches!(
                    definition
                        .nodes
                        .iter()
                        .find(|node| node.instance.id == artifact.node_instance_id)
                        .map(|node| node.instance.kind),
                    Some(BuiltinNode::Start | BuiltinNode::TypedStart)
                )
            })
            .map(|artifact| TurnArtifact {
                id: artifact.id.clone(),
                node_id: artifact.node_instance_id.clone(),
                port: artifact.port.clone(),
                kind: artifact.value.kind().key().to_owned(),
            })
            .collect();
        let graph_terminal = |node_id: &str| {
            !definition
                .wires
                .iter()
                .any(|wire| wire.from.node == node_id)
        };
        let mut unfinished = Vec::new();
        let mut pending_dependencies = Vec::new();
        let mut unsuccessful = HashSet::new();
        let mut skipped = HashSet::new();
        let mut failed = None;
        let mut waiting = None;
        let mut ready = Vec::new();
        for node in &definition.nodes {
            let node_id = &node.instance.id;
            if matches!(
                node.instance.kind,
                BuiltinNode::Start | BuiltinNode::TypedStart
            ) {
                continue;
            }
            let scoped = unit_nodes.contains(node_id.as_str());
            let matching_units = if scoped {
                units
                    .iter()
                    .filter(|unit| Some(&unit.plan_revision) == run.plan_revision.as_ref())
                    .map(|unit| Some(unit.unit.id.as_str()))
                    .collect::<Vec<_>>()
            } else {
                vec![None]
            };
            if matching_units.is_empty() {
                if matches!(
                    conversation_stage_availability(db, &definition, run, node, None)?,
                    StageAvailability::Skipped
                ) {
                    unsuccessful.insert(node_id.as_str());
                    skipped.insert(node_id.as_str());
                } else {
                    pending_dependencies.push(node_id.as_str());
                }
                continue;
            }
            for unit in matching_units {
                let receipt = control.iter().find(|receipt| {
                    &receipt.node_id == node_id
                        && receipt.unit_id.as_deref() == unit
                        && receipt.plan_revision == run.plan_revision
                });
                if let Some(receipt) = receipt {
                    match receipt.state {
                        ControlStageState::Completed => {}
                        ControlStageState::Skipped => {
                            unsuccessful.insert(node_id.as_str());
                            skipped.insert(node_id.as_str());
                        }
                        ControlStageState::Failed => {
                            unsuccessful.insert(node_id.as_str());
                            failed.get_or_insert_with(|| {
                                receipt
                                    .reason
                                    .clone()
                                    .unwrap_or_else(|| format!("{} failed", node.definition.title))
                            });
                        }
                    }
                    continue;
                }
                if current.iter().any(|artifact| {
                    &artifact.node_instance_id == node_id && artifact.unit_id.as_deref() == unit
                }) {
                    continue;
                }
                if let Some(job) = current_job(&jobs, run, node_id, unit) {
                    let attempt = job.job.latest_attempt.as_ref();
                    match attempt.map(|attempt| attempt.status) {
                        Some(RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted)
                            if job.job.queued_recovery.is_none() =>
                        {
                            unsuccessful.insert(node_id.as_str());
                            failed.get_or_insert_with(|| {
                                format!("{} did not complete", node.definition.title)
                            });
                            continue;
                        }
                        Some(RunStatus::WaitingForInput) => {
                            waiting.get_or_insert_with(|| {
                                wait(
                                    node,
                                    "question",
                                    attempt.map(|attempt| attempt.run_id.clone()),
                                    None,
                                )
                            });
                        }
                        _ if job.job.queue_state == QueueState::Cancelled && attempt.is_none() => {
                            unsuccessful.insert(node_id.as_str());
                            failed.get_or_insert_with(|| {
                                format!("{} was cancelled", node.definition.title)
                            });
                            continue;
                        }
                        _ if job.job.queued_recovery.is_none()
                            && attempt
                                .is_some_and(|attempt| attempt.status == RunStatus::Succeeded)
                            && node
                                .definition
                                .outputs
                                .iter()
                                .all(|port| port.value == crate::workflows::PortType::Control) =>
                        {
                            continue;
                        }
                        _ => {}
                    }
                    unfinished.push(node_id.as_str());
                    continue;
                }
                match conversation_stage_availability(db, &definition, run, node, unit)? {
                    StageAvailability::Skipped => {
                        unsuccessful.insert(node_id.as_str());
                        skipped.insert(node_id.as_str());
                        continue;
                    }
                    StageAvailability::Waiting => {
                        pending_dependencies.push(node_id.as_str());
                        continue;
                    }
                    StageAvailability::Ready(inputs) => match node.definition.executor {
                        Executor::ReadContext if unit.is_none() => {
                            ready.push(ReadyStage {
                                node: node.clone(),
                                unit_id: None,
                                inputs,
                                model: None,
                                effort: None,
                            });
                        }
                        Executor::ReadOnlyAgent | Executor::PlanAgent => {
                            let (model, effort) = effective_model(db, turn, node)?;
                            if let Some(model) = model {
                                if let Some(reason) = &turn.error {
                                    waiting.get_or_insert_with(|| {
                                        wait(node, "model", None, Some(reason.clone()))
                                    });
                                }
                                ready.push(ReadyStage {
                                    node: node.clone(),
                                    unit_id: unit.map(str::to_owned),
                                    inputs,
                                    model: Some(model),
                                    effort,
                                });
                            } else {
                                waiting.get_or_insert_with(|| {
                                    wait(
                                        node,
                                        "model",
                                        None,
                                        Some("Choose a conversation model for this stage".into()),
                                    )
                                });
                            }
                        }
                        executor => {
                            waiting.get_or_insert_with(|| {
                                wait(node, operator_kind(executor), None, None)
                            });
                        }
                    },
                }
                unfinished.push(node_id.as_str());
            }
        }

        // Legacy stages and plans that never created units may lack explicit skipped receipts.
        // Follow only unresolved edges, leaving selected failure/rejection branches runnable.
        loop {
            let before = unsuccessful.len();
            for node in &pending_dependencies {
                if definition.wires.iter().any(|wire| {
                    wire.to.node == *node && unsuccessful.contains(wire.from.node.as_str())
                }) {
                    unsuccessful.insert(node);
                    skipped.insert(node);
                }
            }
            if before == unsuccessful.len() {
                break;
            }
        }
        unfinished.extend(
            pending_dependencies
                .into_iter()
                .filter(|node| !unsuccessful.contains(node)),
        );
        let state = if unfinished.is_empty() {
            let summary = current
                .iter()
                .filter_map(|artifact| match &artifact.value {
                    ArtifactValue::ResultSummary { summary, .. } => Some(summary.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !summary.is_empty() {
                TurnState::Completed(Some(summary.join("\n\n")))
            } else if let Some(reason) = failed {
                TurnState::Failed(reason)
            } else {
                let terminal_agents = current
                    .iter()
                    .filter_map(|artifact| match &artifact.value {
                        ArtifactValue::AgentResponse(response)
                            if graph_terminal(&artifact.node_instance_id) =>
                        {
                            Some(response)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let summary = match terminal_agents.as_slice() {
                    [response] => terminal_agent_response(
                        db,
                        &response.run_id,
                        &response.response_revision_id,
                    )?,
                    _ => None,
                };
                TurnState::Completed(summary)
            }
        } else {
            TurnState::Running
        };
        let action_outcome = turn.request.request.action.as_ref().and_then(|action| {
            if matches!(state, TurnState::Running)
                || jobs
                    .iter()
                    .any(|job| job.node_id == action.node_id && !job.attempts.is_empty())
            {
                return None;
            }
            let mut outcome = ActionOutcome::not_executed(&action.node_id);
            outcome.label = definition
                .nodes
                .iter()
                .find(|node| node.instance.id == action.node_id)
                .map(stage_label);
            if skipped.contains(action.node_id.as_str()) {
                outcome.state = "skipped";
            }
            Some(outcome)
        });
        Ok(Self {
            state,
            waiting,
            artifacts,
            action_outcome,
            ready,
        })
    }
}

fn wait(
    node: &PinnedNode,
    kind: &'static str,
    run_id: Option<String>,
    reason: Option<String>,
) -> TurnWait {
    TurnWait {
        node_id: node.instance.id.clone(),
        label: stage_label(node),
        kind,
        run_id,
        reason,
    }
}

fn operator_kind(executor: Executor) -> &'static str {
    match executor {
        Executor::AwaitWorkflowInput => "question",
        Executor::AwaitWorkflowApproval | Executor::RecordImplementationRelease => "approval",
        Executor::FindOrCreateWork | Executor::ProvisionWorktree => "selection",
        Executor::ReadSelectedGit | Executor::VerifyGitMerge | Executor::VerifyGitCommit => "git",
        _ => "operation",
    }
}

fn current_job<'a>(
    jobs: &'a [WorkflowExecutionJob],
    run: &WorkflowRun,
    node: &str,
    unit: Option<&str>,
) -> Option<&'a WorkflowExecutionJob> {
    jobs.iter()
        .filter(|job| {
            job.node_id == node
                && job.unit_id.as_deref() == unit
                && job.job.pipeline.as_ref().is_some_and(|pipeline| {
                    pipeline.plan_revision.is_none() || pipeline.plan_revision == run.plan_revision
                })
        })
        .last()
}

fn terminal_agent_response(db: &Connection, run: &str, revision: &str) -> Result<Option<String>> {
    let origin: Option<String> = db.query_row(
        "SELECT a.conversation_id FROM agent_attempts a
         WHERE a.run_id = ?1 AND NOT EXISTS(SELECT 1 FROM history_deleted_revisions WHERE revision_id = ?2)",
        params![run, revision], |row| row.get(0)
    ).optional()?;
    let Some(conversation) = origin else {
        return Ok(None);
    };
    Ok(load_messages(db, &conversation, &[revision.into()])?
        .into_iter()
        .next()
        .map(|message| message.text))
}

pub(super) fn pinned_model(
    db: &Connection,
    project: &str,
    node: &PinnedNode,
) -> Result<(Option<ModelSelection>, Option<String>)> {
    let NodeConfiguration::AgentRole {
        model,
        requirements,
        ..
    } = &node.instance.configuration
    else {
        return Ok((None, None));
    };
    if let Some(role) = &requirements.bound_role {
        let role =
            roles::load_role(db, project, role)?.context("The pinned stage role is unavailable")?;
        return Ok((model.clone().or(Some(role.model)), role.effort));
    }
    Ok((model.clone(), None))
}

fn effective_model(
    db: &Connection,
    turn: &Turn,
    node: &PinnedNode,
) -> Result<(Option<ModelSelection>, Option<String>)> {
    let (model, effort) = pinned_model(db, &turn.request.project_id, node)?;
    if model.is_some() {
        return Ok((model, effort));
    }
    Ok((
        turn.request.request.selection.clone(),
        turn.request.request.effort.clone(),
    ))
}

pub(super) async fn qualified_stage_scope(
    store: &Store,
    checkout_id: String,
    model: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
    runner: String,
) -> Result<SessionScope> {
    let configured = store.configured_executable(model.provider).await?;
    let model_id = model.model_id.clone();
    let profile =
        tokio::task::spawn_blocking(move || desktop_profile(configured, Some(model_id))).await??;
    let available = profile
        .models
        .iter()
        .find(|entry| entry.option.selection == model && entry.option.selectable)
        .context("The pinned pipeline model or CLI profile is unavailable")?;
    ensure!(
        effort
            .as_ref()
            .is_none_or(|value| available.efforts.contains(value)),
        "The stage thinking level is no longer available"
    );
    Ok(SessionScope {
        model: Some(model.model_id),
        effort,
        provider: model.provider,
        profile_id: model.profile_id,
        account_binding: profile.account_binding,
        checkout: crate::repository::checkout_root(store, checkout_id).await?,
        executable: profile.executable,
        cli_version: profile.version,
        runner,
        permission_revision: DESKTOP_PERMISSION_REVISION.into(),
        mode,
    })
}

struct StageClaim {
    sequence: i64,
    run: String,
    project: String,
    stage: ReadyStage,
}

pub(crate) async fn prepare_next_conversation_stage(
    store: Store,
    runner: String,
    gate: LaunchGate,
) -> Result<bool> {
    let ready = store
        .execute(|db| {
            let ids = db
                .prepare(
                    "SELECT sequence FROM conversation_pipeline_turns
            WHERE state = 'running' AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
            ORDER BY sequence",
                )?
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut ready = Vec::new();
            for sequence in ids {
                let turn = load_turn(db, sequence)?;
                let Some(run) = turn.run_id.as_deref() else {
                    continue;
                };
                let Some(run) = load_workflow_run(db, &turn.request.project_id, run)? else {
                    continue;
                };
                for stage in RunningTurn::read(db, &turn, &run)?.ready {
                    ready.push(StageClaim {
                        sequence,
                        run: run.id.clone(),
                        project: run.project_id.clone(),
                        stage,
                    });
                }
            }
            Ok(ready)
        })
        .await?;
    for claim in ready {
        let result = claim.submit(&store, runner.clone(), &gate).await;
        let error = result
            .as_ref()
            .err()
            .map(|error| crate::diagnostics::preview(&error.to_string()));
        store.execute(move |db| {
            db.execute("UPDATE conversation_pipeline_turns SET error = ?2 WHERE sequence = ?1 AND state = 'running'",
                params![claim.sequence, error])?;
            Ok(())
        }).await?;
        if let Ok(submitted) = result {
            return Ok(submitted);
        }
    }
    Ok(false)
}

impl StageClaim {
    async fn submit(&self, store: &Store, runner: String, gate: &LaunchGate) -> Result<bool> {
        let project = &self.project;
        let node = &self.stage.node.instance.id;
        if self.stage.node.definition.executor == Executor::ReadContext {
            let Some(_boundary) = gate.enter_admission().await else {
                return Ok(false);
            };
            store
                .execute_workflow_host(WorkflowHostSubmission {
                    project_id: project.clone(),
                    run_id: self.run.clone(),
                    node_id: node.clone(),
                    inputs: self.stage.inputs.clone(),
                    request: WorkflowHostRequest::LoadContext,
                })
                .await?;
            return Ok(true);
        }
        let target = store
            .workflow_operator_agent_target(
                project,
                &self.run,
                node,
                self.stage.unit_id.as_deref(),
                &self.stage.inputs,
            )
            .await?;
        ensure!(
            matches!(target.mode, SessionMode::ReadOnly | SessionMode::Plan),
            "Writing stages require a separate approval and execution"
        );
        let model = self
            .stage
            .model
            .clone()
            .context("Choose a model for the pipeline stage")?;
        let scope = qualified_stage_scope(
            store,
            target.checkout_id.clone(),
            model,
            self.stage.effort.clone(),
            target.mode,
            runner,
        )
        .await?;
        let Some(_boundary) = gate.enter_admission().await else {
            return Ok(false);
        };
        store
            .submit_workflow_agent(WorkflowAgentSubmission {
                project_id: project.clone(),
                dispatch: PipelineDispatch {
                    pipeline_run_id: self.run.clone(),
                    node_instance_id: node.clone(),
                    plan_revision: target.plan_revision,
                    release_id: target.release_id,
                },
                request: JobRequest {
                    checkout_id: Some(target.checkout_id),
                    task_id: target.task_id,
                    mode: target.mode,
                    instructions: String::new(),
                    dependencies: Vec::new(),
                    requires_bridge: false,
                },
                scope,
                input: Default::default(),
                artifacts: self.stage.inputs.clone(),
                unit_id: self.stage.unit_id.clone(),
            })
            .await?;
        Ok(true)
    }
}
