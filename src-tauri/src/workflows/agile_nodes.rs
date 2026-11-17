use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    diagnostics, protection,
    repository::RepositoryMarkdownRevision,
    store::Store,
    tasks::{
        ChecklistProgressReceipt, ProjectWork, TaskDispatchContext, TaskPlacement,
        TaskWorkflowState, current_work_state, insert_project_work, load_checklist_items,
        load_project_task, load_project_work_hierarchy, pipeline_work_origin,
        set_pipeline_checklist_item_checked_in_transaction, transition_work_in_transaction,
        workflow_task_context,
    },
};

use super::{
    ArtifactKind, ArtifactValue, NodeConfiguration, NodeInputs, PinnedNode, TaskReference,
    WorkflowRun,
    artifacts::{insert_scoped_artifact, load_artifact},
    control::{refresh, require_stage},
    digest,
    inspection::record_workflow_input_ids,
    iteration::{MAX_FOR_EACH_ITEMS, WorkflowJsonList},
    launch::{load_workflow_launch, require_launch_checklist_item, require_launch_work},
    registry::supports_pinned_execution,
    runtime::{load_pinned_node, load_workflow_run},
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindWorkKind {
    #[default]
    Work,
    Task,
    Subtask,
    Checklist,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindWorkResultMode {
    #[default]
    List,
    Single,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindWorkConfiguration {
    pub kind: FindWorkKind,
    pub result: FindWorkResultMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskWorkflowState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
}

impl FindWorkConfiguration {
    pub(super) fn valid(&self) -> bool {
        self.query.as_ref().is_none_or(|query| query.len() <= 4096)
            && self
                .parent_task_id
                .as_ref()
                .is_none_or(|parent| !parent.trim().is_empty())
            && (self.checked.is_none() || self.kind == FindWorkKind::Checklist)
            && (self.parent_task_id.is_none() || self.kind != FindWorkKind::Task)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateProgressConfiguration {
    Checklist { checked: bool },
    Transition { next: TaskWorkflowState },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManageWorkConfiguration {
    Link,
    CreateTask,
    CreateSubtask,
}

impl ManageWorkConfiguration {
    pub(super) fn needs_task(self) -> bool {
        matches!(self, Self::Link | Self::CreateSubtask)
    }

    pub(super) fn needs_title(self) -> bool {
        matches!(self, Self::CreateTask | Self::CreateSubtask)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadWorkContextMode {
    Work,
    Markdown,
    Both,
}

impl LoadWorkContextMode {
    pub(super) fn needs_task(self) -> bool {
        matches!(self, Self::Work | Self::Both)
    }

    pub(super) fn needs_document(self) -> bool {
        matches!(self, Self::Markdown | Self::Both)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskDispatchContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<RepositoryMarkdownRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgileTarget {
    pub task: TaskReference,
    pub selection: AgileSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgileSelection {
    State {
        expected: TaskWorkflowState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence: Option<String>,
    },
    Checklist {
        item: ChecklistProgressReceipt,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgileProgressReceipt {
    pub target: AgileTarget,
    pub previous: AgileSelection,
    pub changed: bool,
    pub event_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum AgileStageOutcome {
    Completed,
    Failed { reason: String },
}

const AGILE_STAGES_PER_PASS: usize = 16;

struct PendingStage {
    project: String,
    run: String,
    node: String,
    plan_key: String,
    unit_key: String,
    revision: String,
    inputs: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkMatch {
    title: String,
    placement: TaskPlacement,
    target: AgileTarget,
}

pub(super) fn task_target(db: &Connection, project: &str, task: &str) -> Result<AgileTarget> {
    load_project_task(db, project, task)?.context("The selected work is outside this project")?;
    Ok(AgileTarget {
        task: TaskReference {
            project_id: project.into(),
            task_id: task.into(),
        },
        selection: state_selection(db, project, task)?,
    })
}

pub(super) fn load_task_context(
    db: &Connection,
    project: &str,
    run: &str,
    task: &TaskReference,
) -> Result<TaskDispatchContext> {
    ensure!(
        task.project_id == project,
        "The work context belongs to a different project"
    );
    require_launch_work(db, project, run, Some(&task.task_id))?;
    let item = load_workflow_launch(db, project, run)?.and_then(|receipt| receipt.checklist_item);
    workflow_task_context(db, project, &task.task_id, item.as_ref())
}

fn target_for_launch(db: &Connection, project: &str, run: &str, task: &str) -> Result<AgileTarget> {
    require_launch_work(db, project, run, Some(task))?;
    let mut target = task_target(db, project, task)?;
    if let Some(item) =
        load_workflow_launch(db, project, run)?.and_then(|receipt| receipt.checklist_item)
    {
        target.selection = AgileSelection::Checklist { item };
    }
    Ok(target)
}

fn state_selection(db: &Connection, project: &str, task: &str) -> Result<AgileSelection> {
    let sequence: Option<i64> = db.query_row(
        "SELECT max(sequence) FROM project_work_events
         WHERE project_id = ?1 AND task_id = ?2 AND kind IN ('created', 'state_changed')",
        params![project, task],
        |row| row.get(0),
    )?;
    Ok(AgileSelection::State {
        expected: current_work_state(db, project, task)?,
        sequence: sequence.map(|sequence| sequence.to_string()),
    })
}

fn includes_work(config: &FindWorkConfiguration, work: &ProjectWork) -> bool {
    let parent = match &work.placement {
        TaskPlacement::Task { .. } => None,
        TaskPlacement::Subtask { parent_task_id } => Some(parent_task_id),
    };
    (match config.kind {
        FindWorkKind::Work | FindWorkKind::Checklist => true,
        FindWorkKind::Task => parent.is_none(),
        FindWorkKind::Subtask => parent.is_some(),
    }) && config
        .parent_task_id
        .as_ref()
        .is_none_or(|selected| parent == Some(selected))
}

pub(super) fn find_work(
    db: &Connection,
    project: &str,
    run: &str,
    config: &FindWorkConfiguration,
) -> Result<Vec<(&'static str, ArtifactValue)>> {
    ensure!(config.valid(), "The Find work filters are invalid");
    if let Some(parent) = &config.parent_task_id {
        load_project_task(db, project, parent)?
            .context("The parent work is outside this project")?;
    }
    let launch = load_workflow_launch(db, project, run)?;
    let pinned_item = launch
        .as_ref()
        .and_then(|receipt| receipt.checklist_item.as_ref());
    let pinned_task = launch.as_ref().and_then(|receipt| match &receipt.origin {
        super::WorkflowLaunchOrigin::Agile { work_id, .. }
        | super::WorkflowLaunchOrigin::Rule { work_id, .. } => Some(work_id.as_str()),
        super::WorkflowLaunchOrigin::Editor { .. } => None,
    });
    let query = config
        .query
        .as_deref()
        .map(str::trim)
        .map(str::to_lowercase);
    let matches = |title: &str| {
        query
            .as_ref()
            .is_none_or(|query| title.to_lowercase().contains(query))
    };
    let mut found = Vec::new();
    for work in load_project_work_hierarchy(db, project)?.work {
        if !includes_work(config, &work) {
            continue;
        }
        let task = &work.task.id;
        if let Some(state) = config.state
            && current_work_state(db, project, task)? != state
        {
            continue;
        }
        if config.kind == FindWorkKind::Checklist {
            if work.checklist_id.is_none()
                || pinned_item.is_some() && pinned_task != Some(task.as_str())
            {
                continue;
            }
            for item in load_checklist_items(
                db,
                project,
                task,
                pinned_item.map(|item| item.item_id.as_str()),
                pinned_item.map(|item| item.revision),
            )? {
                if matches(&item.body)
                    && config.checked.is_none_or(|checked| checked == item.checked)
                {
                    let selection = AgileSelection::Checklist {
                        item: ChecklistProgressReceipt::from(&item),
                    };
                    found.push(WorkMatch {
                        title: item.body,
                        placement: work.placement.clone(),
                        target: AgileTarget {
                            task: TaskReference {
                                project_id: project.into(),
                                task_id: task.clone(),
                            },
                            selection,
                        },
                    });
                    check_match_limit(config.result, found.len())?;
                }
            }
        } else if matches(&work.task.title) {
            let target = task_target(db, project, task)?;
            found.push(WorkMatch {
                title: work.task.title,
                placement: work.placement,
                target,
            });
            check_match_limit(config.result, found.len())?;
        }
    }

    let values = found
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<_, _>>()?;
    let mut outputs = vec![(
        "matches",
        ArtifactValue::JsonList(WorkflowJsonList::new(values, None)?),
    )];
    if config.result == FindWorkResultMode::Single {
        let selected = found.pop().context("Find work returned no matching work")?;
        outputs.push(("task", ArtifactValue::Task(selected.target.task.clone())));
        outputs.push(("target", ArtifactValue::AgileTarget(selected.target)));
    }
    Ok(outputs)
}

fn check_match_limit(mode: FindWorkResultMode, count: usize) -> Result<()> {
    ensure!(
        mode != FindWorkResultMode::Single || count <= 1,
        "Find work returned multiple matches. Narrow its filters."
    );
    ensure!(
        count <= usize::from(MAX_FOR_EACH_ITEMS),
        "Find work exceeded the supported list size. Narrow its filters."
    );
    Ok(())
}

pub(super) fn update_progress(
    db: &Connection,
    project: &str,
    run: &str,
    target: &AgileTarget,
    config: &UpdateProgressConfiguration,
) -> Result<AgileProgressReceipt> {
    let task = &target.task.task_id;
    ensure!(
        target.task.project_id == project,
        "The progress target belongs to a different project"
    );
    load_project_task(db, project, task)?.context("The progress target is outside this project")?;
    require_launch_work(db, project, run, Some(task))?;
    let prior: i64 = db.query_row(
        "SELECT coalesce(max(sequence), 0) FROM project_work_events WHERE project_id = ?1",
        [project],
        |row| row.get(0),
    )?;
    let updated_selection = match (&target.selection, config) {
        (
            AgileSelection::Checklist { item },
            UpdateProgressConfiguration::Checklist { checked },
        ) => {
            let revised = set_pipeline_checklist_item_checked_in_transaction(
                db,
                project,
                run,
                task,
                &item.item_id,
                item.revision,
                *checked,
            )?;
            AgileSelection::Checklist { item: revised }
        }
        (
            AgileSelection::State { expected, .. },
            UpdateProgressConfiguration::Transition { next },
        ) => {
            require_launch_checklist_item(db, project, run, task, None)?;
            ensure!(
                state_selection(db, project, task)? == target.selection,
                "The work state changed after the pipeline selected it"
            );
            let origin = pipeline_work_origin(db, project, run)?;
            transition_work_in_transaction(db, project, task, *expected, *next, &origin)?;
            state_selection(db, project, task)?
        }
        _ => bail!("The selected progress action does not match its typed work target"),
    };
    let mut query = db.prepare(
        "SELECT id FROM project_work_events WHERE project_id = ?1 AND sequence > ?2
         AND json_extract(origin, '$.kind') = 'pipeline' AND json_extract(origin, '$.context.runId') = ?3
         ORDER BY sequence",
    )?;
    let event_ids = query
        .query_map(params![project, prior, run], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(AgileProgressReceipt {
        target: AgileTarget {
            task: target.task.clone(),
            selection: updated_selection.clone(),
        },
        previous: target.selection.clone(),
        changed: updated_selection != target.selection,
        event_ids,
    })
}

/// The graph evaluator can prepare an action on an inspection refresh. Only
/// the scheduler's committed-stage pass can apply it to the Agile store.
pub(super) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<AgileStageOutcome>> {
    let matches_configuration = match &node.instance.configuration {
        NodeConfiguration::UpdateProgress { .. } => {
            inputs.len() == 1 && inputs.contains_key("target")
        }
        NodeConfiguration::ManageWork { action } => {
            let expected_count =
                usize::from(action.needs_task()) + usize::from(action.needs_title());
            inputs.len() == expected_count
                && inputs.contains_key("task") == action.needs_task()
                && inputs.contains_key("title") == action.needs_title()
        }
        _ => false,
    };
    ensure!(
        matches_configuration,
        "The Agile action needs its exact typed inputs"
    );
    let revision = stage_revision(node, inputs)?;
    let plan_key = run.plan_revision.as_deref().unwrap_or("");
    let unit_key = unit.unwrap_or("");
    let key = params![run.id, node.instance.id, plan_key, unit_key];
    let previous: Option<(String, Option<Vec<u8>>)> = db
        .query_row(
            "SELECT request_revision, outcome FROM workflow_agile_stage_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND unit_key = ?4",
            key,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((saved_revision, outcome)) = previous {
        ensure!(
            saved_revision == revision,
            "The Agile action's selected inputs changed"
        );
        return outcome
            .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
            .transpose();
    }
    db.execute(
        "INSERT INTO workflow_agile_stage_actions
         (pipeline_run_id, node_instance_id, plan_key, unit_key, request_revision, inputs, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
        params![
            run.id,
            node.instance.id,
            plan_key,
            unit_key,
            revision,
            protection::protect(&serde_json::to_vec(inputs)?)?
        ],
    )?;
    Ok(None)
}

fn stage_revision(node: &PinnedNode, inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(&node.revision, inputs))?))
}

impl Store {
    pub(crate) async fn settle_ready_agile_stages(&self) -> Result<usize> {
        self.execute(|db| {
            let mut processed = 0;
            while processed < AGILE_STAGES_PER_PASS {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let Some(stage) = next_pending_stage(&tx)? else {
                    break;
                };
                settle_stage(&tx, &stage)?;
                tx.commit()?;
                processed += 1;
            }
            Ok(processed)
        })
        .await
    }
}

fn next_pending_stage(db: &Connection) -> Result<Option<PendingStage>> {
    Ok(db.query_row(
        "SELECT pin.project_id, action.pipeline_run_id, action.node_instance_id, action.plan_key,
                action.unit_key, action.request_revision, action.inputs
         FROM workflow_agile_stage_actions action
         JOIN workflow_run_pins pin ON pin.pipeline_run_id = action.pipeline_run_id
         WHERE action.state = 'pending' AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
         ORDER BY action.pipeline_run_id, action.node_instance_id, action.plan_key, action.unit_key
         LIMIT 1",
        [],
        |row| Ok(PendingStage {
            project: row.get(0)?,
            run: row.get(1)?,
            node: row.get(2)?,
            plan_key: row.get(3)?,
            unit_key: row.get(4)?,
            revision: row.get(5)?,
            inputs: row.get(6)?,
        }),
    ).optional()?)
}

fn settle_stage(db: &Connection, stage: &PendingStage) -> Result<()> {
    db.execute_batch("SAVEPOINT workflow_agile_progress")?;
    let (state, outcome) = match apply_stage(db, stage) {
        Ok(()) => {
            db.execute_batch("RELEASE workflow_agile_progress")?;
            ("completed", AgileStageOutcome::Completed)
        }
        Err(error) => {
            db.execute_batch(
                "ROLLBACK TO workflow_agile_progress; RELEASE workflow_agile_progress",
            )?;
            (
                "failed",
                AgileStageOutcome::Failed {
                    reason: diagnostics::preview(&error.to_string()),
                },
            )
        }
    };
    let changed = db.execute(
        "UPDATE workflow_agile_stage_actions SET state = ?5, outcome = ?6
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND plan_key = ?3 AND unit_key = ?4
           AND state = 'pending'",
        params![
            stage.run,
            stage.node,
            stage.plan_key,
            stage.unit_key,
            state,
            protection::protect(&serde_json::to_vec(&outcome)?)?
        ],
    )?;
    ensure!(
        changed == 1,
        "The pending Agile action changed during settlement"
    );
    refresh(db, &stage.project, &stage.run)
}

fn apply_stage(db: &Connection, stage: &PendingStage) -> Result<()> {
    let run = load_workflow_run(db, &stage.project, &stage.run)?
        .context("The Agile action's workflow run is unavailable")?;
    ensure!(
        run.plan_revision.as_deref().unwrap_or("") == stage.plan_key,
        "The selected plan changed before the Agile action"
    );
    let node = load_pinned_node(db, &stage.project, &stage.run, &stage.node)?
        .context("The Agile action's workflow node is unavailable")?;
    ensure!(
        supports_pinned_execution(&node.definition),
        "The pinned Agile action executor is unavailable"
    );
    let inputs: NodeInputs = serde_json::from_slice(&protection::unprotect(&stage.inputs)?)?;
    ensure!(
        stage_revision(&node, &inputs)? == stage.revision,
        "The stored Agile action differs from its pinned inputs"
    );
    let unit = (!stage.unit_key.is_empty()).then_some(stage.unit_key.as_str());
    require_stage(db, &stage.project, &stage.run, &stage.node, unit, &inputs)?;
    let outputs = match &node.instance.configuration {
        NodeConfiguration::UpdateProgress { action } => {
            let source = stage_artifact(db, stage, &inputs, "target", ArtifactKind::AgileTarget)?;
            let ArtifactValue::AgileTarget(target) = source else {
                bail!("The pinned Agile action requires a typed work target");
            };
            let receipt = update_progress(db, &stage.project, &stage.run, &target, action)?;
            [
                ("target", ArtifactValue::AgileTarget(receipt.target.clone())),
                ("progress", ArtifactValue::AgileProgressReceipt(receipt)),
            ]
        }
        NodeConfiguration::ManageWork { action } => {
            let target = manage_work(db, stage, &inputs, *action)?;
            [
                ("task", ArtifactValue::Task(target.task.clone())),
                ("target", ArtifactValue::AgileTarget(target)),
            ]
        }
        _ => bail!("The pending Agile action does not match its pinned node"),
    };
    for (port, value) in outputs {
        let artifact = insert_scoped_artifact(
            db,
            &stage.project,
            stage.run.clone(),
            stage.node.clone(),
            port.into(),
            unit.map(str::to_owned),
            value,
        )?;
        record_workflow_input_ids(db, &artifact, &stage.plan_key, &inputs)?;
    }
    Ok(())
}

fn stage_artifact(
    db: &Connection,
    stage: &PendingStage,
    inputs: &NodeInputs,
    port: &str,
    kind: ArtifactKind,
) -> Result<ArtifactValue> {
    let id = inputs
        .get(port)
        .with_context(|| format!("The Agile action needs its {port} input"))?;
    let source = load_artifact(db, &stage.project, &stage.run, id, kind)?
        .with_context(|| format!("The Agile action's {port} input is unavailable"))?;
    Ok(source.value)
}

fn manage_work(
    db: &Connection,
    stage: &PendingStage,
    inputs: &NodeInputs,
    action: ManageWorkConfiguration,
) -> Result<AgileTarget> {
    let project = &stage.project;
    let run = &stage.run;
    let task = if action.needs_task() {
        let ArtifactValue::Task(task) =
            stage_artifact(db, stage, inputs, "task", ArtifactKind::Task)?
        else {
            bail!("The Agile action requires a typed Task or Sub-Task");
        };
        ensure!(
            task.project_id == *project,
            "The selected work belongs to a different project"
        );
        Some(task.task_id)
    } else {
        None
    };
    if action == ManageWorkConfiguration::Link {
        let task = task.context("The Agile link lost its work selection")?;
        return target_for_launch(db, project, run, &task);
    }

    require_launch_work(db, project, run, task.as_deref())?;
    let placement = match task {
        Some(parent_task_id) => {
            require_launch_checklist_item(db, project, run, &parent_task_id, None)?;
            TaskPlacement::Subtask { parent_task_id }
        }
        None => TaskPlacement::Task { epic_id: None },
    };
    let ArtifactValue::RenderedText(title) =
        stage_artifact(db, stage, inputs, "title", ArtifactKind::RenderedText)?
    else {
        bail!("The Agile creation needs a typed work title");
    };
    let origin = pipeline_work_origin(db, project, run)?;
    let created =
        insert_project_work(db, project.clone(), title.trim().into(), placement, &origin)?;
    task_target(db, project, &created.id)
}
