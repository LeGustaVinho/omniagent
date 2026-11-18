use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    protection,
    store::Store,
    workflows::{WorkflowRevision, load_definition},
};

use super::{
    Conversation, MessageRole, activate_context, context_entries, insert_conversation,
    insert_message, load_conversation,
};

#[cfg(test)]
pub(super) mod tests;

#[derive(Debug, Clone, Default, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ConversationPipelineChoice {
    #[default]
    Direct,
    RepositoryDelivery,
    Published {
        #[ts(type = "{ id: string, revision: string }")]
        workflow: WorkflowRevision,
    },
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Deserialize, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum ConversationPipeline {
    #[default]
    Direct,
    RepositoryDelivery {
        #[ts(type = "{ id: string, revision: string }")]
        workflow: WorkflowRevision,
    },
    Published {
        #[ts(type = "{ id: string, revision: string }")]
        workflow: WorkflowRevision,
        name: String,
    },
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/")]
pub struct ConversationPipelineOption {
    pub name: String,
    pub pipeline: ConversationPipelineChoice,
}

pub(crate) fn initialize_pipeline_schema(db: &mut Connection) -> Result<()> {
    // The optional conversation schema can coexist with other pipeline migrations
    // without consuming the next shared user_version on parallel development builds.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_pipeline = tx
        .prepare("SELECT 1 FROM pragma_table_info('conversations') WHERE name = 'pipeline'")?
        .exists([])?;
    if !has_pipeline {
        tx.execute_batch(
            "ALTER TABLE conversations ADD COLUMN pipeline TEXT
             CHECK (pipeline IS NULL OR json_valid(pipeline));",
        )?;
    }
    tx.execute_batch(include_str!("../../migrations/conversation-pipelines.sql"))?;
    tx.commit()?;
    Ok(())
}

fn checkout_project(db: &Connection, checkout_id: &str) -> Result<String> {
    db.query_row(
        "SELECT project_id FROM checkouts WHERE id = ?1",
        [checkout_id],
        |row| row.get(0),
    )
    .context("The conversation worktree is unavailable")
}

pub(super) fn pin_direct_chat(db: &Connection, conversation: &Conversation) -> Result<()> {
    let project = checkout_project(db, &conversation.checkout_id)?;
    crate::workflows::conversation_turns::pin_direct_conversation(db, &project, &conversation.id)?;
    Ok(())
}

fn published_workflow_name(
    db: &Connection,
    project: &str,
    workflow: &WorkflowRevision,
) -> Result<String> {
    let was_published: bool = db.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM workflow_published_heads WHERE project_id = ?1 AND workflow_id = ?2 AND revision = ?3
             UNION ALL
             SELECT 1 FROM workflow_authoring_publications WHERE project_id = ?1 AND workflow_id = ?2 AND workflow_revision = ?3
             UNION ALL
             SELECT 1 FROM workflow_package_artifact_revisions
              WHERE scope_key = 'project:' || ?1 AND kind = 'pipeline' AND artifact_id = ?2 AND revision = ?3
         )",
        params![project, workflow.id, workflow.revision],
        |row| row.get(0),
    )?;
    ensure!(
        was_published,
        "The selected pipeline revision is not published in this project"
    );
    let definition = load_definition(db, project, workflow)?
        .context("The selected pipeline revision is unavailable")?;
    definition.ensure_supported_nodes()?;
    if definition.nodes.iter().any(|node| {
        node.instance.kind == crate::workflows::BuiltinNode::End && !node.instance.id.contains("::")
    }) {
        ensure!(
            crate::workflows::conversation_turns::is_conversation_template(&definition),
            "This template needs conversation inputs at Start and a required text reply at End"
        );
    }
    Ok(definition.name)
}

impl Store {
    pub async fn conversation_pipeline_options(
        &self,
        checkout_id: String,
    ) -> Result<Vec<ConversationPipelineOption>> {
        self.execute(move |db| {
            let project = checkout_project(db, &checkout_id)?;
            let mut heads = db.prepare(
                "SELECT workflow_id, revision FROM workflow_published_heads
                 WHERE project_id = ?1 ORDER BY workflow_id",
            )?;
            let mut published = heads
                .query_map([&project], |row| {
                    Ok(WorkflowRevision {
                        id: row.get(0)?,
                        revision: row.get(1)?,
                    })
                })?
                .map(|workflow| {
                    let workflow = workflow?;
                    let definition = load_definition(db, &project, &workflow)?
                        .context("The published workflow revision is unavailable")?;
                    Ok(
                        crate::workflows::conversation_turns::is_conversation_template(&definition)
                            .then_some(ConversationPipelineOption {
                                name: definition.name,
                                pipeline: ConversationPipelineChoice::Published { workflow },
                            }),
                    )
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            published.sort_by_cached_key(|item| item.name.to_lowercase());
            let mut result = vec![ConversationPipelineOption {
                name: "Direct conversation".into(),
                pipeline: ConversationPipelineChoice::Direct,
            }];
            result.extend(published);
            Ok(result)
        })
        .await
    }

    pub async fn create_conversation_with_pipeline(
        &self,
        checkout_id: String,
        title: String,
        pipeline: ConversationPipelineChoice,
    ) -> Result<Conversation> {
        ensure!(!title.trim().is_empty(), "conversation title is empty");
        let (project, pipeline) = self
            .resolve_conversation_pipeline(&checkout_id, pipeline)
            .await?;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation =
                insert_conversation_with_pipeline(&tx, checkout_id, &project, title, pipeline)?;
            tx.commit()?;
            Ok(conversation)
        })
        .await
    }

