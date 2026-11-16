use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::workflows::{
    NodeConfiguration, WorkflowDefinition, WorkflowRevision, command::validate_command_binding,
    compile_resolved, digest, registry::supports_pinned_execution, runtime::load_definition,
};

use super::{
    DraftScope, Store, WorkflowNodePosition, latest_request, load_head, load_scope, new_id,
    reconcile, seal,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPublication {
    pub id: String,
    pub workflow: WorkflowRevision,
    pub base: Option<WorkflowRevision>,
    pub draft_id: String,
    pub reviewed_revision_id: String,
    pub originating_request_id: Option<String>,
    pub originating_run_id: Option<String>,
    pub decision_source: PublicationDecision,
    pub confirmed_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationDecision {
    DesktopUser,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkflowPublicationOutcome {
    Published { publication: WorkflowPublication },
    StaleBase { current: WorkflowDefinition },
}

impl Store {
    pub async fn publish_reviewed_workflow(
        &self,
        project: String,
        draft: String,
        reviewed_revision: String,
        expected_base: Option<WorkflowRevision>,
    ) -> Result<WorkflowPublicationOutcome> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let scope = load_scope(&tx, &project, &draft)?;
            ensure!(base_version(&scope) == expected_base, "The confirmed base does not match the draft's saved base");
            if let Some(publication) = load_publication(&tx, &project, &draft)? {
                ensure!(publication.reviewed_revision_id == reviewed_revision, "This draft already has a different published revision");
                return Ok(WorkflowPublicationOutcome::Published { publication });
            }
            reconcile(&tx, &draft, &scope)?;
            let scope = load_scope(&tx, &project, &draft)?;
            ensure!(scope.head.as_deref() == Some(&reviewed_revision), "The draft changed. Review its latest revision before publishing.");
            ensure!(!matches!(latest_request(&tx, &draft)?.status.state.as_str(),
                "queued" | "starting" | "running" | "waiting_for_input" | "paused"),
                "Wait for the current authoring request before publishing");
            if let Some(current) = published_head(&tx, &project, &scope.workflow)?
                && Some(&current) != expected_base.as_ref()
            {
                let current = load_definition(&tx, &project, &current)?
                    .context("The current published version is unavailable")?;
                tx.commit()?;
                return Ok(WorkflowPublicationOutcome::StaleBase { current });
            }
            if let Some(base) = &expected_base {
                load_definition(&tx, &project, base)?.context("The confirmed base is unavailable in this project")?;
            }
            let reviewed = load_head(&tx, &draft, Some(&reviewed_revision))?
                .context("The reviewed draft revision is unavailable")?;
            let mut definition = compile_resolved(scope.workflow.clone(), reviewed.graph, |node| {
                reviewed.definitions.get(&node.id)
            })?;
            for node in &definition.nodes {
                ensure!(supports_pinned_execution(&node.definition),
                    "The pinned definition for {} is no longer supported. Review its available update before publishing.",
                    node.instance.id);
                if let NodeConfiguration::Command(command) = &node.instance.configuration {
                    validate_command_binding(&tx, &project, command)
                        .with_context(|| format!("Could not resolve the Command binding for {}", node.instance.id))?;
                }
            }
            if !reviewed.layout.is_empty() {
                // Include reviewed canvas content in identity without changing
                // the runtime definition format used by preexisting pins.
                definition.version.revision = digest(&serde_json::to_vec(&(&definition.version.revision, &reviewed.layout))?);
            }
            let inserted = tx.execute(
                "INSERT INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, workflow_id, revision) DO NOTHING",
                params![project, definition.version.id, definition.version.revision, seal(&definition)?],
            )?;
            if inserted == 0 {
                ensure!(load_definition(&tx, &project, &definition.version)?.as_ref() == Some(&definition),
                    "The existing immutable workflow definition differs from the reviewed graph");
            }
            tx.execute(
                "INSERT INTO workflow_published_heads VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id, workflow_id) DO UPDATE SET revision = excluded.revision",
                params![project, definition.version.id, definition.version.revision],
            )?;
            let (request, run) = closest_authoring_origin(&tx, &draft, &reviewed_revision)?;
            tx.execute(
                "INSERT INTO workflow_authoring_publications
                 (id, project_id, workflow_id, workflow_revision, previous_revision, draft_id,
                  reviewed_revision_id, originating_request_id, originating_run_id, decision_source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'desktop_user')",
                params![new_id(), project, definition.version.id, definition.version.revision,
                    scope.base, draft, reviewed_revision, request, run],
            )?;
            let publication = load_publication(&tx, &project, &draft)?.context("The publication receipt is unavailable")?;
            tx.commit()?;
            Ok(WorkflowPublicationOutcome::Published { publication })
        }).await
    }
}

