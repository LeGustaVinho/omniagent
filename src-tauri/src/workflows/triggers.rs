use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    diagnostics,
    store::Store,
    tasks::{AgileEventData, AgileEventOrigin, TaskWorkflowState},
};

use super::{
    WorkflowLaunchOrigin, WorkflowLaunchSource, WorkflowRevision,
    launch::{WorkflowStartSchema, validate_workflow_launch_checkout},
    launch_workflow_in_tx,
    runtime::load_definition,
    validate_workflow_launch_source,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTriggerEvent {
    Created,
    StateChanged,
    ChecklistChecked,
    ChecklistReopened,
}

impl WorkflowTriggerEvent {
    fn key(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::StateChanged => "state_changed",
            Self::ChecklistChecked => "checklist_checked",
            Self::ChecklistReopened => "checklist_reopened",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerWorkKind {
    All,
    Task,
    Subtask,
}

impl TriggerWorkKind {
    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Task => "task",
            Self::Subtask => "subtask",
        }
    }

    fn includes(self, subtask: bool) -> bool {
        match self {
            Self::All => true,
            Self::Task => !subtask,
            Self::Subtask => subtask,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTriggerSpec {
    pub name: String,
    pub enabled: bool,
    pub event: WorkflowTriggerEvent,
    pub work: TriggerWorkKind,
    pub target_state: Option<TaskWorkflowState>,
    pub pipeline: WorkflowRevision,
    pub source: WorkflowTriggerSource,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowTriggerSource {
    pub checkout_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,
}

impl From<WorkflowLaunchSource> for WorkflowTriggerSource {
    fn from(source: WorkflowLaunchSource) -> Self {
        Self {
            checkout_id: source.checkout_id,
            revision_id: Some(source.revision_id),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerRule {
    pub id: String,
    pub revision: i64,
    pub spec: WorkflowTriggerSpec,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTriggerActivation {
    pub rule_id: String,
    pub rule_name: String,
    pub rule_revision: i64,
    pub event_id: String,
    pub work_id: String,
    pub run_id: Option<String>,
    pub outcome: String,
    pub detail: Option<String>,
    pub recorded_at: String,
}

const SELECT_CURRENT_RULE: &str =
    "SELECT rule.id, spec.revision, spec.name, spec.enabled, spec.event_kind, spec.work_kind,
            spec.target_state, spec.workflow_id, spec.workflow_revision, spec.checkout_id, spec.markdown_revision_id
     FROM workflow_trigger_rules rule JOIN workflow_trigger_revisions spec ON spec.project_id = rule.project_id
       AND spec.rule_id = rule.id AND spec.revision = rule.current_revision
     WHERE rule.project_id = ?1";

impl Store {
    pub async fn workflow_trigger_rules(
        &self,
        project: String,
    ) -> Result<Vec<WorkflowTriggerRule>> {
        self.execute(move |db| {
            let mut query = db.prepare(&format!(
                "{SELECT_CURRENT_RULE} ORDER BY spec.name, rule.id"
            ))?;
            query
                .query_map([project], read_rule)?
                .map(|row| decode_rule(row?))
                .collect()
        })
        .await
    }

    pub async fn save_workflow_trigger_rule(
        &self,
        project: String,
        rule_id: Option<String>,
        expected_revision: Option<i64>,
        mut spec: WorkflowTriggerSpec,
    ) -> Result<WorkflowTriggerRule> {
        spec.name = spec.name.trim().to_owned();
        ensure!(!spec.name.is_empty(), "Enter a name for the pipeline rule");
        ensure!(
            spec.event == WorkflowTriggerEvent::StateChanged || spec.target_state.is_none(),
            "A target state applies to status-change rules"
        );
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let definition = load_definition(&tx, &project, &spec.pipeline)?
                .context("Choose an exact pipeline revision from this project")?;
            let start = WorkflowStartSchema::for_definition(&definition);
            ensure!(start.as_ref().is_none_or(WorkflowStartSchema::accepts_automation),
                "This pipeline requires inputs supplied at manual launch");
            let requires_document = start.as_ref().is_none_or(|start| start.requires_document);
            validate_workflow_launch_checkout(&tx, &project, &spec.source.checkout_id)?;
            if let Some(revision) = &spec.source.revision_id {
                validate_workflow_launch_source(&tx, &project, &spec.source.checkout_id, revision)?;
            } else {
                ensure!(!requires_document, "Select a saved Markdown revision for the Start document port");
            }
            let (id, revision) = match (rule_id, expected_revision) {
                (Some(id), Some(expected)) => {
                    let current = tx.query_row(
                        "SELECT current_revision FROM workflow_trigger_rules WHERE project_id = ?1 AND id = ?2",
                        params![project, id], |row| row.get::<_, i64>(0),
                    ).optional()?.context("The pipeline rule is outside this project")?;
                    ensure!(current == expected, "The rule changed. Reload the latest revision before saving.");
                    let next = current.checked_add(1).context("The rule revision is too large")?;
                    tx.execute("UPDATE workflow_trigger_rules SET current_revision = ?3 WHERE project_id = ?1 AND id = ?2",
                        params![project, id, next])?;
                    (id, next)
                }
                (None, None) => {
                    let id = Uuid::new_v4().to_string();
                    tx.execute("INSERT INTO workflow_trigger_rules VALUES (?1, ?2, 1)", params![project, id])?;
                    (id, 1)
                }
                _ => anyhow::bail!("Select the current rule revision before saving an edit"),
            };
            let target_state = spec.target_state.map(enum_value).transpose()?;
            tx.execute(
                "INSERT INTO workflow_trigger_revisions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![project, id, revision, spec.name, spec.enabled, spec.event.key(), spec.work.key(), target_state,
                    spec.pipeline.id, spec.pipeline.revision, spec.source.checkout_id, spec.source.revision_id],
            )?;
            tx.commit()?;
            Ok(WorkflowTriggerRule { id, revision, spec })
        }).await
    }

    pub async fn workflow_trigger_activations(
        &self,
        project: String,
    ) -> Result<Vec<WorkflowTriggerActivation>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT activation.rule_id, spec.name, activation.rule_revision, work.id, work.task_id,
                        activation.pipeline_run_id, activation.outcome, activation.detail, work.recorded_at
                 FROM workflow_trigger_activations activation
                 JOIN workflow_trigger_revisions spec ON spec.project_id = activation.project_id
                    AND spec.rule_id = activation.rule_id AND spec.revision = activation.rule_revision
                 JOIN project_work_events work ON work.id = activation.event_id AND work.project_id = activation.project_id
                 WHERE activation.project_id = ?1 ORDER BY work.sequence DESC, activation.rule_id LIMIT 100",
            )?;
            Ok(query.query_map([project], |row| Ok(WorkflowTriggerActivation {
                rule_id: row.get(0)?, rule_name: row.get(1)?, rule_revision: row.get(2)?, event_id: row.get(3)?,
                work_id: row.get(4)?, run_id: row.get(5)?, outcome: row.get(6)?, detail: row.get(7)?, recorded_at: row.get(8)?,
            }))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }
}

/// Runs in the same transaction as the originating work change, so a restart
/// cannot leave an event without its rule decision or create a duplicate run.
pub(crate) fn activate(
    db: &Connection,
    project: &str,
    task: &str,
    event: &str,
    data: &AgileEventData,
    origin: &AgileEventOrigin,
) -> Result<()> {
    let rules = {
        let mut query = db.prepare(&format!(
            "{SELECT_CURRENT_RULE} AND spec.enabled = 1 AND spec.event_kind = ?2
             AND NOT EXISTS (SELECT 1 FROM workflow_trigger_activations a WHERE a.project_id = rule.project_id
                 AND a.rule_id = rule.id AND a.event_id = ?3)
             ORDER BY rule.id"
        ))?;
        query
            .query_map(params![project, data.key(), event], read_rule)?
            .map(|row| decode_rule(row?))
            .collect::<Result<Vec<_>>>()?
    };
    if rules.is_empty() {
        return Ok(());
    }
    let subtask = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM project_task_memberships WHERE project_id = ?1 AND task_id = ?2 AND parent_task_id IS NOT NULL)",
        params![project, task], |row| row.get::<_, bool>(0),
    )?;
    for rule in rules {
        if !rule.spec.work.includes(subtask) || rule.spec.target_state.is_some_and(|target| {
            !matches!(data, AgileEventData::StateChanged { current, .. } if *current == target)
        }) {
            continue;
        }
        let (outcome, run, detail) = if origin.contains_pipeline(&rule.spec.pipeline.id) {
            (
                "skipped_cycle",
                None,
                Some("The pipeline already ran in this automation chain".to_owned()),
            )
        } else {
            db.execute_batch("SAVEPOINT workflow_trigger_launch")?;
            let result = launch_workflow_in_tx(
                db,
                project,
                &rule.spec.pipeline,
                &rule.spec.source.checkout_id,
                rule.spec.source.revision_id.as_deref(),
                &WorkflowLaunchOrigin::Rule {
                    rule_id: rule.id.clone(),
                    event_id: event.into(),
                    work_id: task.into(),
                },
            );
            match result {
                Ok(run) => {
                    db.execute_batch("RELEASE workflow_trigger_launch")?;
                    ("launched", Some(run.id), None)
                }
                Err(error) => {
                    db.execute_batch(
                        "ROLLBACK TO workflow_trigger_launch; RELEASE workflow_trigger_launch",
                    )?;
                    (
                        "failed",
                        None,
                        Some(diagnostics::preview(&error.to_string())),
                    )
                }
            }
        };
        db.execute(
            "INSERT INTO workflow_trigger_activations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![project, rule.id, event, rule.revision, run, outcome, detail],
        )?;
    }
    Ok(())
}

type RuleRow = (
    String,
    i64,
    String,
    bool,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
);

fn read_rule(row: &Row<'_>) -> rusqlite::Result<RuleRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn decode_rule(
    (
        id,
        revision,
        name,
        enabled,
        event,
        work,
        state,
        workflow,
        workflow_revision,
        checkout_id,
        revision_id,
    ): RuleRow,
) -> Result<WorkflowTriggerRule> {
    let decode = |value: String| serde_json::Value::String(value);
    Ok(WorkflowTriggerRule {
        id,
        revision,
        spec: WorkflowTriggerSpec {
            name,
            enabled,
            event: serde_json::from_value(decode(event))?,
            work: serde_json::from_value(decode(work))?,
            target_state: state
                .map(|value| serde_json::from_value(decode(value)))
                .transpose()?,
            pipeline: WorkflowRevision {
                id: workflow,
                revision: workflow_revision,
            },
            source: WorkflowTriggerSource {
                checkout_id,
                revision_id,
            },
        },
    })
}

fn enum_value(value: impl Serialize) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => anyhow::bail!("Expected a saved Agile state identifier"),
    }
}
