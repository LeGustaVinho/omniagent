use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    dispatch::{
        ConversationArtifactRevision, ConversationSelection, SelectedMessageSpan,
        load_conversation_artifact, record_conversation_excerpt,
    },
    protection,
    store::Store,
};

use super::{
    Conversation, MessageRevision, context_entries, load_conversation, load_messages, new_id,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReferenceSource {
    pub source: Conversation,
    pub project_id: String,
    pub project_name: String,
    pub context_id: String,
    pub retained_context_ids: Vec<String>,
    pub messages: Vec<MessageRevision>,
    pub summaries: Vec<ConversationReferenceSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReferenceSummary {
    pub artifact_id: String,
    pub context_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ConversationReferenceSelection {
    Messages {
        context_id: String,
        spans: Vec<SelectedMessageSpan>,
    },
    Summary {
        artifact_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReferenceSnapshot {
    pub id: String,
    pub destination_conversation_id: String,
    pub project_id: String,
    pub project_name: String,
    pub source_conversation_id: String,
    pub source_title: String,
    pub artifact: ConversationArtifactRevision,
}

struct ReferenceScope {
    project_id: String,
    project_name: String,
    source: Conversation,
}

impl Store {
    pub async fn conversation_reference_source(
        &self,
        destination_conversation_id: String,
        source_conversation_id: String,
        source_context_id: Option<String>,
    ) -> Result<ConversationReferenceSource> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let scope =
                reference_scope(&tx, &destination_conversation_id, &source_conversation_id)?;
            let retained_context_ids = retained_contexts(&tx, &scope.source)?;
            let context_id = source_context_id.unwrap_or_else(|| scope.source.context_id.clone());
            ensure!(
                retained_context_ids.contains(&context_id),
                "the selected context is outside the retained source conversation"
            );
            let entries = context_entries(&tx, &source_conversation_id, &context_id)?;
            let messages = load_messages(&tx, &source_conversation_id, &entries)?;
            let summaries = retained_summaries(&tx, &scope)?;
            Ok(ConversationReferenceSource {
                source: scope.source,
                project_id: scope.project_id,
                project_name: scope.project_name,
                context_id,
                retained_context_ids,
                messages,
                summaries,
            })
        })
        .await
    }

    pub async fn preview_conversation_reference(
        &self,
        destination_conversation_id: String,
        source_conversation_id: String,
        selection: ConversationReferenceSelection,
    ) -> Result<ConversationReferenceSnapshot> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let scope = reference_scope(&tx, &destination_conversation_id, &source_conversation_id)?;
            let artifact = match selection {
                ConversationReferenceSelection::Messages { context_id, spans } => {
                    ensure!(retained_contexts(&tx, &scope.source)?.contains(&context_id), "the selected context is outside the retained source conversation");
                    record_conversation_excerpt(&tx, ConversationSelection { conversation_id: source_conversation_id.clone(), context_id, spans })?
                }
                ConversationReferenceSelection::Summary { artifact_id } => {
                    let artifact = load_conversation_artifact(&tx, &source_conversation_id, &artifact_id)?
                        .context("the selected summary is not saved in the source conversation")?;
                    ensure!(matches!(artifact, ConversationArtifactRevision::Summary { .. }), "the selected revision is not a summary");
                    artifact
                }
            };
            require_artifact_scope(&artifact, &source_conversation_id, &scope.project_id)?;
            let snapshot = ConversationReferenceSnapshot {
                id: new_id(),
                destination_conversation_id,
                project_id: scope.project_id,
                project_name: scope.project_name,
                source_conversation_id,
                source_title: scope.source.title,
                artifact,
            };
            tx.execute("INSERT INTO conversation_reference_previews (id, destination_conversation_id, source_conversation_id, project_id, artifact_id, payload)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![snapshot.id, snapshot.destination_conversation_id, snapshot.source_conversation_id,
                snapshot.project_id, snapshot.artifact.id(), protection::protect(&serde_json::to_vec(&snapshot)?)?])?;
            tx.commit()?;
            Ok(snapshot)
        }).await
    }
}

