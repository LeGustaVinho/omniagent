use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    conversations::{MessageRole, SessionScope},
    jobs::{JobRequest, insert_selected_operator_job},
    model_catalog::ModelSelection,
    protection,
    providers::SessionMode,
    repository::{
        self, CheckoutLayout,
        comparison::{materialize_comparison, verify_comparison},
    },
    store::Store,
};

mod evidence;
mod outcomes;
pub(crate) mod selection;
mod synthesis;
pub use evidence::ComparisonEnvironment;
pub use outcomes::ComparisonOutcome;
pub use selection::{ComparisonInput, ComparisonSelection};
pub use synthesis::ComparisonSynthesis;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelComparison {
    pub id: String,
    pub commit: String,
    pub tree: String,
    pub prompt: String,
    pub candidates: Vec<ComparisonCandidate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonCandidate {
    pub selection: ModelSelection,
    pub job_id: String,
    pub run_id: Option<String>,
    pub conversation_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub state: String,
}

pub(crate) struct ComparisonPin {
    pub(crate) group_id: String,
    pub(crate) commit: String,
    pub(crate) tree: String,
    root: PathBuf,
    prompt: String,
    selection: ModelSelection,
}

impl ComparisonPin {
    pub(crate) fn checkout(&self) -> Result<CheckoutLayout> {
        verify_comparison(&self.root, &self.commit, &self.tree)
    }
}

impl Store {
    pub async fn comparison_source_commit(
        &self,
        project: String,
        checkout: String,
    ) -> Result<Option<String>> {
        let root = registered_root(self, project, checkout.clone()).await?;
        let layout = repository::verify_checkout(self, checkout, root).await?;
        tokio::task::spawn_blocking(move || repository::base_commit(&layout.root)).await?
    }

    pub async fn start_model_comparison(
        &self,
        project: String,
        checkout: String,
        commit: String,
        prompt: String,
        profiles: Vec<SessionScope>,
        storage: PathBuf,
    ) -> Result<ModelComparison> {
        let prompt = prompt.trim().to_owned();
        ensure!(!prompt.is_empty(), "Enter the shared comparison prompt");
        ensure!(
            profiles.len() >= 2,
            "Choose at least two distinct models or execution profiles"
        );
        let root = registered_root(self, project.clone(), checkout.clone()).await?;
        let mut selections = HashSet::new();
        let profiles = profiles
            .into_iter()
            .map(|scope| {
                ensure!(
                    scope.checkout == root && scope.mode == SessionMode::ReadOnly,
                    "Each candidate needs a verified read-only profile for this project"
                );
                let selection = selected_model(&scope)?;
                ensure!(
                    selections.insert(selection.clone()),
                    "Select each model and execution profile only once"
                );
                Ok((selection, scope))
            })
            .collect::<Result<Vec<_>>>()?;
        let verified = repository::verify_checkout(self, checkout.clone(), root).await?;
        let commit = commit.trim().to_ascii_lowercase();
        let (lease, path, tree) = tokio::task::spawn_blocking({
            let commit = commit.clone();
            move || materialize_comparison(&verified, &commit, &storage)
        })
        .await??;
        let group = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let id = Uuid::new_v4().to_string();
                let path = path
                    .to_str()
                    .context("The comparison checkout path is unavailable")?;
                ensure!(
                    !tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM checkouts WHERE root = ?1 COLLATE NOCASE)",
                        [path],
                        |row| row.get::<_, bool>(0)
                    )?,
                    "The comparison checkout is already registered for project work"
                );
                tx.execute(
                    "INSERT INTO model_comparison_groups VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        id,
                        project,
                        checkout,
                        commit,
                        tree,
                        path,
                        protection::protect(prompt.as_bytes())?
                    ],
                )?;
                for (position, (selection, scope)) in profiles.into_iter().enumerate() {
                    let job = insert_selected_operator_job(
                        &tx,
                        project.clone(),
                        scope,
                        JobRequest {
                            checkout_id: Some(checkout.clone()),
                            task_id: None,
                            mode: SessionMode::ReadOnly,
                            instructions: prompt.clone(),
                            dependencies: Vec::new(),
                            requires_bridge: false,
                        },
                    )?;
                    tx.execute(
                        "INSERT INTO model_comparison_candidates VALUES (?1, ?2, ?3, ?4)",
                        params![
                            id,
                            i64::try_from(position)?,
                            job.id,
                            serde_json::to_string(&selection)?
                        ],
                    )?;
                }
                let group = load_group(&tx, &project, &id)?;
                tx.commit()?;
                lease.preserve();
                Ok(group)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(group)
    }

    pub async fn model_comparisons(&self, project: String) -> Result<Vec<ModelComparison>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let ids = {
                let mut query = tx.prepare("SELECT id FROM model_comparison_groups WHERE project_id = ?1 ORDER BY rowid DESC")?;
                query.query_map([&project], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ids.into_iter().map(|id| load_group(&tx, &project, &id)).collect()
        }).await
    }

    pub(crate) async fn model_comparison_pin_for_run(
        &self,
        run: String,
    ) -> Result<Option<ComparisonPin>> {
        self.execute(move |db| {
            let saved = db.query_row(
                "WITH executions AS (
                    SELECT c.group_id, c.job_id, c.selection, g.prompt FROM model_comparison_candidates c JOIN model_comparison_groups g ON g.id = c.group_id
                    UNION ALL SELECT group_id, job_id, selection, prompt FROM model_comparison_syntheses
                 ) SELECT g.id, g.commit_oid, g.tree_oid, g.comparison_checkout, e.prompt, e.selection
                 FROM executions e JOIN model_comparison_groups g ON g.id = e.group_id
                 JOIN job_attempts a ON a.job_id = e.job_id JOIN runs r ON r.id = a.run_id AND r.checkout_id = g.source_checkout_id
                 WHERE r.id = ?1", [run],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, String>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, String>(5)?)),
            ).optional()?;
            saved.map(|(group_id, commit, tree, root, protected, selection)| Ok(ComparisonPin {
                group_id, commit, tree, root: root.into(), prompt: String::from_utf8(protection::unprotect(&protected)?)?,
                selection: serde_json::from_str(&selection)?,
            })).transpose()
        }).await
    }
}

