use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::store::Store;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStrategy {
    #[default]
    Independent,
    SharedBranchPerTask,
}

impl BranchStrategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Independent => "independent",
            Self::SharedBranchPerTask => "shared_branch_per_task",
        }
    }
}

impl rusqlite::types::FromSql for BranchStrategy {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "independent" => Ok(Self::Independent),
            "shared_branch_per_task" => Ok(Self::SharedBranchPerTask),
            _ => Err(rusqlite::types::FromSqlError::InvalidType),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskBranchStrategy {
    pub override_strategy: Option<BranchStrategy>,
    pub effective_strategy: BranchStrategy,
    pub locked: bool,
}

pub(crate) fn branch_strategy_for_scope(
    db: &Connection,
    project: &str,
    scope: &str,
) -> Result<(String, TaskBranchStrategy)> {
    db.query_row(
        "SELECT COALESCE(m.parent_task_id, t.id), s.override_strategy,
                COALESCE(s.provisioned_strategy, s.override_strategy, p.strategy, 'independent'),
                s.provisioned_strategy IS NOT NULL
         FROM project_tasks t
         LEFT JOIN project_task_memberships m ON m.project_id = t.project_id AND m.task_id = t.id
         LEFT JOIN task_branch_strategies s ON s.project_id = t.project_id
            AND s.task_id = COALESCE(m.parent_task_id, t.id)
         LEFT JOIN project_branch_strategies p ON p.project_id = t.project_id
         WHERE t.project_id = ?1 AND t.id = ?2",
        params![project, scope],
        |row| {
            Ok((
                row.get(0)?,
                TaskBranchStrategy {
                    override_strategy: row.get(1)?,
                    effective_strategy: row.get(2)?,
                    locked: row.get(3)?,
                },
            ))
        },
    )
    .optional()?
    .context("work scope must belong to the selected project")
}

pub(crate) fn pin_branch_strategy(
    db: &Connection,
    project: &str,
    task: &str,
    strategy: TaskBranchStrategy,
) -> Result<()> {
    if !strategy.locked {
        db.execute(
            "INSERT INTO task_branch_strategies(project_id, task_id, provisioned_strategy)
             VALUES (?1, ?2, ?3) ON CONFLICT(project_id, task_id) DO UPDATE
             SET provisioned_strategy = excluded.provisioned_strategy",
            params![project, task, strategy.effective_strategy.as_str()],
        )?;
    }
    Ok(())
}

pub(crate) fn write_task_branch_strategy(
    db: &Connection,
    project: &str,
    task: &str,
    strategy: Option<BranchStrategy>,
) -> Result<TaskBranchStrategy> {
    let (parent, current) = branch_strategy_for_scope(db, project, task)?;
    ensure!(
        parent == task,
        "a sub-task inherits its task's branch strategy"
    );
    if current.override_strategy == strategy {
        return Ok(current);
    }
    ensure!(
        !current.locked,
        "branch strategy is locked once worktree provisioning begins"
    );
    db.execute(
        "INSERT INTO task_branch_strategies(project_id, task_id, override_strategy)
         VALUES (?1, ?2, ?3) ON CONFLICT(project_id, task_id) DO UPDATE
         SET override_strategy = excluded.override_strategy",
        params![project, task, strategy.map(BranchStrategy::as_str)],
    )?;
    Ok(branch_strategy_for_scope(db, project, task)?.1)
}

impl Store {
    pub async fn project_branch_strategy(&self, project_id: String) -> Result<BranchStrategy> {
        self.execute(move |db| {
            db.query_row(
                "SELECT COALESCE(s.strategy, 'independent') FROM projects p
                 LEFT JOIN project_branch_strategies s ON s.project_id = p.id WHERE p.id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
            .context("branch strategy requires a registered project")
        })
        .await
    }

    pub async fn set_project_branch_strategy(
        &self,
        project_id: String,
        strategy: BranchStrategy,
    ) -> Result<BranchStrategy> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO project_branch_strategies(project_id, strategy) VALUES (?1, ?2)
                 ON CONFLICT(project_id) DO UPDATE SET strategy = excluded.strategy",
                params![project_id, strategy.as_str()],
            )?;
            Ok(strategy)
        })
        .await
    }

    pub async fn task_branch_strategy(
        &self,
        project_id: String,
        task_id: String,
    ) -> Result<TaskBranchStrategy> {
        self.execute(move |db| Ok(branch_strategy_for_scope(db, &project_id, &task_id)?.1))
            .await
    }

    pub async fn set_task_branch_strategy(
        &self,
        project_id: String,
        task_id: String,
        strategy: Option<BranchStrategy>,
    ) -> Result<TaskBranchStrategy> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let updated = write_task_branch_strategy(&tx, &project_id, &task_id, strategy)?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }
}
