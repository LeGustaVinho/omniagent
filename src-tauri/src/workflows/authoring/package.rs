use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    store::Store,
    workflows::{NodeDefinition, WorkflowRevision, builtin_registry},
};

use super::{new_id, open, seal};

pub(crate) mod mcp;
mod model;
mod preview;
mod publish;
mod request;
mod simulation;
#[cfg(test)]
mod authoring_acceptance;
#[cfg(test)]
mod subflow_tests;
#[cfg(test)]
mod tests;
pub use model::*;
pub use preview::WorkflowPackagePreview;
pub(crate) use publish::ensure_package_runnable;
pub use simulation::{WorkflowSimulation, WorkflowSimulationScenario};

const MAX_MEMBERS: usize = 32;
const MAX_PACKAGE_BYTES: usize = 192 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageDocument {
    package: WorkflowPackage,
    definitions: BTreeMap<String, BTreeMap<String, NodeDefinition>>,
}

impl PackageDocument {
    fn capture(
        db: &Connection,
        project: &str,
        package: WorkflowPackage,
        previous: Option<&Self>,
    ) -> Result<Self> {
        validate_structure(&package)?;
        let registry: HashMap<_, _> = builtin_registry()
            .into_iter()
            .map(|definition| (definition.kind, definition))
            .collect();
        let definitions = package
            .members
            .iter()
            .map(|member| {
                let nodes = match &member.body {
                    PackageArtifactBody::Pipeline { graph }
                    | PackageArtifactBody::Subflow { graph, .. } => graph
                        .nodes
                        .iter()
                        .map(|node| (node.id.as_str(), node.kind))
                        .collect::<Vec<_>>(),
                    PackageArtifactBody::Preset { node_kind, .. } => vec![("$preset", *node_kind)],
                    _ => Vec::new(),
                };
                let base = member
                    .base
                    .as_ref()
                    .map(|version| publish::load_published(db, project, version))
                    .transpose()?
                    .flatten();
                let old = previous
                    .and_then(|saved| saved.definitions.get(&member.key))
                    .or_else(|| base.as_ref().map(|saved| &saved.definitions));
                let pinned = nodes
                    .into_iter()
                    .map(|(id, kind)| {
                        let definition = old
                            .and_then(|entries| entries.get(id))
                            .filter(|entry| entry.kind == kind)
                            .or_else(|| registry.get(&kind))
                            .context("The package contains an unknown node")?;
                        Ok((id.to_owned(), definition.clone()))
                    })
                    .collect::<Result<_>>()?;
                Ok((member.key.clone(), pinned))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            package,
            definitions,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPackageSummary {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub member_count: usize,
    pub published: bool,
}

impl Store {
    pub async fn create_workflow_package(
        &self,
        project: String,
        conversation: Option<String>,
        package: WorkflowPackage,
    ) -> Result<WorkflowPackageDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let draft = create_draft(&tx, &project, conversation, package)?;
            tx.commit()?;
            Ok(draft)
        })
        .await
    }

    pub async fn save_workflow_package(
        &self,
        project: String,
        draft: String,
        expected_revision: String,
        package: WorkflowPackage,
    ) -> Result<WorkflowPackageDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored = load_draft(&tx, &project, &draft)?;
            request::reconcile(&tx, &project, &stored)?;
            let stored = load_draft(&tx, &project, &draft)?;
            ensure!(
                stored.publication.is_none(),
                "Create a new draft to revise a published package"
            );
            if stored.revision != expected_revision {
                tx.commit()?;
                anyhow::bail!("The package changed. Review the latest revision before saving.");
            }
            let (_, previous) = read_document(&tx, &draft, &expected_revision)?;
            let document = PackageDocument::capture(&tx, &project, package, Some(&previous))?;
            if previous.package != document.package {
                persist_revision(&tx, &draft, Some(&expected_revision), None, &document)?;
            }
            let saved = load_draft(&tx, &project, &draft)?;
            tx.commit()?;
            Ok(saved)
        })
        .await
    }

    pub async fn workflow_package(
        &self,
        project: String,
        draft: String,
    ) -> Result<WorkflowPackageDraft> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = load_draft(&tx, &project, &draft)?;
            request::reconcile(&tx, &project, &current)?;
            let current = load_draft(&tx, &project, &draft)?;
            tx.commit()?;
            Ok(current)
        })
        .await
    }

    pub async fn workflow_packages(
        &self,
        project: String,
        conversation: Option<String>,
    ) -> Result<Vec<WorkflowPackageSummary>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT d.id, v.id, v.payload, EXISTS(SELECT 1 FROM workflow_package_publications p WHERE p.draft_id = d.id)
                 FROM workflow_package_drafts d
                 JOIN workflow_package_revisions v ON v.draft_id = d.id AND v.id = d.head_revision
                 WHERE d.project_id = ?1 AND (?2 IS NULL OR d.conversation_id = ?2)
                 ORDER BY d.rowid DESC",
            )?;
            query.query_map(params![project, conversation], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Vec<u8>>(2)?, row.get(3)?)))?
                .map(|row| {
                    let (id, revision, bytes, published) = row?;
                    let document: PackageDocument = open(&bytes)?;
                    Ok(WorkflowPackageSummary { id, revision, name: document.package.name, member_count: document.package.members.len(), published })
                }).collect()
        }).await
    }
}