fn base_version(scope: &DraftScope) -> Option<WorkflowRevision> {
    scope.base.as_ref().map(|revision| WorkflowRevision {
        id: scope.workflow.clone(),
        revision: revision.clone(),
    })
}

fn published_head(
    db: &Connection,
    project: &str,
    workflow: &str,
) -> Result<Option<WorkflowRevision>> {
    Ok(db.query_row(
        "SELECT revision FROM workflow_published_heads WHERE project_id = ?1 AND workflow_id = ?2",
        params![project, workflow],
        |row| Ok(WorkflowRevision { id: workflow.into(), revision: row.get(0)? }),
    ).optional()?)
}

pub(super) fn load_publication(
    db: &Connection,
    project: &str,
    draft: &str,
) -> Result<Option<WorkflowPublication>> {
    Ok(db
        .query_row(
            "SELECT id, workflow_id, workflow_revision, previous_revision, reviewed_revision_id,
                originating_request_id, originating_run_id, confirmed_at
         FROM workflow_authoring_publications WHERE project_id = ?1 AND draft_id = ?2",
            params![project, draft],
            |row| {
                let workflow_id: String = row.get(1)?;
                let base: Option<String> = row.get(3)?;
                Ok(WorkflowPublication {
                    id: row.get(0)?,
                    workflow: WorkflowRevision {
                        id: workflow_id.clone(),
                        revision: row.get(2)?,
                    },
                    base: base.map(|revision| WorkflowRevision {
                        id: workflow_id,
                        revision,
                    }),
                    draft_id: draft.into(),
                    reviewed_revision_id: row.get(4)?,
                    originating_request_id: row.get(5)?,
                    originating_run_id: row.get(6)?,
                    decision_source: PublicationDecision::DesktopUser,
                    confirmed_at: row.get(7)?,
                })
            },
        )
        .optional()?)
}

pub(super) fn ensure_editable(db: &Connection, draft: &str) -> Result<()> {
    let published = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_authoring_publications WHERE draft_id = ?1)",
        [draft],
        |row| row.get::<_, bool>(0),
    )?;
    ensure!(
        !published,
        "Start a new draft from a published revision to make changes"
    );
    Ok(())
}

pub(super) fn published_layout(
    db: &Connection,
    project: &str,
    version: &WorkflowRevision,
) -> Result<std::collections::BTreeMap<String, WorkflowNodePosition>> {
    let published = db
        .query_row(
            "SELECT draft_id, reviewed_revision_id FROM workflow_authoring_publications
         WHERE project_id = ?1 AND workflow_id = ?2 AND workflow_revision = ?3
         ORDER BY rowid DESC LIMIT 1",
            params![project, version.id, version.revision],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((draft, reviewed_revision)) = published else {
        return Ok(Default::default());
    };
    Ok(load_head(db, &draft, Some(&reviewed_revision))?
        .context("The published canvas revision is unavailable")?
        .layout)
}

fn closest_authoring_origin(
    db: &Connection,
    draft: &str,
    revision: &str,
) -> Result<(Option<String>, Option<String>)> {
    Ok(db.query_row(
        "WITH RECURSIVE lineage(id, parent, request, run, distance) AS (
            SELECT id, parent_revision, request_id, run_id, 0
            FROM workflow_authoring_revisions WHERE draft_id = ?1 AND id = ?2
            UNION ALL
            SELECT prior.id, prior.parent_revision, prior.request_id, prior.run_id, lineage.distance + 1
            FROM workflow_authoring_revisions prior JOIN lineage ON prior.id = lineage.parent
            WHERE prior.draft_id = ?1
         )
         SELECT request, run FROM lineage WHERE request IS NOT NULL AND run IS NOT NULL
         ORDER BY distance LIMIT 1",
        params![draft, revision],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.unwrap_or_default())
}

#[cfg(test)]
mod tests;
