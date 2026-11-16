use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{providers::Provider, store::Store};

use super::ModelSelection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDefaultPurpose {
    Conversation,
    Planning,
    Implementation,
}

impl ModelDefaultPurpose {
    fn key(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Planning => "planning",
            Self::Implementation => "implementation",
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectModelDefaults {
    pub conversation: Option<ModelSelection>,
    pub planning: Option<ModelSelection>,
    pub implementation: Option<ModelSelection>,
}

impl Store {
    pub async fn project_model_defaults(&self, project: String) -> Result<ProjectModelDefaults> {
        self.execute(move |db| load_project_defaults(db, &project))
            .await
    }

    pub async fn checkout_model_defaults(&self, checkout: String) -> Result<ProjectModelDefaults> {
        self.execute(move |db| {
            let project: String = db
                .query_row(
                    "SELECT project_id FROM checkouts WHERE id = ?1",
                    [checkout],
                    |row| row.get(0),
                )
                .optional()?
                .context("the checkout is no longer registered")?;
            load_project_defaults(db, &project)
        })
        .await
    }

    pub async fn set_project_model_default(
        &self,
        project: String,
        purpose: ModelDefaultPurpose,
        selection: Option<ModelSelection>,
    ) -> Result<ProjectModelDefaults> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            require_project(&tx, &project)?;
            save(&tx, &project_key(&project, purpose), selection.as_ref())?;
            let defaults = load_project_defaults(&tx, &project)?;
            tx.commit()?;
            Ok(defaults)
        })
        .await
    }

    pub async fn execution_profile_default_model(
        &self,
        provider: Provider,
        profile: String,
    ) -> Result<Option<ModelSelection>> {
        self.execute(move |db| load(db, &profile_key(provider, &profile)))
            .await
    }

    pub async fn set_execution_profile_default_model(
        &self,
        provider: Provider,
        profile: String,
        selection: Option<ModelSelection>,
    ) -> Result<Option<ModelSelection>> {
        ensure!(
            !profile.trim().is_empty(),
            "an execution profile is required"
        );
        ensure!(
            selection
                .as_ref()
                .is_none_or(|model| model.provider == provider && model.profile_id == profile),
            "the chosen model belongs to another execution profile"
        );
        self.execute(move |db| {
            save(db, &profile_key(provider, &profile), selection.as_ref())?;
            Ok(selection)
        })
        .await
    }
}

fn require_project(db: &Connection, project: &str) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project],
        |row| row.get(0),
    )?;
    ensure!(exists, "the project is no longer registered");
    Ok(())
}

fn load_project_defaults(db: &Connection, project: &str) -> Result<ProjectModelDefaults> {
    require_project(db, project)?;
    Ok(ProjectModelDefaults {
        conversation: load(db, &project_key(project, ModelDefaultPurpose::Conversation))?,
        planning: load(db, &project_key(project, ModelDefaultPurpose::Planning))?,
        implementation: load(
            db,
            &project_key(project, ModelDefaultPurpose::Implementation),
        )?,
    })
}

fn project_key(project: &str, purpose: ModelDefaultPurpose) -> String {
    format!("models.project.{project}.{}", purpose.key())
}

fn profile_key(provider: Provider, profile: &str) -> String {
    format!("models.profile.{}.{profile}", provider.as_str())
}

fn load(db: &Connection, key: &str) -> Result<Option<ModelSelection>> {
    let selection: Option<String> = db
        .query_row(
            "SELECT value FROM workspace_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    selection
        .map(|value| Ok(serde_json::from_str(&value)?))
        .transpose()
}

fn save(db: &Connection, key: &str, selection: Option<&ModelSelection>) -> Result<()> {
    if let Some(selection) = selection {
        ensure!(
            !selection.profile_id.trim().is_empty() && !selection.model_id.trim().is_empty(),
            "a model and execution profile are required"
        );
        db.execute(
            "INSERT INTO workspace_settings (key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(selection)?],
        )?;
    } else {
        db.execute("DELETE FROM workspace_settings WHERE key = ?1", [key])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
