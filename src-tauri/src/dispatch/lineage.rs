use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    attachments::{AttachmentDelivery, AttachmentInput, AttachmentMetadata},
    context_records::assembly::ContextInputRevision,
    protection,
    providers::questions::QuestionOrigin,
    repository::RepositoryMarkdownRevision,
    runner::ExecutionPlatform,
    store::Store,
    tasks::DescriptionOwner,
};

use super::{ContextSnapshot, NativeHistory};

mod conversation_artifacts;
pub(super) use conversation_artifacts::pending_summary_sources;
pub(crate) use conversation_artifacts::{load_conversation_artifact, record_conversation_excerpt};
pub(crate) use conversation_artifacts::{enqueue_summary, reconcile_summary};
pub use conversation_artifacts::{
    ArtifactSource, ByteRange, ConversationArtifactRevision, ConversationSelection, ExcerptSource,
    QueuedSummary, SelectedMessageSpan, SummaryRun,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchIdentity {
    pub project_id: String,
    pub checkout_id: String,
    pub registered_root: Option<PathBuf>,
    pub registered_git_dir: Option<PathBuf>,
    pub working_directory: PathBuf,
    pub runner: String,
    pub execution_platform: Option<ExecutionPlatform>,
    pub base_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageRevisionOrigin {
    pub project_id: String,
    pub checkout_id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub revision_id: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DispatchSourceRevision {
    ConversationContext {
        context_id: String,
    },
    ProviderHistory {
        history: NativeHistory,
    },
    Question {
        origin: QuestionOrigin,
    },
    Message {
        origin: MessageRevisionOrigin,
    },
    MessageCopySource {
        copied_revision_id: String,
        origin: MessageRevisionOrigin,
    },
    SummaryInput {
        request_id: String,
        source_context_id: String,
        covered: Vec<ArtifactSource>,
    },
    CompactedConversation {
        artifact_revision_id: String,
        source_context_id: String,
        covered: Vec<ArtifactSource>,
    },
    ConversationReference {
        message_revision_id: String,
        project_id: String,
        source_conversation_id: String,
        artifact_revision_id: String,
    },
    LiveInstruction {
        origin: MessageRevisionOrigin,
    },
    Attachment {
        attachment_index: usize,
        message_revision_id: String,
        metadata: AttachmentMetadata,
        delivery: AttachmentDelivery,
    },
    RepositoryMarkdown {
        revision: RepositoryMarkdownRevision,
    },
    JobPolicy {
        revision_id: String,
    },
    RolePolicy {
        revision_id: String,
        digest: String,
    },
    RoleContext {
        revision: ContextInputRevision,
    },
    WorkDescription {
        owner: DescriptionOwner,
        revision_id: String,
        revision: i64,
    },
    ChecklistItem {
        task_id: String,
        item_id: String,
        revision: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchLineage {
    pub execution: DispatchIdentity,
    pub sources: Vec<DispatchSourceRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchUse {
    pub dispatch_id: String,
    pub conversation_id: String,
    pub run_id: String,
    pub turn_id: String,
}

pub(crate) fn record_message_copy(db: &Connection, source: &str, copied: &str) -> Result<()> {
    let inserted = db.execute(
        "INSERT INTO message_revision_copy_sources (copied_revision_id, source_revision_id)
         SELECT copied.id, source.id
         FROM message_revisions copied JOIN message_revisions source ON source.content = copied.content
         WHERE source.id = ?1 AND copied.id = ?2",
        params![source, copied],
    )?;
    ensure!(
        inserted == 1,
        "copy lineage requires the exact saved source revision"
    );
    db.execute(
        "INSERT INTO history_deleted_revisions
         SELECT ?2 FROM history_deleted_revisions WHERE revision_id = ?1",
        params![source, copied],
    )?;
    Ok(())
}

/// Runs in the transaction that prepares the exact dispatch. Persist the
/// registered checkout now because a registration can later point elsewhere.
pub(crate) fn record(
    db: &Connection,
    dispatch_id: &str,
    run_id: &str,
    snapshot: &ContextSnapshot,
) -> Result<()> {
    save(db, dispatch_id, run_id, snapshot, true)
}

pub(super) fn preserve_legacy_attachment_lineage(
    db: &Connection,
    dispatch_id: &str,
    run_id: &str,
    snapshot: &ContextSnapshot,
) -> Result<()> {
    let exists = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM dispatch_lineage WHERE dispatch_id = ?1)",
        [dispatch_id],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        save(db, dispatch_id, run_id, snapshot, false)?;
    }
    Ok(())
}

fn save(
    db: &Connection,
    dispatch_id: &str,
    run_id: &str,
    snapshot: &ContextSnapshot,
    capture_registration: bool,
) -> Result<()> {
    let (lineage, messages) = collect(db, run_id, snapshot, capture_registration)?;
    let payload = protection::protect(&serde_json::to_vec(&lineage)?)?;
    db.execute(
        "INSERT INTO dispatch_lineage (dispatch_id, payload) VALUES (?1, ?2)",
        params![dispatch_id, payload],
    )?;
    let mut insert = db.prepare_cached(
        "INSERT INTO dispatch_lineage_messages (dispatch_id, revision_id) VALUES (?1, ?2)",
    )?;
    for revision in messages {
        insert.execute(params![dispatch_id, revision])?;
    }
    Ok(())
}

impl Store {
    pub async fn dispatch_lineage(
        &self,
        conversation_id: String,
        run_id: String,
    ) -> Result<Option<DispatchLineage>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let evidence = tx
                .query_row(
                    "SELECT l.payload, CASE WHEN l.payload IS NULL THEN s.payload END
                 FROM agent_attempts a
                 JOIN context_snapshots s ON s.run_id = a.run_id
                 JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                 LEFT JOIN dispatch_lineage l ON l.dispatch_id = d.id
                 WHERE a.conversation_id = ?1 AND a.run_id = ?2",
                    params![conversation_id, run_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<Vec<u8>>>(0)?,
                            row.get::<_, Option<Vec<u8>>>(1)?,
                        ))
                    },
                )
                .optional()?;
            let Some((recorded, snapshot)) = evidence else {
                return Ok(None);
            };
            let mut lineage = if let Some(recorded) = recorded {
                decode(&recorded)?
            } else {
                let snapshot: ContextSnapshot =
                    decode(&snapshot.context("dispatch has no saved snapshot")?)?;
                collect(&tx, &run_id, &snapshot, false)?.0
            };
            append_live_instructions(&tx, &run_id, &mut lineage)?;
            Ok(Some(lineage))
        })
        .await
    }

    /// Find derivatives of a selected local message revision. The source
    /// conversation is checked before any cross-conversation usage is returned.
    pub async fn dispatches_using_message(
        &self,
        source_conversation_id: String,
        revision_id: String,
    ) -> Result<Vec<DispatchUse>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            if !message_origin(&tx, &revision_id)?
                .is_some_and(|origin| origin.conversation_id == source_conversation_id)
            {
                return Ok(Vec::new());
            }
            let derived_revisions = derived_message_revisions(&tx, &revision_id)?;
            let mut query = tx.prepare(
                "WITH known AS (
                    SELECT m.dispatch_id FROM dispatch_lineage_messages m
                    WHERE m.revision_id IN (SELECT value FROM json_each(?1))
                    UNION
                    SELECT d.id FROM instruction_delivery_attempts i
                    JOIN context_snapshots s ON s.run_id = i.run_id
                    JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                    WHERE i.revision_id IN (SELECT value FROM json_each(?1))
                        AND i.route = 'steering' AND i.prepared_event_seq IS NOT NULL
                )
                SELECT d.id, a.conversation_id, a.run_id, d.turn_id,
                       CASE WHEN k.dispatch_id IS NULL THEN s.payload END
                FROM dispatches d
                JOIN context_snapshots s ON s.id = d.snapshot_id
                JOIN agent_attempts a ON a.run_id = s.run_id
                LEFT JOIN known k ON k.dispatch_id = d.id
                LEFT JOIN dispatch_lineage l ON l.dispatch_id = d.id
                WHERE d.ordinal = 1 AND (k.dispatch_id IS NOT NULL OR l.dispatch_id IS NULL)
                ORDER BY a.conversation_id, a.ordinal",
            )?;
            let mut rows = query.query([serde_json::to_string(&derived_revisions)?])?;
            let mut uses = Vec::new();
            while let Some(row) = rows.next()? {
                if let Some(payload) = row.get::<_, Option<Vec<u8>>>(4)? {
                    let snapshot: ContextSnapshot = decode(&payload)?;
                    if !snapshot
                        .submitted_messages
                        .iter()
                        .any(|message| derived_revisions.contains(&message.revision_id))
                        && !snapshot.attachments.iter().any(|attachment| {
                            derived_revisions.contains(&attachment.message_revision_id)
                        })
                    {
                        continue;
                    }
                }
                uses.push(DispatchUse {
                    dispatch_id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    run_id: row.get(2)?,
                    turn_id: row.get(3)?,
                });
            }
            Ok(uses)
        })
        .await
    }
}

