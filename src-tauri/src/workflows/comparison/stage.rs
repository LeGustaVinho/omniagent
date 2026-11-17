use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    diagnostics,
    dispatch::read_background_dispatch_result,
    jobs::{PipelineDispatch, insert_workflow_comparison_candidate},
    model_catalog::ModelSelection,
    protection,
    providers::{
        SessionMode,
        profiles::{DESKTOP_PERMISSION_REVISION, desktop_profile},
    },
    store::Store,
    workflows::{
        ArtifactKind, ArtifactValue, NodeConfiguration, NodeInputs, PinnedNode, WorkflowArtifact,
        WorkflowJobInput, WorkflowRun,
        artifacts::{insert_scoped_artifact, load_artifact},
        control::{self, ControlStageState},
        digest,
        inspection::record_workflow_completion_inputs,
        runtime::load_pinned_node,
    },
};

use super::{ComparisonSelection, PreparedComparison, load_group, registered_root};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowComparisonConfiguration {
    pub selections: Vec<ModelSelection>,
}

impl WorkflowComparisonConfiguration {
    pub(crate) fn valid(&self) -> bool {
        self.selections.len() >= 2
            && self.selections.iter().collect::<HashSet<_>>().len() == self.selections.len()
            && self.selections.iter().all(|selection| {
                !selection.profile_id.trim().is_empty() && !selection.model_id.trim().is_empty()
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonResults {
    pub group_id: String,
    pub commit: String,
    pub tree: String,
    pub state: WorkflowComparisonOutcome,
    pub candidates: Vec<WorkflowComparisonCandidate>,
}

impl WorkflowComparisonResults {
    pub fn successful_selection(&self) -> ComparisonSelection {
        ComparisonSelection {
            group_id: self.group_id.clone(),
            run_ids: self
                .candidates
                .iter()
                .filter_map(|candidate| {
                    candidate.response_revision_id.as_ref()?;
                    candidate.run_id.clone()
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowComparisonOutcome {
    Succeeded,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonCandidate {
    pub selection: ModelSelection,
    pub job_id: String,
    pub run_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub response_revision_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowComparisonStage {
    pub node_id: String,
    pub node_revision: String,
    pub unit_id: Option<String>,
    pub plan_revision: Option<String>,
    pub inputs: NodeInputs,
    pub group_id: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct StagePin {
    node_revision: String,
    inputs: NodeInputs,
    unit_id: Option<String>,
    plan_revision: Option<String>,
}

pub(crate) struct ComparisonStageOutcome {
    pub state: ControlStageState,
    pub port: &'static str,
    pub reason: Option<String>,
}

impl ComparisonStageOutcome {
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            state: ControlStageState::Failed,
            port: "failed",
            reason: Some(reason.into()),
        }
    }

    fn from_results(results: &WorkflowComparisonResults) -> Self {
        let reason = match results.state {
            WorkflowComparisonOutcome::Failed => {
                return Self::failed("No candidate produced a retained response");
            }
            WorkflowComparisonOutcome::Partial => {
                Some("Some candidates did not produce a retained response".into())
            }
            WorkflowComparisonOutcome::Succeeded => None,
        };
        Self {
            state: ControlStageState::Completed,
            port: "completed",
            reason,
        }
    }
}

struct StageRecord {
    group_id: Option<String>,
    result_artifact_id: Option<String>,
    state: String,
    error: Option<String>,
}

fn load_stage(db: &Connection, run: &str, node: &str, key: &str) -> Result<Option<StageRecord>> {
    Ok(db
        .query_row(
            "SELECT group_id, result_artifact_id, state, error FROM workflow_comparison_stages
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3",
            params![run, node, key],
            |row| {
                Ok(StageRecord {
                    group_id: row.get(0)?,
                    result_artifact_id: row.get(1)?,
                    state: row.get(2)?,
                    error: row.get(3)?,
                })
            },
        )
        .optional()?)
}

pub(crate) fn evaluate(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &BTreeMap<String, WorkflowArtifact>,
) -> Result<Option<ComparisonStageOutcome>> {
    let pin = StagePin {
        node_revision: node.revision.clone(),
        inputs: inputs
            .iter()
            .map(|(port, artifact)| (port.clone(), artifact.id.clone()))
            .collect(),
        unit_id: unit.map(str::to_owned),
        plan_revision: if unit.is_some() {
            run.plan_revision.clone()
        } else {
            None
        },
    };
    let encoded = serde_json::to_vec(&pin)?;
    let key = digest(&encoded);
    let node_id = &node.instance.id;
    let Some(stage) = load_stage(db, &run.id, node_id, &key)? else {
        db.execute(
            "INSERT INTO workflow_comparison_stages(pipeline_run_id, node_instance_id, execution_key, payload, state)
             VALUES (?1, ?2, ?3, ?4, 'queued')",
            params![run.id, node_id, key, protection::protect(&encoded)?],
        )?;
        return Ok(None);
    };
    let outcome = match stage.state.as_str() {
        "queued" => None,
        "failed" => Some(ComparisonStageOutcome::failed(
            stage.error.context("Missing comparison failure")?,
        )),
        "settled" => {
            let artifact = load_artifact(
                db,
                &run.project_id,
                &run.id,
                &stage
                    .result_artifact_id
                    .context("Missing comparison result")?,
                ArtifactKind::ComparisonResults,
            )?
            .context("The settled comparison result is unavailable")?;
            let ArtifactValue::ComparisonResults(results) = &artifact.value else {
                anyhow::bail!("The comparison result has an invalid type")
            };
            record_workflow_completion_inputs(
                db,
                &artifact,
                run.plan_revision.as_deref().unwrap_or(""),
                inputs,
            )?;
            Some(ComparisonStageOutcome::from_results(results))
        }
        "running" => {
            let group = stage.group_id.context("Missing comparison group")?;
            let Some(results) = collect_results(db, &run.project_id, &group)? else {
                return Ok(None);
            };
            let outcome = ComparisonStageOutcome::from_results(&results);
            let artifact = insert_scoped_artifact(
                db,
                &run.project_id,
                run.id.clone(),
                node_id.clone(),
                "results".into(),
                pin.unit_id.clone(),
                ArtifactValue::ComparisonResults(results),
            )?;
            record_workflow_completion_inputs(
                db,
                &artifact,
                run.plan_revision.as_deref().unwrap_or(""),
                inputs,
            )?;
            db.execute(
                "UPDATE workflow_comparison_stages SET state = 'settled', result_artifact_id = ?4
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3",
                params![run.id, node_id, key, artifact.id],
            )?;
            Some(outcome)
        }
        _ => anyhow::bail!("The workflow comparison state is invalid"),
    };
    Ok(outcome)
}

fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "interrupted")
}

fn collect_results(
    db: &Connection,
    project: &str,
    group_id: &str,
) -> Result<Option<WorkflowComparisonResults>> {
    let group = load_group(db, project, group_id)?;
    let has_pending_recovery: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM model_comparison_candidates c
         JOIN job_recoveries r ON r.job_id = c.job_id JOIN jobs j ON j.id = c.job_id
         WHERE c.group_id = ?1 AND j.queue_state <> 'cancelled')",
        [group_id],
        |row| row.get(0),
    )?;
    if has_pending_recovery
        || group
            .candidates
            .iter()
            .any(|candidate| !terminal(&candidate.state))
    {
        return Ok(None);
    }
    let checkout: String = db.query_row(
        "SELECT source_checkout_id FROM model_comparison_groups WHERE id = ?1 AND project_id = ?2",
        params![group_id, project],
        |row| row.get(0),
    )?;
    let mut candidates = Vec::with_capacity(group.candidates.len());
    for candidate in group.candidates {
        let response_revision_id = match (
            candidate.state.as_str(),
            &candidate.run_id,
            &candidate.dispatch_id,
        ) {
            ("succeeded", Some(run), Some(dispatch)) => preserved_response(
                db,
                &checkout,
                &group.prompt,
                &candidate.job_id,
                run,
                dispatch,
            )?,
            _ => None,
        };
        candidates.push(WorkflowComparisonCandidate {
            selection: candidate.selection,
            job_id: candidate.job_id,
            run_id: candidate.run_id,
            dispatch_id: candidate.dispatch_id,
            response_revision_id,
            state: candidate.state,
        });
    }
    let usable = candidates
        .iter()
        .filter(|candidate| candidate.response_revision_id.is_some())
        .count();
    let state = if usable == 0 {
        WorkflowComparisonOutcome::Failed
    } else if usable == candidates.len() {
        WorkflowComparisonOutcome::Succeeded
    } else {
        WorkflowComparisonOutcome::Partial
    };
    Ok(Some(WorkflowComparisonResults {
        group_id: group.id,
        commit: group.commit,
        tree: group.tree,
        state,
        candidates,
    }))
}

fn preserved_response(
    db: &Connection,
    checkout: &str,
    prompt: &str,
    job: &str,
    run: &str,
    dispatch: &str,
) -> Result<Option<String>> {
    let deleted: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM history_deleted_dispatch_payloads WHERE dispatch_id = ?1)",
        [dispatch],
        |row| row.get(0),
    )?;
    if deleted {
        return Ok(None);
    }
    Ok(
        read_background_dispatch_result(db, run, job, checkout, prompt)
            .ok()
            .map(|(result, _)| result.output_revision_id),
    )
}

pub(crate) fn load_stages(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Vec<WorkflowComparisonStage>> {
    db.prepare(
        "SELECT node_instance_id, payload, group_id, state, error FROM workflow_comparison_stages
         WHERE pipeline_run_id = ?1 ORDER BY node_instance_id, execution_key",
    )?
    .query_map([&run.id], |row| {
        Ok((
            row.get(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?
    .map(|row| {
        let (node_id, payload, group_id, state, error) = row?;
        let pin: StagePin = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        Ok(WorkflowComparisonStage {
            node_id,
            node_revision: pin.node_revision,
            unit_id: pin.unit_id,
            plan_revision: pin.plan_revision,
            inputs: pin.inputs,
            group_id,
            state,
            error,
        })
    })
    .collect()
}

#[derive(Clone)]
pub(crate) struct PendingWorkflowComparison {
    project: String,
    run: String,
    node: String,
    key: String,
    pin: StagePin,
}

pub(crate) struct PreparedWorkflowComparison(PreparedComparison);

struct RequestedComparison {
    configuration: WorkflowComparisonConfiguration,
    checkout: String,
    commit: String,
    prompt: String,
}

impl Store {
    pub(crate) async fn next_workflow_comparison(
        &self,
    ) -> Result<Option<PendingWorkflowComparison>> {
        self.execute(|db| {
            let record = db.query_row(
                "SELECT p.project_id, s.pipeline_run_id, s.node_instance_id, s.execution_key, s.payload
                 FROM workflow_comparison_stages s JOIN workflow_run_pins p ON p.pipeline_run_id = s.pipeline_run_id
                 WHERE s.state = 'queued' AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY s.pipeline_run_id, s.node_instance_id, s.execution_key LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, Vec<u8>>(4)?)),
            ).optional()?;
            record.map(|(project, run, node, key, payload)| Ok(PendingWorkflowComparison {
                project, run, node, key,
                pin: serde_json::from_slice(&protection::unprotect(&payload)?)?,
            })).transpose()
        }).await
    }

    pub(crate) async fn refresh_workflow_comparisons(&self) -> Result<()> {
        self.execute(|db| {
            let pending = db.prepare(
                "SELECT DISTINCT p.project_id, s.pipeline_run_id
                 FROM workflow_comparison_stages s JOIN workflow_run_pins p ON p.pipeline_run_id = s.pipeline_run_id
                 JOIN job_pipeline_runs workflow ON workflow.id = p.pipeline_run_id
                 WHERE s.state = 'running' AND EXISTS (
                   SELECT 1 FROM model_comparison_candidates c JOIN job_pipeline_origins origin ON origin.job_id = c.job_id
                   WHERE c.group_id = s.group_id AND (origin.plan_revision IS NULL OR origin.plan_revision = workflow.plan_revision)
                 ) AND NOT EXISTS (
                   SELECT 1 FROM model_comparison_candidates c
                   JOIN jobs j ON j.id = c.job_id
                   LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (
                       SELECT MAX(latest.ordinal) FROM job_attempts latest WHERE latest.job_id = j.id)
                   LEFT JOIN runs r ON r.id = a.run_id
                   WHERE c.group_id = s.group_id AND (
                       COALESCE(r.status, j.queue_state) NOT IN ('succeeded', 'failed', 'cancelled', 'interrupted')
                       OR (j.queue_state <> 'cancelled' AND EXISTS(SELECT 1 FROM job_recoveries recovery WHERE recovery.job_id = j.id))))",
            )?.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !pending.is_empty() {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                for (project, run) in pending {
                    control::refresh(&tx, &project, &run)?;
                }
                tx.commit()?;
            }
            Ok(())
        }).await
    }
}

impl PendingWorkflowComparison {
    pub(crate) async fn prepare(
        &self,
        store: &Store,
        storage: PathBuf,
        runner: String,
    ) -> Result<PreparedWorkflowComparison> {
        let RequestedComparison {
            configuration,
            checkout,
            commit,
            prompt,
        } = self.request(store).await?;
        let root = registered_root(store, self.project.clone(), checkout.clone()).await?;
        let mut profiles = Vec::with_capacity(configuration.selections.len());
        for selection in configuration.selections {
            let configured = store.configured_executable(selection.provider).await?;
            let model_id = selection.model_id.clone();
            let profile =
                tokio::task::spawn_blocking(move || desktop_profile(configured, Some(model_id)))
                    .await??;
            let option = profile
                .models
                .into_iter()
                .find(|model| model.option.selection == selection && model.option.selectable)
                .map(|model| model.option)
                .with_context(|| {
                    format!("Comparison model {} is unavailable", selection.model_id)
                })?;
            profiles.push((
                SessionScope {
                    provider: selection.provider,
                    profile_id: selection.profile_id,
                    account_binding: profile.account_binding,
                    checkout: root.clone(),
                    executable: profile.executable,
                    cli_version: profile.version,
                    runner: runner.clone(),
                    permission_revision: DESKTOP_PERMISSION_REVISION.into(),
                    mode: SessionMode::ReadOnly,
                    model: Some(selection.model_id),
                    effort: None,
                },
                Some(option),
            ));
        }
        Ok(PreparedWorkflowComparison(
            store
                .prepare_model_comparison(
                    self.project.clone(),
                    checkout,
                    commit,
                    prompt,
                    profiles,
                    storage,
                )
                .await?,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn prepare_with_fixture_profiles(
        &self,
        store: &Store,
        storage: PathBuf,
        profiles: Vec<SessionScope>,
    ) -> Result<PreparedWorkflowComparison> {
        let request = self.request(store).await?;
        let selections = profiles
            .iter()
            .map(super::selected_model)
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            selections == request.configuration.selections,
            "Fixture profiles differ from the pinned comparison"
        );
        Ok(PreparedWorkflowComparison(
            store
                .prepare_model_comparison(
                    self.project.clone(),
                    request.checkout,
                    request.commit,
                    request.prompt,
                    profiles.into_iter().map(|scope| (scope, None)).collect(),
                    storage,
                )
                .await?,
        ))
    }

    async fn request(&self, store: &Store) -> Result<RequestedComparison> {
        let (project, run, node, pin) = (
            self.project.clone(),
            self.run.clone(),
            self.node.clone(),
            self.pin.clone(),
        );
        let (configuration, prompt, document) = store
            .execute(move |db| {
                control::require_stage(
                    db,
                    &project,
                    &run,
                    &node,
                    pin.unit_id.as_deref(),
                    &pin.inputs,
                )?;
                let pinned = load_pinned_node(db, &project, &run, &node)?
                    .filter(|pinned| pinned.revision == pin.node_revision)
                    .context("The pinned comparison stage is unavailable")?;
                let NodeConfiguration::CompareModels(configuration) = pinned.instance.configuration
                else {
                    anyhow::bail!("The comparison configuration is unavailable");
                };
                ensure!(
                    configuration.valid(),
                    "Choose distinct comparison model profiles"
                );
                let input = |port: &str, kind| -> Result<ArtifactValue> {
                    let id = pin
                        .inputs
                        .get(port)
                        .context("The comparison input is missing")?;
                    Ok(load_artifact(db, &project, &run, id, kind)?
                        .context("The pinned comparison input is unavailable")?
                        .value)
                };
                let ArtifactValue::RenderedText(prompt) =
                    input("request", ArtifactKind::RenderedText)?
                else {
                    anyhow::bail!("The comparison request needs text")
                };
                let ArtifactValue::Markdown(document) = input("context", ArtifactKind::Markdown)?
                else {
                    anyhow::bail!("The comparison needs a pinned Markdown revision")
                };
                Ok((configuration, prompt, document))
            })
            .await?;
        ensure!(
            !prompt.trim().is_empty(),
            "Enter the shared comparison request"
        );
        let commit = document
            .base_commit
            .clone()
            .context("The comparison context needs a saved Git commit")?;
        let checkout = document.checkout_id.clone();
        let saved = store
            .repository_markdown(checkout.clone(), document.id.clone())
            .await?
            .filter(|saved| saved.revision == document)
            .context("The comparison context no longer matches the saved Markdown revision")?;
        let mut prompt = prompt.trim().to_owned();
        saved.append_to_prompt(&mut prompt)?;
        Ok(RequestedComparison {
            configuration,
            checkout,
            commit,
            prompt,
        })
    }

    pub(crate) async fn commit(
        self,
        store: &Store,
        prepared: PreparedWorkflowComparison,
    ) -> Result<()> {
        let changed = store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let paused: bool = tx.query_row(
                    "SELECT paused FROM job_queue_settings WHERE id = 1",
                    [],
                    |row| row.get(0),
                )?;
                if paused || !self.queued(&tx)? {
                    return Ok(false);
                }
                if let Err(error) = control::require_stage(
                    &tx,
                    &self.project,
                    &self.run,
                    &self.node,
                    self.pin.unit_id.as_deref(),
                    &self.pin.inputs,
                ) {
                    self.reject(&tx, &diagnostics::preview(&error.to_string()))?;
                    tx.commit()?;
                    return Ok(true);
                }
                let task_id = self.unit_work(&tx)?;
                let mut prepared = prepared.0;
                let binding = PipelineDispatch {
                    pipeline_run_id: self.run.clone(),
                    node_instance_id: self.node.clone(),
                    plan_revision: self.pin.plan_revision.clone(),
                    release_id: None,
                };
                let input = WorkflowJobInput {
                    unit_id: self.pin.unit_id.clone(),
                    artifacts: self.pin.inputs.clone(),
                    plan_revision_request: None,
                    comparison: None,
                };
                let group = prepared.insert_group(&tx, |scope, mut request| {
                    request.task_id = task_id.clone();
                    insert_workflow_comparison_candidate(
                        &tx,
                        self.project.clone(),
                        scope,
                        request,
                        &binding,
                        &input,
                    )
                })?;
                tx.execute(
                    "UPDATE workflow_comparison_stages SET state = 'running', group_id = ?4
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3",
                    params![self.run, self.node, self.key, group.id],
                )?;
                tx.commit()?;
                prepared.lease.preserve();
                Ok(true)
            })
            .await?;
        if changed {
            store.job_signal.send_replace(());
        }
        Ok(())
    }

    pub(crate) async fn fail(self, store: &Store, error: &anyhow::Error) -> Result<()> {
        let reason = diagnostics::preview(&error.to_string());
        store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                self.reject(&tx, &reason)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        store.job_signal.send_replace(());
        Ok(())
    }

    fn queued(&self, db: &Connection) -> Result<bool> {
        Ok(load_stage(db, &self.run, &self.node, &self.key)?
            .is_some_and(|stage| stage.state == "queued"))
    }

    fn unit_work(&self, db: &Connection) -> Result<Option<String>> {
        let Some(unit) = &self.pin.unit_id else {
            return Ok(None);
        };
        let task = db
            .query_row(
                "SELECT u.task_id FROM workflow_delivery_units u
                 JOIN job_pipeline_runs r ON r.id = u.pipeline_run_id AND r.plan_revision = u.plan_revision
                 WHERE u.project_id = ?1 AND u.pipeline_run_id = ?2 AND u.unit_id = ?3
                   AND u.plan_revision = ?4",
                params![self.project, self.run, unit, self.pin.plan_revision],
                |row| row.get(0),
            )
            .optional()?
            .context("The comparison logical unit is no longer active")?;
        Ok(Some(task))
    }

    fn reject(&self, db: &Connection, reason: &str) -> Result<()> {
        db.execute(
            "UPDATE workflow_comparison_stages SET state = 'failed', error = ?4
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND execution_key = ?3 AND state = 'queued'",
            params![self.run, self.node, self.key, reason],
        )?;
        control::refresh(db, &self.project, &self.run)
    }
}
