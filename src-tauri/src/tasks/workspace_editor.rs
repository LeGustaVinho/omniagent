use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    providers::Provider,
    repository::{BranchStrategy, branch_strategy_for_scope, write_task_branch_strategy},
    store::Store,
};

use super::{
    AgileEventOrigin, ChecklistExecutionMode, DescriptionOwner, PlannedAssignment, TaskPlacement,
    TaskWorkflowState, WorkStateChanges,
    activity::{load_task_details, write_task_details},
    agile_transition::apply_work_transition,
    checklists::{checklist_mode_override, write_checklist_mode_override},
    current_work_state,
    descriptions::{DescriptionEditorState, consume_draft, load_editor_state, write_description},
    insert_project_work_record, publish_work_creation,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkspacePlacement {
    Epic,
    Task { epic_id: Option<String> },
    Subtask { parent_task_id: String },
}

impl WorkspacePlacement {
    fn task_placement(&self) -> Option<TaskPlacement> {
        match self {
            Self::Epic => None,
            Self::Task { epic_id } => Some(TaskPlacement::Task {
                epic_id: epic_id.clone(),
            }),
            Self::Subtask { parent_task_id } => Some(TaskPlacement::Subtask {
                parent_task_id: parent_task_id.clone(),
            }),
        }
    }

    fn owner(&self, id: &str) -> DescriptionOwner {
        match self {
            Self::Epic => DescriptionOwner::Epic(id.into()),
            _ => DescriptionOwner::Work(id.into()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTaskSettings {
    priority: i32,
    acceptance_criteria: Vec<String>,
    prerequisites: Vec<String>,
    blockers: Vec<String>,
    checklist_mode: Option<ChecklistExecutionMode>,
    branch_strategy: Option<BranchStrategy>,
    assignment: Option<WorkspaceAssignment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceAssignment {
    provider: Provider,
    profile_id: String,
    model_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDraft {
    description: String,
    settings: Option<WorkspaceTaskSettings>,
    state: Option<TaskWorkflowState>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceWorkEditor {
    title: String,
    placement: WorkspacePlacement,
    draft: WorkspaceDraft,
    branch_locked: bool,
    version: String,
}

struct WorkSnapshot {
    editor: WorkspaceWorkEditor,
    description: DescriptionEditorState,
}

impl Store {
    pub async fn create_workspace_work(
        &self,
        project: String,
        title: String,
        placement: WorkspacePlacement,
        draft: Option<WorkspaceDraft>,
    ) -> Result<String> {
        let title = checked_title(title)?;
        let draft = draft.unwrap_or_default();
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let origin = AgileEventOrigin::Operator;
            let id = if let Some(task_placement) = placement.task_placement() {
                let parent = match &task_placement {
                    TaskPlacement::Subtask { parent_task_id } => {
                        Some(WorkStateChanges::capture(&tx, &project, parent_task_id)?)
                    }
                    _ => None,
                };
                let task =
                    insert_project_work_record(&tx, project.clone(), title, &task_placement)?;
                let mut changes =
                    parent.unwrap_or(WorkStateChanges::capture(&tx, &project, &task.id)?);
                if matches!(&task_placement, TaskPlacement::Subtask { .. }) {
                    changes.include(WorkStateChanges::capture(&tx, &project, &task.id)?);
                }
                if let Some(settings) = draft.settings {
                    write_settings(&tx, &project, &task.id, &placement, settings)?;
                }
                set_state(
                    &tx,
                    &project,
                    &task.id,
                    draft.state,
                    Some(TaskWorkflowState::Backlog),
                )?;
                write_new_description(&tx, &project, placement.owner(&task.id), draft.description)?;
                publish_work_creation(&tx, &task, task_placement, &origin)?;
                changes.publish(&tx, &project, &origin)?;
                task.id
            } else {
                ensure!(
                    draft.settings.is_none() && draft.state.is_none(),
                    "epic state follows its tasks"
                );
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO project_epics(project_id, id, title) VALUES (?1, ?2, ?3)",
                    params![project, id, title],
                )?;
                write_new_description(
                    &tx,
                    &project,
                    DescriptionOwner::Epic(id.clone()),
                    draft.description,
                )?;
                id
            };
            tx.commit()?;
            Ok(id)
        })
        .await
    }

    pub async fn workspace_work_editor(
        &self,
        project: String,
        id: String,
    ) -> Result<WorkspaceWorkEditor> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            Ok(read_snapshot(&tx, &project, &id)?.editor)
        })
        .await
    }

    pub async fn update_workspace_work(
        &self,
        project: String,
        id: String,
        expected: String,
        title: String,
        placement: WorkspacePlacement,
        draft: WorkspaceDraft,
    ) -> Result<()> {
        let title = checked_title(title)?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = read_snapshot(&tx, &project, &id)?;
            ensure!(
                current.editor.version == expected,
                "the work changed; reload it before saving"
            );
            write_editor_description(
                &tx,
                &project,
                current.editor.placement.owner(&id),
                current.description,
                draft.description,
            )?;
            match (&current.editor.placement, &placement) {
                (WorkspacePlacement::Epic, WorkspacePlacement::Epic) => {
                    ensure!(
                        draft.settings.is_none() && draft.state.is_none(),
                        "epic state follows its tasks"
                    );
                    tx.execute(
                        "UPDATE project_epics SET title = ?3 WHERE project_id = ?1 AND id = ?2",
                        params![project, id, title],
                    )?;
                }
                (WorkspacePlacement::Task { .. }, WorkspacePlacement::Task { .. })
                | (WorkspacePlacement::Subtask { .. }, WorkspacePlacement::Subtask { .. }) => {
                    let mut changes = WorkStateChanges::capture(&tx, &project, &id)?;
                    if current.editor.placement != placement {
                        if let WorkspacePlacement::Subtask { parent_task_id } = &placement {
                            ensure!(
                                !current.editor.branch_locked,
                                "the parent is locked once worktree provisioning begins"
                            );
                            changes.include(WorkStateChanges::capture(
                                &tx,
                                &project,
                                parent_task_id,
                            )?);
                        }
                        write_placement(&tx, &project, &id, &placement)?;
                    }
                    tx.execute(
                        "UPDATE project_tasks SET title = ?3 WHERE project_id = ?1 AND id = ?2",
                        params![project, id, title],
                    )?;
                    let settings = draft.settings.context("task settings are missing")?;
                    if current.editor.draft.settings.as_ref() != Some(&settings) {
                        write_settings(&tx, &project, &id, &placement, settings)?;
                    }
                    set_state(&tx, &project, &id, draft.state, current.editor.draft.state)?;
                    changes.publish(&tx, &project, &AgileEventOrigin::Operator)?;
                }
                _ => anyhow::bail!("the work type cannot be changed"),
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
}

fn checked_title(title: String) -> Result<String> {
    let title = title.trim().to_owned();
    ensure!(!title.is_empty(), "the title is empty");
    Ok(title)
}

fn read_snapshot(db: &Connection, project: &str, id: &str) -> Result<WorkSnapshot> {
    let row = db
        .query_row(
            "SELECT t.title, m.epic_id, m.parent_task_id FROM project_tasks t
         LEFT JOIN project_task_memberships m ON m.project_id = t.project_id AND m.task_id = t.id
         WHERE t.project_id = ?1 AND t.id = ?2",
            params![project, id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let (title, placement, settings, state, branch_locked) =
        if let Some((title, epic_id, parent_task_id)) = row {
            let (parent, strategy) = branch_strategy_for_scope(db, project, id)?;
            let details = load_task_details(db, project, id)?;
            let settings = WorkspaceTaskSettings {
                priority: details.priority,
                acceptance_criteria: details.acceptance_criteria,
                prerequisites: details.prerequisites,
                blockers: details.blockers,
                checklist_mode: checklist_mode_override(db, project, id)?,
                branch_strategy: (parent == id)
                    .then_some(strategy.override_strategy)
                    .flatten(),
                assignment: details
                    .planned_assignments
                    .into_iter()
                    .find(|entry| entry.role == "implementer")
                    .map(|entry| WorkspaceAssignment {
                        provider: entry.provider,
                        profile_id: entry.profile_id,
                        model_id: entry.model,
                    }),
            };
            let placement = parent_task_id
                .map_or(WorkspacePlacement::Task { epic_id }, |parent_task_id| {
                    WorkspacePlacement::Subtask { parent_task_id }
                });
            (
                title,
                placement,
                Some(settings),
                Some(current_work_state(db, project, id)?),
                strategy.locked,
            )
        } else {
            let title: String = db
                .query_row(
                    "SELECT title FROM project_epics WHERE project_id = ?1 AND id = ?2",
                    params![project, id],
                    |row| row.get(0),
                )
                .optional()?
                .context("work is outside the selected project")?;
            (title, WorkspacePlacement::Epic, None, None, false)
        };
    let description = load_editor_state(db, project, &placement.owner(id))?;
    let draft = WorkspaceDraft {
        description: description
            .draft
            .as_ref()
            .map(|draft| draft.markdown.clone())
            .or_else(|| {
                description
                    .saved
                    .as_ref()
                    .and_then(|saved| saved.markdown.clone())
            })
            .unwrap_or_default(),
        settings,
        state,
    };
    let version = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            project,
            id,
            &title,
            &placement,
            &draft,
            &description,
            branch_locked
        ))?)
    );
    Ok(WorkSnapshot {
        editor: WorkspaceWorkEditor {
            title,
            placement,
            draft,
            branch_locked,
            version,
        },
        description,
    })
}

fn write_placement(
    db: &Connection,
    project: &str,
    id: &str,
    placement: &WorkspacePlacement,
) -> Result<()> {
    let (epic, parent) = match placement {
        WorkspacePlacement::Task { epic_id } => (epic_id.as_deref(), None),
        WorkspacePlacement::Subtask { parent_task_id } => (None, Some(parent_task_id.as_str())),
        WorkspacePlacement::Epic => return Ok(()),
    };
    if epic.is_none() && parent.is_none() {
        db.execute(
            "DELETE FROM project_task_memberships WHERE project_id = ?1 AND task_id = ?2",
            params![project, id],
        )?;
    } else {
        db.execute("INSERT INTO project_task_memberships(project_id, task_id, epic_id, parent_task_id) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(project_id, task_id) DO UPDATE SET epic_id = excluded.epic_id, parent_task_id = excluded.parent_task_id",
            params![project, id, epic, parent]).context("the parent must belong to the same project and have the correct work type")?;
    }
    Ok(())
}

fn write_settings(
    db: &Connection,
    project: &str,
    id: &str,
    placement: &WorkspacePlacement,
    settings: WorkspaceTaskSettings,
) -> Result<()> {
    let mut details = load_task_details(db, project, id)?;
    details.priority = settings.priority;
    details.acceptance_criteria = settings.acceptance_criteria;
    details.prerequisites = settings.prerequisites;
    details.blockers = settings.blockers;
    details
        .planned_assignments
        .retain(|entry| entry.role != "implementer");
    if let Some(assignment) = settings.assignment {
        ensure!(
            !assignment.profile_id.trim().is_empty()
                && assignment
                    .model_id
                    .as_ref()
                    .is_none_or(|model| !model.trim().is_empty()),
            "an assignment requires a valid profile and model"
        );
        details.planned_assignments.push(PlannedAssignment {
            role: "implementer".into(),
            provider: assignment.provider,
            profile_id: assignment.profile_id,
            model: assignment.model_id,
        });
    }
    write_task_details(db, project, id, details)?;
    write_checklist_mode_override(db, project, id, settings.checklist_mode)?;
    if matches!(placement, WorkspacePlacement::Task { .. }) {
        write_task_branch_strategy(db, project, id, settings.branch_strategy)?;
    } else {
        ensure!(
            settings.branch_strategy.is_none(),
            "a sub-task inherits its task's branch strategy"
        );
    }
    Ok(())
}

fn set_state(
    db: &Connection,
    project: &str,
    id: &str,
    choice: Option<TaskWorkflowState>,
    previous: Option<TaskWorkflowState>,
) -> Result<()> {
    if let Some(choice) = choice.filter(|choice| Some(*choice) != previous) {
        apply_work_transition(
            db,
            project,
            id,
            current_work_state(db, project, id)?,
            choice,
        )?;
    }
    Ok(())
}

fn write_new_description(
    db: &Connection,
    project: &str,
    owner: DescriptionOwner,
    markdown: String,
) -> Result<()> {
    if !markdown.is_empty() {
        write_description(db, project, owner, None, Some(markdown))?;
    }
    Ok(())
}

fn write_editor_description(
    db: &Connection,
    project: &str,
    owner: DescriptionOwner,
    state: DescriptionEditorState,
    markdown: String,
) -> Result<()> {
    let markdown = (!markdown.is_empty()).then_some(markdown);
    if let Some(draft) = state.draft {
        ensure!(
            draft.base.as_ref().map(|saved| &saved.id)
                == state.saved.as_ref().map(|saved| &saved.id),
            "the saved description changed; resolve its draft in Context before saving"
        );
        ensure!(
            consume_draft(db, project, &owner, &draft.revision)?,
            "the description draft changed"
        );
    } else if state
        .saved
        .as_ref()
        .and_then(|saved| saved.markdown.as_ref())
        == markdown.as_ref()
    {
        return Ok(());
    }
    write_description(db, project, owner, state.saved, markdown)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[tokio::test]
    async fn work_editor_creates_and_updates_atomically() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let checkout = temp.path().join("repo");
        fs::create_dir(&checkout)?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&checkout)
                .status()?
                .success()
        );
        let store = Store::open(temp.path().join("profile.sqlite"))?;
        let project = crate::repository::register(&store, checkout)
            .await?
            .project_id;
        let epic = store
            .create_workspace_work(
                project.clone(),
                "Release".into(),
                WorkspacePlacement::Epic,
                Some(WorkspaceDraft {
                    description: "Release notes".into(),
                    ..Default::default()
                }),
            )
            .await?;
        let placement = WorkspacePlacement::Task {
            epic_id: Some(epic.clone()),
        };
        let draft = WorkspaceDraft {
            description: "Validate the installer".into(),
            state: Some(TaskWorkflowState::Ready),
            settings: Some(WorkspaceTaskSettings {
                priority: 1,
                acceptance_criteria: vec!["Verify upgrade".into()],
                checklist_mode: Some(ChecklistExecutionMode::RunByItem),
                branch_strategy: Some(BranchStrategy::SharedBranchPerTask),
                ..Default::default()
            }),
        };
        let task = store
            .create_workspace_work(
                project.clone(),
                "Installer".into(),
                placement.clone(),
                Some(draft.clone()),
            )
            .await?;
        let editor = store
            .workspace_work_editor(project.clone(), task.clone())
            .await?;
        assert_eq!(editor.placement, placement);
        assert_eq!(editor.draft, draft);

        let mut invalid = draft.clone();
        invalid
            .settings
            .as_mut()
            .unwrap()
            .prerequisites
            .push("other-project-work".into());
        assert!(
            store
                .update_workspace_work(
                    project.clone(),
                    task.clone(),
                    editor.version.clone(),
                    "Uncommitted title".into(),
                    placement.clone(),
                    invalid
                )
                .await
                .is_err()
        );
        let saved = store
            .workspace_work_editor(project.clone(), task.clone())
            .await?;
        assert_eq!(saved.title, "Installer");
        assert_eq!(saved.version, editor.version);

        let mut next = draft;
        next.description = "Inspect the saved release receipt".into();
        store
            .update_workspace_work(
                project.clone(),
                task.clone(),
                editor.version.clone(),
                "Upgrade".into(),
                placement.clone(),
                next.clone(),
            )
            .await?;
        assert!(
            store
                .update_workspace_work(
                    project.clone(),
                    task.clone(),
                    editor.version,
                    "Stale title".into(),
                    placement,
                    next.clone()
                )
                .await
                .is_err()
        );
        assert_eq!(
            store
                .workspace_work_editor(project.clone(), task)
                .await?
                .draft,
            next
        );
        assert_eq!(
            store
                .workspace_work_editor(project, epic)
                .await?
                .draft
                .description,
            "Release notes"
        );
        Ok(())
    }
}