fn collect(
    db: &Connection,
    run_id: &str,
    snapshot: &ContextSnapshot,
    capture_registration: bool,
) -> Result<(DispatchLineage, HashSet<String>)> {
    let execution = db.query_row(
        "SELECT c.project_id, c.id, c.root, c.git_dir
         FROM runs r JOIN checkouts c ON c.id = r.checkout_id WHERE r.id = ?1",
        [run_id],
        |row| {
            Ok(DispatchIdentity {
                project_id: row.get(0)?,
                checkout_id: row.get(1)?,
                registered_root: if capture_registration {
                    Some(PathBuf::from(row.get::<_, String>(2)?))
                } else {
                    None
                },
                registered_git_dir: if capture_registration {
                    Some(PathBuf::from(row.get::<_, String>(3)?))
                } else {
                    None
                },
                working_directory: snapshot.working_directory().to_path_buf(),
                runner: snapshot.scope.runner.clone(),
                execution_platform: snapshot
                    .capabilities
                    .as_ref()
                    .and_then(|assessment| assessment.scope.platform.clone()),
                base_commit: snapshot.base_commit.clone(),
            })
        },
    )?;

    let mut sources = vec![DispatchSourceRevision::ConversationContext {
        context_id: snapshot.context_id.clone(),
    }];
    if snapshot.native_history != NativeHistory::Fresh {
        sources.push(DispatchSourceRevision::ProviderHistory {
            history: snapshot.native_history.clone(),
        });
    }
    if let Some(origin) = &snapshot.answered_question {
        sources.push(DispatchSourceRevision::Question {
            origin: origin.clone(),
        });
    }
    if let Some(revision_id) = &snapshot.job_policy_revision {
        sources.push(DispatchSourceRevision::JobPolicy {
            revision_id: revision_id.clone(),
        });
    }

    let mut messages = HashSet::new();
    for revision_id in snapshot
        .submitted_messages
        .iter()
        .map(|message| &message.revision_id)
        .chain(
            snapshot
                .attachments
                .iter()
                .map(|attachment| &attachment.message_revision_id),
        )
    {
        if messages.contains(revision_id) {
            continue;
        }
        if let Some(origin) = message_origin(db, revision_id)? {
            messages.insert(revision_id.clone());
            sources.push(DispatchSourceRevision::Message { origin });
            append_message_copy_sources(db, revision_id, &mut sources)?;
        }
    }
    for (attachment_index, attachment) in snapshot.attachments.iter().enumerate() {
        let delivery = match &attachment.input {
            AttachmentInput::Native(_) => AttachmentDelivery::Native,
            AttachmentInput::Reference(_) => AttachmentDelivery::Reference,
            AttachmentInput::Excerpt(excerpt) => AttachmentDelivery::Excerpt {
                start: excerpt.start,
                end: excerpt.end,
            },
        };
        sources.push(DispatchSourceRevision::Attachment {
            attachment_index,
            message_revision_id: attachment.message_revision_id.clone(),
            metadata: attachment.metadata.clone(),
            delivery,
        });
    }
    if let Some((request_id, request)) =
        conversation_artifacts::summary_input_at_dispatch(db, run_id, snapshot)?
    {
        messages.extend(
            request
                .covered
                .iter()
                .map(|source| source.origin.revision_id.clone()),
        );
        sources.push(DispatchSourceRevision::SummaryInput {
            request_id,
            source_context_id: request.source_context_id,
            covered: request.covered,
        });
    }
    if let Some(overlay) = crate::conversations::compaction::load_overlay(db, &snapshot.context_id)?
        && snapshot.submitted_messages.first().is_some_and(|entry| entry == &overlay.seed())
        && let ConversationArtifactRevision::Summary { id, source_context_id, covered, .. } = overlay.artifact
    {
        messages.extend(covered.iter().map(|part| part.origin.revision_id.clone()));
        sources.push(DispatchSourceRevision::CompactedConversation {
            artifact_revision_id: id, source_context_id, covered,
        });
    }
    for submitted in &snapshot.conversation_references {
        let reference = &submitted.reference;
        messages.extend(crate::conversations::reference_delivery::source_revisions(&reference.artifact)
            .into_iter().map(str::to_owned));
        sources.push(DispatchSourceRevision::ConversationReference {
            message_revision_id: submitted.message_revision_id.clone(),
            project_id: reference.project_id.clone(),
            source_conversation_id: reference.source_conversation_id.clone(),
            artifact_revision_id: reference.artifact.id().into(),
        });
    }
    if let Some(markdown) = &snapshot.repository_markdown {
        sources.push(DispatchSourceRevision::RepositoryMarkdown {
            revision: markdown.revision.clone(),
        });
    }
    if let Some(context) = &snapshot.role_context {
        sources.push(DispatchSourceRevision::RolePolicy {
            revision_id: context.policy_revision_id.clone(),
            digest: context.policy_digest.clone(),
        });
        sources.extend(context.selection.blocks.iter().map(|block| {
            DispatchSourceRevision::RoleContext {
                revision: block.revision.clone(),
            }
        }));
    }
    if let Some(work) = &snapshot.task_context {
        for revision in [
            &work.descriptions.epic,
            &work.descriptions.task,
            &work.descriptions.subtask,
        ]
        .into_iter()
        .flatten()
        {
            sources.push(DispatchSourceRevision::WorkDescription {
                owner: revision.owner.clone(),
                revision_id: revision.id.clone(),
                revision: revision.revision,
            });
        }
        sources.extend(
            work.checklist
                .iter()
                .map(|item| DispatchSourceRevision::ChecklistItem {
                    task_id: work.task_id.clone(),
                    item_id: item.id.clone(),
                    revision: item.revision,
                }),
        );
    }
    Ok((DispatchLineage { execution, sources }, messages))
}