    pub(super) async fn resolve_conversation_pipeline(
        &self,
        checkout_id: &str,
        pipeline: ConversationPipelineChoice,
    ) -> Result<(String, ConversationPipeline)> {
        let selected_checkout = checkout_id.to_owned();
        let project = self
            .execute(move |db| checkout_project(db, &selected_checkout))
            .await?;
        let workflow = match pipeline {
            ConversationPipelineChoice::RepositoryDelivery => {
                anyhow::bail!("Repository delivery has been removed");
            }
            ConversationPipelineChoice::Published { workflow } => {
                let exact = workflow.clone();
                let scoped_project = project.clone();
                let name = self
                    .execute(move |db| published_workflow_name(db, &scoped_project, &exact))
                    .await?;
                ConversationPipeline::Published { workflow, name }
            }
            ConversationPipelineChoice::Direct => ConversationPipeline::Direct,
        };
        Ok((project, workflow))
    }
}

pub(super) fn insert_conversation_with_pipeline(
    db: &Connection,
    checkout_id: String,
    project: &str,
    title: String,
    pipeline: ConversationPipeline,
) -> Result<Conversation> {
    ensure!(
        checkout_project(db, &checkout_id)? == project,
        "The worktree project changed"
    );
    let mut conversation = insert_conversation(db, checkout_id, title, false)?;
    save_conversation_pipeline(db, &mut conversation, pipeline)?;
    Ok(conversation)
}

pub(super) fn read_optional_pipeline(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<ConversationPipeline>> {
    row.get::<_, Option<String>>(index)?
        .map(|json| serde_json::from_str::<ConversationPipeline>(&json))
        .transpose()
        .map(|pipeline| match pipeline {
            Some(ConversationPipeline::RepositoryDelivery { .. }) => {
                Some(ConversationPipeline::Direct)
            }
            pipeline => pipeline,
        })
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn save_conversation_pipeline(
    db: &Connection,
    conversation: &mut Conversation,
    pipeline: ConversationPipeline,
) -> Result<()> {
    if pipeline == ConversationPipeline::Direct {
        pin_direct_chat(db, conversation)?;
    }
    let payload = serde_json::to_string(&pipeline)?;
    db.execute(
        "UPDATE conversations SET pipeline = ?1 WHERE id = ?2",
        params![payload, conversation.id],
    )?;
    conversation.pipeline = Some(pipeline);
    Ok(())
}

pub(super) fn inherit_pipeline(
    db: &Connection,
    source: &Conversation,
    target: &mut Conversation,
) -> Result<()> {
    if let Some(pipeline) = source.pipeline.as_ref() {
        ensure!(
            checkout_project(db, &source.checkout_id)?
                == checkout_project(db, &target.checkout_id)?,
            "A conversation pipeline can only be forked within the same project"
        );
        save_conversation_pipeline(db, target, pipeline.clone())?;
    } else {
        pin_direct_chat(db, target)?;
    }
    db.execute(
        "UPDATE conversation_direct_pipeline_pins AS target
         SET (workflow_id, revision) = (SELECT workflow_id, revision FROM conversation_direct_pipeline_pins WHERE conversation_id = ?2)
         WHERE target.conversation_id = ?1 AND EXISTS (SELECT 1 FROM conversation_direct_pipeline_pins source
             WHERE source.conversation_id = ?2 AND source.project_id = target.project_id)",
        params![target.id, source.id],
    )?;
    Ok(())
}

pub(crate) fn load_conversation_pipeline(
    db: &Connection,
    conversation_id: &str,
) -> Result<ConversationPipeline> {
    Ok(load_conversation(db, conversation_id)?
        .pipeline
        .unwrap_or_default())
}

pub(crate) fn append_pipeline_message(
    db: &Connection,
    conversation_id: &str,
    role: MessageRole,
    text: &str,
) -> Result<(String, String)> {
    let conversation = load_conversation(db, conversation_id)?;
    let mut entries = context_entries(db, conversation_id, &conversation.context_id)?;
    let revision = insert_message(
        db,
        conversation_id,
        role,
        protection::protect(text.as_bytes())?,
    )?;
    entries.push(revision.clone());
    let context = activate_context(db, conversation_id, &entries)?;
    Ok((revision, context))
}
