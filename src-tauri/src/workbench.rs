use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::{
    repository::ProjectCheckout,
    runs::{RunRecord, load_run},
    store::Store,
};

const ACTIVE_CHECKOUT: &str = "active_checkout";
const MAX_LAYOUT_BYTES: usize = 512 * 1024;

impl Store {
    pub async fn inspect_conversation_run(
        &self,
        project_id: String,
        conversation_id: String,
        run_id: String,
    ) -> Result<RunRecord> {
        self.execute(move |db| {
            let belongs: bool = db.query_row(
                "SELECT EXISTS(SELECT 1
                 FROM agent_attempts a JOIN conversations c ON c.id = a.conversation_id
                 JOIN checkouts w ON w.id = c.checkout_id
                 WHERE a.run_id = ?1 AND c.id = ?2 AND w.project_id = ?3)",
                params![run_id, conversation_id, project_id],
                |row| row.get(0),
            )?;
            ensure!(belongs, "Run does not belong to this conversation and project");
            load_run(db, &run_id)
        })
        .await
    }

    pub async fn registered_checkouts(&self) -> Result<Vec<ProjectCheckout>> {
        self.execute(|db| {
            let mut statement = db.prepare(
                "SELECT p.id, p.root, c.id, c.root
                 FROM projects p JOIN checkouts c ON c.project_id = p.id
                 ORDER BY p.root COLLATE NOCASE, c.root COLLATE NOCASE",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(ProjectCheckout {
                    project_id: row.get(0)?,
                    repository_root: row.get(1)?,
                    checkout_id: row.get(2)?,
                    checkout_root: row.get(3)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn active_checkout(&self) -> Result<Option<ProjectCheckout>> {
        self.execute(|db| {
            Ok(db
                .query_row(
                    "SELECT p.id, p.root, c.id, c.root
                 FROM workspace_settings s
                 JOIN checkouts c ON c.id = s.value
                 JOIN projects p ON p.id = c.project_id
                 WHERE s.key = ?1",
                    [ACTIVE_CHECKOUT],
                    |row| {
                        Ok(ProjectCheckout {
                            project_id: row.get(0)?,
                            repository_root: row.get(1)?,
                            checkout_id: row.get(2)?,
                            checkout_root: row.get(3)?,
                        })
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn select_active_checkout(&self, checkout_id: String) -> Result<()> {
        self.execute(move |db| {
            let updated = db.execute(
                "INSERT INTO workspace_settings (key, value)
                 SELECT ?1, id FROM checkouts WHERE id = ?2
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![ACTIVE_CHECKOUT, checkout_id],
            )?;
            ensure!(updated == 1, "checkout is not registered");
            Ok(())
        })
        .await
    }

    pub async fn workbench_layout(
        &self,
        checkout_id: String,
        workspace: String,
    ) -> Result<Option<String>> {
        self.execute(move |db| {
            Ok(db.query_row(
                "SELECT layout FROM workbench_layouts WHERE checkout_id = ?1 AND workspace = ?2",
                params![checkout_id, workspace],
                |row| row.get(0),
            ).optional()?)
        })
        .await
    }

    pub async fn save_workbench_layout(
        &self,
        checkout_id: String,
        workspace: String,
        layout: String,
    ) -> Result<()> {
        ensure!(
            layout.len() <= MAX_LAYOUT_BYTES,
            "workbench layout is too large"
        );
        ensure!(
            serde_json::from_str::<Value>(&layout)?.is_object(),
            "workbench layout must be a JSON object"
        );
        self.execute(move |db| {
            db.execute(
                "INSERT INTO workbench_layouts (checkout_id, workspace, layout) VALUES (?1, ?2, ?3)
                 ON CONFLICT (checkout_id, workspace) DO UPDATE SET layout = excluded.layout",
                params![checkout_id, workspace, layout],
            )?;
            Ok(())
        })
        .await
    }
}
