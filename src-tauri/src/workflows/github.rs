mod client;
mod policy;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{
    BuiltinNode, WorkflowRun,
    runtime::{load_definition, load_workflow_run},
};

pub use crate::repository::github::{
    CheckEvidence, CiSnapshot, CiState, GitHubRepository, PublicRepository, PullRequest,
    RepositorySlug, RequiredCheck,
};
pub(crate) use client::GithubDeliveryClient;
pub use policy::GithubSettings;

pub(crate) fn workflow_github_merge_destination(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<Option<String>> {
    let Some(run) = load_workflow_run(db, project, run)? else {
        return Ok(None);
    };
    if let Some((_, destination)) =
        delivery_pin(db, &run.id, run.plan_revision.as_deref().unwrap_or(""))?
    {
        return Ok(Some(destination));
    }
    Ok(workflow_github_merge_settings(db, &run)?.map(|(_, settings)| settings.destination_branch))
}

fn workflow_github_merge_settings(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Option<(String, GithubSettings)>> {
    let definition = load_definition(db, &run.project_id, &run.version)?
        .context("The pinned workflow definition is unavailable")?;
    if !definition.nodes.iter().any(|node| {
        node.instance.kind
            == BuiltinNode::Archived(crate::workflows::ArchivedNode::MergePullRequest)
    }) {
        return Ok(None);
    }
    Ok(Some(policy::load_settings(db, &run.project_id)?.context(
        "Configure the project's GitHub delivery destination before committing this workflow",
    )?))
}

fn delivery_pin(db: &Connection, run: &str, plan: &str) -> Result<Option<(String, String)>> {
    Ok(db
        .query_row(
            "SELECT policy_revision, destination_branch FROM workflow_github_delivery_pins
         WHERE pipeline_run_id = ?1 AND plan_key = ?2",
            params![run, plan],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

pub(crate) fn pin_before_workflow_commit(
    db: &Connection,
    unit: &super::WorkflowDeliveryUnit,
) -> Result<()> {
    let run = load_workflow_run(db, &unit.project_id, &unit.workflow_run_id)?
        .context("The commit belongs to an unavailable workflow")?;
    ensure!(
        run.plan_revision.as_ref() == Some(&unit.plan_revision),
        "The delivery plan changed before committing"
    );
    if let Some((revision, _)) = delivery_pin(db, &run.id, &unit.plan_revision)? {
        policy::require_settings(db, &unit.project_id, &revision)?;
        return Ok(());
    }
    let Some((revision, settings)) = workflow_github_merge_settings(db, &run)? else {
        return Ok(());
    };
    let milestone = crate::tasks::load_workflow_milestone_policy(db, &unit.project_id, &run.id)?;
    ensure!(
        milestone.destination_branch.as_deref() == Some(&settings.destination_branch),
        "The GitHub delivery policy and planned destination differ"
    );
    db.execute(
        "INSERT INTO workflow_github_delivery_pins VALUES (?1, ?2, ?3, ?4)",
        params![
            run.id,
            unit.plan_revision,
            revision,
            settings.destination_branch
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchConfiguration {
    pub limit: u16,
}

impl SearchConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=30).contains(&self.limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchEvidence {
    pub query: String,
    pub repositories: Vec<PublicRepository>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullEvidence {
    pub policy_revision: String,
    pub task_id: String,
    pub checkout_id: String,
    pub commit_evidence_id: String,
    pub git_artifact_id: String,
    pub requires_human: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_syncs: Vec<crate::repository::verified_merge::SourceSync>,
    pub pull: PullRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiEvidence {
    pub pull_artifact_id: String,
    pub pull: PullEvidence,
    pub snapshot: CiSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubWait {
    node_id: String,
    unit_id: Option<String>,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<GithubWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, NULLIF(unit_key, ''), reason FROM workflow_github_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND port IS NULL
         ORDER BY node_instance_id, unit_key",
    )?;
    let rows = query
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or("")],
            |row| {
                Ok(GithubWait {
                    node_id: row.get(0)?,
                    unit_id: row.get(1)?,
                    reason: row.get(2)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