impl ComparisonPin {
    pub(crate) fn verify_attempt(&self, attempt: &crate::conversations::AttemptPlan) -> Result<()> {
        ensure!(
            attempt.scope.mode == SessionMode::ReadOnly
                && !attempt.requires_bridge
                && matches!(attempt.intent, crate::providers::SessionIntent::Start)
                && attempt.attachments.is_empty()
                && selected_model(&attempt.scope)? == self.selection
                && matches!(attempt.input.as_slice(), [message] if message.role == MessageRole::User && message.text == self.prompt),
            "The candidate no longer matches the shared comparison input and read-only profile"
        );
        Ok(())
    }
}

fn selected_model(scope: &SessionScope) -> Result<ModelSelection> {
    Ok(ModelSelection {
        provider: scope.provider,
        profile_id: scope.profile_id.clone(),
        model_id: scope
            .model
            .clone()
            .context("Choose an exact model for the candidate")?,
    })
}

async fn registered_root(store: &Store, project: String, checkout: String) -> Result<PathBuf> {
    store
        .execute(move |db| {
            let root = db
                .query_row(
                    "SELECT root FROM checkouts WHERE id = ?1 AND project_id = ?2",
                    params![checkout, project],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .context("Choose a checkout in this project")?;
            Ok(root.into())
        })
        .await
}

fn load_group(db: &Connection, project: &str, id: &str) -> Result<ModelComparison> {
    let (commit, tree, prompt) = db.query_row(
        "SELECT commit_oid, tree_oid, prompt FROM model_comparison_groups WHERE project_id = ?1 AND id = ?2",
        params![project, id], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Vec<u8>>(2)?)),
    )?;
    let mut query = db.prepare(
        "SELECT c.selection, j.id, a.run_id, agent.conversation_id, dispatch.id,
            CASE WHEN r.status IN ('succeeded','failed','cancelled','interrupted') THEN r.status
                WHEN dispatch.delivery = 'submitted' THEN r.status
                ELSE j.queue_state END
         FROM model_comparison_candidates c JOIN jobs j ON j.id = c.job_id
         LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (SELECT MAX(latest.ordinal) FROM job_attempts latest WHERE latest.job_id = j.id)
         LEFT JOIN runs r ON r.id = a.run_id
         LEFT JOIN agent_attempts agent ON agent.run_id = r.id
         LEFT JOIN context_snapshots snapshot ON snapshot.run_id = r.id
         LEFT JOIN dispatches dispatch ON dispatch.snapshot_id = snapshot.id AND dispatch.ordinal = 1
         WHERE c.group_id = ?1 ORDER BY c.position",
    )?;
    let rows = query.query_map([id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })?;
    let candidates = rows
        .map(|row| {
            let (selection, job_id, run_id, conversation_id, dispatch_id, state) = row?;
            Ok(ComparisonCandidate {
                selection: serde_json::from_str(&selection)?,
                job_id,
                run_id,
                conversation_id,
                dispatch_id,
                state,
            })
        })
        .collect::<Result<_>>()?;
    Ok(ModelComparison {
        id: id.into(),
        commit,
        tree,
        prompt: String::from_utf8(protection::unprotect(&prompt)?)?,
        candidates,
    })
}