fn create_draft(
    db: &Connection,
    project: &str,
    conversation: Option<String>,
    package: WorkflowPackage,
) -> Result<WorkflowPackageDraft> {
    if let Some(conversation) = &conversation {
        let visible = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM conversations chat JOIN checkouts checkout ON checkout.id = chat.checkout_id
                     WHERE chat.id = ?1 AND checkout.project_id = ?2 AND NOT chat.background)",
                    params![conversation, project], |row| row.get::<_, bool>(0),
                )?;
        ensure!(
            visible,
            "Open a conversation in this project to link the package"
        );
    }
    let id = new_id();
    let document = PackageDocument::capture(db, project, package, None)?;
    db.execute(
        "INSERT INTO workflow_package_drafts(id, project_id, conversation_id) VALUES (?1, ?2, ?3)",
        params![id, project, conversation],
    )?;
    persist_revision(db, &id, None, None, &document)?;
    load_draft(db, project, &id)
}

fn validate_structure(package: &WorkflowPackage) -> Result<()> {
    bounded_name(&package.name)?;
    ensure!(
        package.members.len() <= MAX_MEMBERS,
        "A package supports up to {MAX_MEMBERS} related artifacts"
    );
    ensure!(
        serde_json::to_vec(package)?.len() <= MAX_PACKAGE_BYTES,
        "The package exceeds {MAX_PACKAGE_BYTES} UTF-8 bytes"
    );
    let mut keys = BTreeSet::new();
    for member in &package.members {
        bounded_name(&member.name)?;
        bounded_name(&member.key)?;
        ensure!(
            keys.insert(&member.key),
            "Duplicate package member {}",
            member.key
        );
        ensure!(
            member.destination != PackageDestination::Library
                || member.body.kind() != PackageArtifactKind::Pipeline,
            "Publish pipelines to their project"
        );
        if let Some(base) = &member.base {
            ensure!(
                base.destination == member.destination && base.kind == member.body.kind(),
                "The base and published artifact use different destinations or types"
            );
        }
        for (slot, reference) in &member.references {
            bounded_name(slot)?;
            if let PackageReference::Parameter { name, .. } = reference {
                bounded_name(name)?;
            }
        }
    }
    Ok(())
}

fn bounded_name(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 160 && !value.chars().any(char::is_control),
        "Enter a name of at most 160 UTF-8 bytes"
    );
    Ok(())
}

fn persist_revision(
    db: &Connection,
    draft: &str,
    parent: Option<&str>,
    run: Option<&str>,
    document: &PackageDocument,
) -> Result<String> {
    let revision = insert_revision(db, draft, parent, run, document)?;
    promote_revision(db, draft, parent, &revision)?;
    Ok(revision)
}

fn insert_revision(
    db: &Connection,
    draft: &str,
    parent: Option<&str>,
    run: Option<&str>,
    document: &PackageDocument,
) -> Result<String> {
    let revision = new_id();
    db.execute(
        "INSERT INTO workflow_package_revisions VALUES (?1, ?2, ?3, ?4, ?5)",
        params![draft, revision, parent, run, seal(document)?],
    )?;
    Ok(revision)
}

fn promote_revision(
    db: &Connection,
    draft: &str,
    parent: Option<&str>,
    revision: &str,
) -> Result<()> {
    ensure!(db.execute("UPDATE workflow_package_drafts SET head_revision = ?3 WHERE id = ?1 AND head_revision IS ?2",
        params![draft, parent, revision])? == 1, "The package revision changed");
    Ok(())
}

fn read_document(
    db: &Connection,
    draft: &str,
    revision: &str,
) -> Result<(Option<String>, PackageDocument)> {
    let (parent, payload): (Option<String>, Vec<u8>) = db.query_row(
        "SELECT parent_revision, payload FROM workflow_package_revisions WHERE draft_id = ?1 AND id = ?2",
        params![draft, revision], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((parent, open(&payload)?))
}

fn load_draft(db: &Connection, project: &str, draft: &str) -> Result<WorkflowPackageDraft> {
    let (conversation_id, revision): (Option<String>, String) = db.query_row(
        "SELECT conversation_id, head_revision FROM workflow_package_drafts WHERE id = ?1 AND project_id = ?2",
        params![draft, project], |row| Ok((row.get(0)?, row.get(1)?)),
    ).context("The package draft is unavailable in this project")?;
    let (parent_revision, document) = read_document(db, draft, &revision)?;
    let publication = db.query_row(
        "SELECT payload FROM workflow_package_publications WHERE project_id = ?1 AND draft_id = ?2",
        params![project, draft], |row| row.get::<_, Vec<u8>>(0),
    ).optional()?.map(|bytes| open(&bytes)).transpose()?;
    let request = request::latest_request(db, draft)?.map(|request| request.status);
    Ok(WorkflowPackageDraft {
        id: draft.into(),
        conversation_id,
        revision,
        parent_revision,
        package: document.package,
        publication,
        request,
    })
}

fn pipeline_reference(version: &WorkflowRevision) -> PackageArtifactVersion {
    PackageArtifactVersion {
        destination: PackageDestination::Project,
        kind: PackageArtifactKind::Pipeline,
        id: version.id.clone(),
        revision: version.revision.clone(),
    }
}