pub(crate) fn load_conversation_reference(
    db: &Connection,
    destination_conversation_id: &str,
    project_id: &str,
    preview_id: &str,
) -> Result<ConversationReferenceSnapshot> {
    let (source, artifact, sealed): (String, String, Vec<u8>) = db.query_row(
        "SELECT source_conversation_id, artifact_id, payload FROM conversation_reference_previews
         WHERE id = ?1 AND destination_conversation_id = ?2 AND project_id = ?3",
        params![preview_id, destination_conversation_id, project_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?.context("the conversation reference preview is unavailable for this destination")?;
    let snapshot: ConversationReferenceSnapshot =
        serde_json::from_slice(&protection::unprotect(&sealed)?)?;
    ensure!(
        snapshot.id == preview_id
            && snapshot.destination_conversation_id == destination_conversation_id
            && snapshot.source_conversation_id == source
            && snapshot.artifact.id() == artifact,
        "the reference preview does not match its saved revision"
    );
    recheck_conversation_reference(db, destination_conversation_id, project_id, &snapshot)?;
    Ok(snapshot)
}

pub(crate) fn recheck_conversation_reference(
    db: &Connection,
    destination_conversation_id: &str,
    project_id: &str,
    snapshot: &ConversationReferenceSnapshot,
) -> Result<()> {
    ensure!(
        snapshot.project_id == project_id,
        "the conversation reference was reviewed for another project"
    );
    let check = reference_scope(
        db,
        destination_conversation_id,
        &snapshot.source_conversation_id,
    )?;
    ensure!(
        check.project_id == project_id,
        "the source or destination project changed after reference review"
    );
    require_artifact_scope(
        &snapshot.artifact,
        &snapshot.source_conversation_id,
        project_id,
    )?;
    let saved =
        load_conversation_artifact(db, &snapshot.source_conversation_id, snapshot.artifact.id())?;
    ensure!(
        saved.as_ref() == Some(&snapshot.artifact),
        "the reviewed conversation artifact is unavailable or changed"
    );
    Ok(())
}

fn reference_scope(db: &Connection, destination: &str, source: &str) -> Result<ReferenceScope> {
    let (project_id, project_root): (String, String) = db.query_row(
        "SELECT p.id, p.root FROM conversations destination
         JOIN checkouts destination_checkout ON destination_checkout.id = destination.checkout_id
         JOIN projects p ON p.id = destination_checkout.project_id
         JOIN conversations source ON source.id = ?2 AND source.background = 0
         JOIN checkouts source_checkout ON source_checkout.id = source.checkout_id AND source_checkout.project_id = p.id
         WHERE destination.id = ?1 AND destination.background = 0 AND destination.id <> source.id",
        params![destination, source], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?.context("select another visible conversation from the same project")?;
    let project_name = Path::new(&project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(project_root.as_str())
        .to_owned();
    Ok(ReferenceScope {
        project_id,
        project_name,
        source: load_conversation(db, source)?,
    })
}

fn retained_contexts(db: &Connection, source: &Conversation) -> Result<Vec<String>> {
    let mut ids = vec![source.context_id.clone()];
    let mut known = HashSet::from([source.context_id.clone()]);
    let mut statement = db.prepare(
        "SELECT id FROM (
            SELECT previous_context_id AS id, sequence AS position FROM conversation_history_journal WHERE conversation_id = ?1
            UNION ALL
            SELECT source_context_id, 0 FROM conversation_forks WHERE source_conversation_id = ?1 AND source_context_id IS NOT NULL
         ) ORDER BY position DESC, id",
    )?;
    for id in statement.query_map([&source.id], |row| row.get::<_, String>(0))? {
        let id = id?;
        if known.insert(id.clone()) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn retained_summaries(
    db: &Connection,
    scope: &ReferenceScope,
) -> Result<Vec<ConversationReferenceSummary>> {
    let mut statement = db.prepare(
        "SELECT id FROM conversation_artifact_revisions WHERE source_conversation_id = ?1 AND summary_run_id IS NOT NULL ORDER BY id",
    )?;
    let mut summaries = Vec::new();
    for id in statement.query_map([&scope.source.id], |row| row.get::<_, String>(0))? {
        let id = id?;
        let artifact = load_conversation_artifact(db, &scope.source.id, &id)?
            .context("the saved summary is unavailable")?;
        require_artifact_scope(&artifact, &scope.source.id, &scope.project_id)?;
        let ConversationArtifactRevision::Summary {
            id,
            source_context_id,
            text,
            ..
        } = artifact
        else {
            anyhow::bail!("the saved summary revision has an invalid type");
        };
        summaries.push(ConversationReferenceSummary {
            artifact_id: id,
            context_id: source_context_id,
            text,
        });
    }
    Ok(summaries)
}

fn require_artifact_scope(
    artifact: &ConversationArtifactRevision,
    source: &str,
    project: &str,
) -> Result<()> {
    let applies = |part: &crate::dispatch::ArtifactSource| {
        part.origin.conversation_id == source && part.origin.project_id == project
    };
    let valid = match artifact {
        ConversationArtifactRevision::Excerpt { excerpts, .. } => {
            !excerpts.is_empty() && excerpts.iter().all(|part| applies(&part.source))
        }
        ConversationArtifactRevision::Summary { covered, .. } => {
            !covered.is_empty() && covered.iter().all(applies)
        }
    };
    ensure!(
        valid,
        "the saved reference includes evidence outside its source conversation or project"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
