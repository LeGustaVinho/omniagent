use serde::{Deserialize, Serialize};

use super::TaskReference;
use crate::{
    repository::RepositoryMarkdownRevision,
    tasks::{ChecklistProgressReceipt, TaskDispatchContext, TaskWorkflowState},
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadWorkContextMode {
    Work,
    Markdown,
    Both,
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