fn message_origin(db: &Connection, revision_id: &str) -> Result<Option<MessageRevisionOrigin>> {
    Ok(db
        .query_row(
            "SELECT c.project_id, c.id, m.conversation_id, m.id, v.id, v.revision
         FROM message_revisions v JOIN messages m ON m.id = v.message_id
         JOIN conversations a ON a.id = m.conversation_id JOIN checkouts c ON c.id = a.checkout_id
         WHERE v.id = ?1",
            [revision_id],
            |row| {
                Ok(MessageRevisionOrigin {
                    project_id: row.get(0)?,
                    checkout_id: row.get(1)?,
                    conversation_id: row.get(2)?,
                    message_id: row.get(3)?,
                    revision_id: row.get(4)?,
                    revision: row.get(5)?,
                })
            },
        )
        .optional()?)
}

fn derived_message_revisions(db: &Connection, revision: &str) -> Result<HashSet<String>> {
    let mut query = db.prepare(
        "WITH RECURSIVE derived(id) AS (
            VALUES (?1)
            UNION
            SELECT copy.copied_revision_id
            FROM message_revision_copy_sources copy JOIN derived ON copy.source_revision_id = derived.id
         ) SELECT id FROM derived",
    )?;
    Ok(query
        .query_map([revision], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn append_message_copy_sources(
    db: &Connection,
    revision: &str,
    sources: &mut Vec<DispatchSourceRevision>,
) -> Result<()> {
    let mut query = db.prepare_cached(
        "SELECT source_revision_id FROM message_revision_copy_sources WHERE copied_revision_id = ?1",
    )?;
    let mut copied = revision.to_owned();
    let mut visited = HashSet::new();
    while let Some(source) = query
        .query_row([&copied], |row| row.get::<_, String>(0))
        .optional()?
    {
        ensure!(
            visited.insert(source.clone()),
            "cyclic message copy lineage"
        );
        if let Some(origin) = message_origin(db, &source)? {
            sources.push(DispatchSourceRevision::MessageCopySource {
                copied_revision_id: copied,
                origin,
            });
        }
        copied = source;
    }
    Ok(())
}

fn append_live_instructions(
    db: &Connection,
    run_id: &str,
    lineage: &mut DispatchLineage,
) -> Result<()> {
    let mut query = db.prepare(
        "SELECT revision_id FROM instruction_delivery_attempts
         WHERE run_id = ?1 AND route = 'steering' AND prepared_event_seq IS NOT NULL
         ORDER BY prepared_event_seq",
    )?;
    for revision in query.query_map([run_id], |row| row.get::<_, String>(0))? {
        let revision = revision?;
        if let Some(origin) = message_origin(db, &revision)? {
            lineage
                .sources
                .push(DispatchSourceRevision::LiveInstruction { origin });
            append_message_copy_sources(db, &revision, &mut lineage.sources)?;
        }
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use anyhow::ensure;

    use crate::{
        attachments::{
            AttachmentDispatch, AttachmentPreview, AttachmentReference, AttachmentSource,
            CapturedPayload,
        },
        conversations::SessionScope,
        dispatch::{Delivery, EventSource},
        providers::{
            Provider, ProviderEvent, ProviderEventData, ProviderLocation, SessionMode,
            questions::QuestionRoute,
        },
        repository,
        runs::RunStatus,
    };

    use super::*;

    #[tokio::test]
    async fn committed_lineage_keeps_the_registered_root_attachment_revision_and_source_ownership()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("lineage.db");
        let store = Store::open(database)?;
        let root = dunce::canonicalize(directory.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()?
                .success()
        );
        let checkout = repository::register(&store, root.clone()).await?;
        let conversation = store
            .create_conversation(checkout.checkout_id.clone(), "Source".into())
            .await?;
        let wrong_conversation = store
            .create_conversation(checkout.checkout_id.clone(), "Other".into())
            .await?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: "lineage-profile".into(),
            account_binding: None,
            checkout: root.clone(),
            executable: root.join("codex.exe"),
            cli_version: "1.0.0".into(),
            runner: "fixture-runner".into(),
            permission_revision: "fixture-permissions".into(),
            mode: SessionMode::ReadOnly,
            model: None,
            effort: None,
        };

        fs::write(root.join("source.md"), b"protected-original\r\n")?;
        let markdown = store
            .capture_repository_markdown(checkout.checkout_id.clone(), root.join("source.md"))
            .await?;
        let previews = [
            AttachmentDelivery::Excerpt { start: 0, end: 9 },
            AttachmentDelivery::Reference,
        ]
        .map(|delivery| {
            AttachmentPreview::read_with_delivery(
                &root,
                AttachmentSource::Checkout {
                    relative_path: "source.md".into(),
                },
                delivery,
            )
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        let attempt = store
            .prepare_agent_attempt_with_attachments(
                conversation.id.clone(),
                conversation.context_id.clone(),
                scope.clone(),
                "private original instruction".into(),
                previews,
            )
            .await?;
        let (source_revision, captured) = &attempt.attachments[0];
        let CapturedPayload::Excerpt { excerpt } = &captured.payload else {
            anyhow::bail!("expected captured excerpt")
        };
        let source_revision = source_revision.clone();
        let sha256 = captured.metadata.sha256.clone();

        let snapshot = ContextSnapshot {
            version: 13,
            context_id: attempt.context_id.clone(),
            scope: scope.clone(),
            session_id: None,
            job_policy_revision: None,
            role_context: None,
            base_commit: None,
            submitted_messages: attempt.input.clone(),
            conversation_references: Vec::new(),
            native_history: NativeHistory::Fresh,
            capabilities: None,
            filesystem_skills: Vec::new(),
            reported_skills: None,
            instructions: None,
            scoped_launch: None,
            requested_skill: None,
            plugin_inventory: None,
            attachments: vec![
                AttachmentDispatch {
                    message_revision_id: source_revision.clone(),
                    metadata: captured.metadata.clone(),
                    qualification_id: None,
                    input: AttachmentInput::Excerpt(excerpt.clone()),
                },
                AttachmentDispatch {
                    message_revision_id: source_revision.clone(),
                    metadata: attempt.attachments[1].1.metadata.clone(),
                    qualification_id: None,
                    input: AttachmentInput::Reference(AttachmentReference {
                        name: "source.md".to_owned(),
                        path: root.join("source.md"),
                        uri: None,
                        format: captured.metadata.format,
                        size: captured.metadata.size,
                    }),
                },
            ],
            task_context: None,
            repository_markdown: Some(markdown.clone()),
            question_route: QuestionRoute::Native,
            answered_question: None,
        };
        store.mark_agent_running(attempt.run.id.clone()).await?;
        let read_event = |path: &str| ProviderEventData::ContextRead {
            path: path.to_owned(),
            tool: "fixture read".to_owned(),
        };
        let dispatch_id = store
            .prepare_dispatch(
                attempt.run.id.clone(),
                snapshot,
                vec![b"fixture wire".to_vec()],
            )
            .await?;
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: read_event("source.md"),
                },
            )
            .await?;
        store
            .settle_dispatch(dispatch_id.clone(), Delivery::Submitted)
            .await?;
        store
            .record_agent_observation(
                attempt.run.id.clone(),
                EventSource::Application,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data: ProviderEventData::DispatchSubmitted {
                        dispatch_id: dispatch_id.clone(),
                    },
                },
            )
            .await?;
        for (source, parent, path) in [
            (EventSource::Application, None, "source.md"),
            (EventSource::Provider, Some("child-tool"), "source.md"),
            (EventSource::Provider, None, "../source.md"),
            (EventSource::Provider, None, "renamed.md"),
            (EventSource::Provider, None, "source.md"),
        ] {
            store
                .record_agent_observation(
                    attempt.run.id.clone(),
                    source,
                    ProviderEvent {
                        location: ProviderLocation {
                            parent_item_id: parent.map(str::to_owned),
                            ..Default::default()
                        },
                        data: read_event(path),
                    },
                )
                .await?;
        }
        store
            .finish_agent_attempt(attempt.run.id.clone(), RunStatus::Succeeded, None)
            .await?;

        let source_context = store
            .conversation(conversation.id.clone())
            .await?
            .context_id;
        let fork = store
            .fork_conversation(
                conversation.id.clone(),
                source_context,
                Some(source_revision.clone()),
                "Fork".into(),
            )
            .await?;
        let fork_attempt = store
            .prepare_agent_attempt(
                fork.id.clone(),
                fork.context_id,
                scope,
                "Follow the fork".into(),
            )
            .await?;
        let copied_revision = fork_attempt.input[0].revision_id.clone();
        assert_ne!(copied_revision, source_revision);
        let mut fork_snapshot = store
            .agent_dispatch(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .context("missing source dispatch")?
            .snapshot;
        fork_snapshot
            .context_id
            .clone_from(&fork_attempt.context_id);
        fork_snapshot
            .submitted_messages
            .clone_from(&fork_attempt.input);
        fork_snapshot.repository_markdown = None;
        for attachment in &mut fork_snapshot.attachments {
            attachment.message_revision_id.clone_from(&copied_revision);
        }
        store
            .mark_agent_running(fork_attempt.run.id.clone())
            .await?;
        let fork_dispatch = store
            .prepare_dispatch(
                fork_attempt.run.id.clone(),
                fork_snapshot,
                vec![b"fixture fork wire".to_vec()],
            )
            .await?;
        store
            .settle_dispatch(fork_dispatch.clone(), Delivery::Submitted)
            .await?;

        fs::rename(root.join("source.md"), root.join("moved.md"))?;
        fs::write(root.join("moved.md"), b"different current source")?;
        store
            .execute({
                let id = checkout.checkout_id.clone();
                move |db| {
                    db.execute(
                        "UPDATE checkouts SET root = 'changed registered root' WHERE id = ?1",
                        [id],
                    )?;
                    Ok(())
                }
            })
            .await?;
        let backup = root.join("backup.sqlite");
        store
            .execute({
                let backup = backup.to_string_lossy().into_owned();
                move |db| {
                    db.execute("VACUUM INTO ?1", [backup])?;
                    Ok(())
                }
            })
            .await?;
        drop(store);

        let store = Store::open(backup)?;
        let restored = store
            .agent_dispatch(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .context("backup omitted protected attachments")?;
        assert!(restored.attachments[0].reported_reads.is_empty());
        let reports = &restored.attachments[1].reported_reads;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].path, "source.md");
        assert_eq!(reports[0].tool, "fixture read");
        let evidence = store
            .dispatch_lineage(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .context("missing lineage")?;
        assert_eq!(evidence.execution.checkout_id, checkout.checkout_id);
        assert_eq!(
            evidence.execution.registered_root.as_deref(),
            Some(root.as_path())
        );
        assert_eq!(evidence.execution.runner, "fixture-runner");
        assert!(evidence.sources.iter().any(|source| matches!(source,
            DispatchSourceRevision::Message { origin } if origin.revision_id == source_revision
                && origin.project_id == checkout.project_id && origin.conversation_id == conversation.id)));
        assert!(evidence.sources.iter().any(|source| matches!(source,
            DispatchSourceRevision::Attachment { attachment_index: 0, message_revision_id, metadata, delivery: AttachmentDelivery::Excerpt { start: 0, end: 9 } }
                if message_revision_id == &source_revision && metadata.sha256 == sha256)));
        assert!(
            evidence
                .sources
                .contains(&DispatchSourceRevision::RepositoryMarkdown {
                    revision: markdown.revision
                })
        );
        assert!(
            store
                .dispatch_lineage(wrong_conversation.id.clone(), attempt.run.id.clone())
                .await?
                .is_none()
        );
        assert!(
            store
                .dispatches_using_message(wrong_conversation.id, source_revision.clone())
                .await?
                .is_empty()
        );
        let fork_evidence = store
            .dispatch_lineage(fork.id.clone(), fork_attempt.run.id.clone())
            .await?
            .context("missing fork lineage")?;
        assert!(fork_evidence.sources.iter().any(|source| matches!(source,
            DispatchSourceRevision::MessageCopySource { copied_revision_id, origin }
            if copied_revision_id == &copied_revision && origin.revision_id == source_revision)));
        let uses = store
            .dispatches_using_message(conversation.id.clone(), source_revision.clone())
            .await?;
        assert_eq!(uses.len(), 2);
        assert!(uses.contains(&DispatchUse {
            dispatch_id: dispatch_id.clone(),
            conversation_id: conversation.id.clone(),
            run_id: attempt.run.id.clone(),
            turn_id: attempt.run.turn_id,
        }));
        assert!(uses.contains(&DispatchUse {
            dispatch_id: fork_dispatch.clone(),
            conversation_id: fork.id,
            run_id: fork_attempt.run.id,
            turn_id: fork_attempt.run.turn_id,
        }));
        store.execute({
            let dispatch_id = dispatch_id.clone();
            let source_revision = source_revision.clone();
            let sha256 = sha256.clone();
            move |db| {
                let payload: Vec<u8> = db.query_row("SELECT payload FROM dispatch_lineage WHERE dispatch_id = ?1", [&dispatch_id], |row| row.get(0))?;
                assert!(!payload.windows(sha256.len()).any(|window| window == sha256.as_bytes()));
                let indexed_ids: Vec<String> = db.prepare("SELECT revision_id FROM dispatch_lineage_messages WHERE dispatch_id = ?1")?
                    .query_map([&dispatch_id], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
                assert_eq!(indexed_ids, [source_revision]);
                // Simulate a pre-lineage dispatch to cover the historical fallback.
                db.execute("DELETE FROM dispatch_lineage WHERE dispatch_id = ?1", [&dispatch_id])?;
                Ok(())
            }
        }).await?;
        assert_eq!(
            store
                .dispatches_using_message(conversation.id.clone(), source_revision.clone())
                .await?
                .len(),
            2
        );
        let legacy = store
            .dispatch_lineage(conversation.id.clone(), attempt.run.id.clone())
            .await?
            .unwrap();
        assert_eq!(legacy.execution.registered_root, None);
        assert_eq!(legacy.sources, evidence.sources);
        store
            .execute(move |db| {
                db.execute("DELETE FROM dispatches WHERE id = ?1", [&fork_dispatch])?;
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM dispatch_lineage WHERE dispatch_id = ?1",
                        [&fork_dispatch],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    db.query_row(
                        "SELECT COUNT(*) FROM dispatch_lineage_messages WHERE dispatch_id = ?1",
                        [&fork_dispatch],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await?;
        assert_eq!(
            store
                .dispatches_using_message(conversation.id, source_revision)
                .await?
                .len(),
            1
        );
        Ok(())
    }
}
