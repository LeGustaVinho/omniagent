use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::{
        ConversationWorktreePreview, ProjectCheckout, inspect_fork_checkout,
        materialize_fork_checkout,
    },
    store::Store,
};

use super::{
    ContinuationLevel, Conversation, check_idle_base,
    continuation::fork_history_at_checkout,
    history::{edit_fork_message, require_user_revision, retained_prefix},
    load_conversation, new_id,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkCheckoutPolicy {
    #[default]
    ReuseCurrentCheckout,
    CreateSeparateWorktree,
    AskForCheckout,
}

impl ForkCheckoutPolicy {
    fn key(self) -> &'static str {
        match self {
            Self::ReuseCurrentCheckout => "reuse_current_checkout",
            Self::CreateSeparateWorktree => "create_separate_worktree",
            Self::AskForCheckout => "ask_for_checkout",
        }
    }

    fn parse(key: &str) -> Result<Self> {
        match key {
            "reuse_current_checkout" => Ok(Self::ReuseCurrentCheckout),
            "create_separate_worktree" => Ok(Self::CreateSeparateWorktree),
            "ask_for_checkout" => Ok(Self::AskForCheckout),
            _ => anyhow::bail!("unknown fork checkout policy"),
        }
    }

    fn choice(self) -> Option<ForkCheckoutChoice> {
        match self {
            Self::ReuseCurrentCheckout => Some(ForkCheckoutChoice::ReuseCurrentCheckout),
            Self::CreateSeparateWorktree => Some(ForkCheckoutChoice::CreateSeparateWorktree),
            Self::AskForCheckout => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkCheckoutChoice {
    ReuseCurrentCheckout,
    CreateSeparateWorktree,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkCheckoutSettings {
    pub project_default: ForkCheckoutPolicy,
    pub conversation_override: Option<ForkCheckoutPolicy>,
    pub effective: ForkCheckoutPolicy,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkCheckoutPreview {
    pub preview_id: Option<String>,
    pub settings: ForkCheckoutSettings,
    pub choice: Option<ForkCheckoutChoice>,
    pub project_id: String,
    pub repository_root: String,
    pub checkout_id: String,
    pub checkout_root: PathBuf,
    pub base_commit: Option<String>,
    pub dirty_paths: Vec<String>,
    pub selected_paths: Vec<String>,
    pub target_checkout_root: Option<PathBuf>,
    pub target_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkConversationRequest {
    pub conversation_id: String,
    pub expected_context_id: String,
    pub source_context_id: Option<String>,
    pub through_revision: Option<String>,
    pub title: String,
    pub edited_text: Option<String>,
    pub checkout_preview_id: Option<String>,
}

pub(super) struct ForkTarget {
    pub checkout_id: String,
    preview_id: Option<String>,
}

pub(super) struct ContinuationCheckout {
    pub checkout_id: String,
    pub project_id: String,
    pub root: PathBuf,
}

impl Store {
    pub async fn conversation_continuation_checkouts(
        &self,
        conversation_id: String,
    ) -> Result<Vec<ProjectCheckout>> {
        self.execute(move |db| {
            let source = load_conversation(db, &conversation_id)?;
            let (project, _) = checkout_identity(db, &source.checkout_id)?;
            let mut statement = db.prepare(
                "SELECT checkout.id, checkout.root, project.root FROM checkouts checkout
                JOIN projects project ON project.id = checkout.project_id WHERE project.id = ?1
                ORDER BY (checkout.id <> ?2), checkout.root, checkout.id",
            )?;
            let rows = statement.query_map(params![project, source.checkout_id], |row| {
                Ok(ProjectCheckout {
                    project_id: project.clone(),
                    checkout_id: row.get(0)?,
                    checkout_root: row.get(1)?,
                    repository_root: row.get(2)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn conversation_continuation_checkout_id(
        &self,
        conversation_id: String,
        requested_checkout_id: Option<String>,
    ) -> Result<String> {
        self.execute(move |db| {
            let source = load_conversation(db, &conversation_id)?;
            Ok(
                resolve_continuation_checkout(db, &source, requested_checkout_id.as_deref())?
                    .checkout_id,
            )
        })
        .await
    }
    pub async fn conversation_fork_checkout_settings(
        &self,
        conversation_id: String,
    ) -> Result<ForkCheckoutSettings> {
        self.execute(move |db| {
            fork_checkout_settings(db, &load_conversation(db, &conversation_id)?)
        })
        .await
    }

    pub async fn set_conversation_fork_checkout_policy(
        &self,
        conversation_id: String,
        level: ContinuationLevel,
        choice: Option<ForkCheckoutPolicy>,
    ) -> Result<ForkCheckoutSettings> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let conversation = load_conversation(&tx, &conversation_id)?;
            let (project, _) = checkout_identity(&tx, &conversation.checkout_id)?;
            match (level, choice.map(ForkCheckoutPolicy::key)) {
                (ContinuationLevel::Project, Some(choice)) => tx.execute(
                    "INSERT INTO project_fork_checkout_policies VALUES (?1,?2)
                    ON CONFLICT(project_id) DO UPDATE SET choice = excluded.choice",
                    params![project, choice],
                )?,
                (ContinuationLevel::Project, None) => tx.execute(
                    "DELETE FROM project_fork_checkout_policies WHERE project_id = ?1",
                    [&project],
                )?,
                (ContinuationLevel::Conversation, Some(choice)) => tx.execute(
                    "INSERT INTO conversation_fork_checkout_policies VALUES (?1,?2)
                    ON CONFLICT(conversation_id) DO UPDATE SET choice = excluded.choice",
                    params![conversation_id, choice],
                )?,
                (ContinuationLevel::Conversation, None) => tx.execute(
                    "DELETE FROM conversation_fork_checkout_policies WHERE conversation_id = ?1",
                    [&conversation_id],
                )?,
            };
            let settings = fork_checkout_settings(&tx, &conversation)?;
            tx.commit()?;
            Ok(settings)
        })
        .await
    }

    pub async fn preview_conversation_fork_checkout(
        &self,
        conversation_id: String,
        expected_context_id: String,
        choice: Option<ForkCheckoutChoice>,
        selected_paths: Vec<String>,
        default_root: PathBuf,
    ) -> Result<ForkCheckoutPreview> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let source = check_idle_base(&tx, &conversation_id, &expected_context_id)?;
            let settings = fork_checkout_settings(&tx, &source)?;
            let choice = choice.or(settings.effective.choice());
            let id = new_id();
            let plan = inspect_fork_checkout(&tx, &source.checkout_id, &id,
                choice == Some(ForkCheckoutChoice::CreateSeparateWorktree), selected_paths, &default_root)?;
            if choice.is_some() {
                tx.execute("INSERT INTO conversation_fork_checkout_previews (id,source_conversation_id,source_context_id,payload)
                    VALUES (?1,?2,?3,?4)", params![id, source.id, source.context_id, protection::protect(&serde_json::to_vec(&plan)?)?])?;
            }
            tx.commit()?;
            Ok(ForkCheckoutPreview { preview_id: choice.map(|_| id), settings, choice, project_id: plan.project_id,
                repository_root: plan.repository_root, checkout_id: plan.source_checkout_id,
                checkout_root: plan.checkout_root, base_commit: plan.base_commit, dirty_paths: plan.dirty_paths, selected_paths: plan.selected_paths,
                target_checkout_root: plan.target_checkout_root, target_branch: plan.target_branch })
        }).await
    }

    pub async fn fork_conversation_with_checkout(
        &self,
        request: ForkConversationRequest,
    ) -> Result<Conversation> {
        ensure!(
            !request.title.trim().is_empty(),
            "conversation title is empty"
        );
        if let Some(text) = &request.edited_text {
            ensure!(!text.trim().is_empty(), "message is empty");
        }
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut source =
                check_idle_base(&tx, &request.conversation_id, &request.expected_context_id)?;
            let context = request
                .source_context_id
                .as_deref()
                .unwrap_or(&request.expected_context_id);
            if context != request.expected_context_id {
                let retained: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM conversation_history_journal
                    WHERE conversation_id = ?1 AND previous_context_id = ?2)",
                    params![source.id, context],
                    |row| row.get(0),
                )?;
                ensure!(
                    retained,
                    "only a retained source context may be forked here"
                );
            }
            let entries = retained_prefix(
                &tx,
                &source.id,
                context,
                request.through_revision.as_deref(),
            )?;
            if request.edited_text.is_some() {
                let revision = request
                    .through_revision
                    .as_deref()
                    .context("select the user message to edit in the fork")?;
                require_user_revision(&tx, &source, revision)?;
            }
            let target =
                reviewed_fork_target(&tx, &source, request.checkout_preview_id.as_deref())?;
            source.context_id = context.into();
            let mut fork = fork_history_at_checkout(
                &tx,
                &source,
                &entries,
                target.checkout_id.clone(),
                request.title,
            )?;
            if let Some(text) = request.edited_text {
                edit_fork_message(&tx, &mut fork, protection::protect(text.as_bytes())?)?;
            }
            target.claim(&tx, &fork.id)?;
            tx.commit()?;
            Ok(fork)
        })
        .await
    }
}

pub(super) fn reviewed_fork_target(
    db: &Connection,
    source: &Conversation,
    preview: Option<&str>,
) -> Result<ForkTarget> {
    let Some(id) = preview else {
        require_implicit_reuse(db, source)?;
        return Ok(ForkTarget {
            checkout_id: source.checkout_id.clone(),
            preview_id: None,
        });
    };
    let bytes: Vec<u8> = db.query_row("SELECT payload FROM conversation_fork_checkout_previews
        WHERE id = ?1 AND source_conversation_id = ?2 AND source_context_id = ?3 AND fork_conversation_id IS NULL",
        params![id,source.id,source.context_id], |row| row.get(0)).optional()?.context("checkout review changed or has already been used")?;
    let plan: ConversationWorktreePreview =
        serde_json::from_slice(&protection::unprotect(&bytes)?)?;
    ensure!(
        plan.source_checkout_id == source.checkout_id,
        "the conversation checkout changed after review"
    );
    let target = materialize_fork_checkout(db, &plan, id)?;
    Ok(ForkTarget {
        checkout_id: target.checkout_id,
        preview_id: Some(id.into()),
    })
}

impl ForkTarget {
    pub(super) fn claim(&self, db: &Connection, conversation: &str) -> Result<()> {
        if let Some(id) = &self.preview_id {
            ensure!(
                db.execute(
                    "UPDATE conversation_fork_checkout_previews SET fork_conversation_id = ?2
                WHERE id = ?1 AND fork_conversation_id IS NULL",
                    params![id, conversation]
                )? == 1,
                "checkout review has already been used"
            );
        }
        Ok(())
    }
}

pub(super) fn require_implicit_reuse(db: &Connection, source: &Conversation) -> Result<()> {
    ensure!(
        fork_checkout_settings(db, source)?.effective == ForkCheckoutPolicy::ReuseCurrentCheckout,
        "review a checkout for this conversation fork"
    );
    Ok(())
}

fn fork_checkout_settings(db: &Connection, source: &Conversation) -> Result<ForkCheckoutSettings> {
    let (project, _) = checkout_identity(db, &source.checkout_id)?;
    let (project_default, conversation_override): (Option<String>, Option<String>) = db.query_row(
        "SELECT (SELECT choice FROM project_fork_checkout_policies WHERE project_id = ?1),
                (SELECT choice FROM conversation_fork_checkout_policies WHERE conversation_id = ?2)", params![project,source.id],
        |row| Ok((row.get(0)?,row.get(1)?)))?;
    let project_default = project_default
        .as_deref()
        .map(ForkCheckoutPolicy::parse)
        .transpose()?
        .unwrap_or_default();
    let conversation_override = conversation_override
        .as_deref()
        .map(ForkCheckoutPolicy::parse)
        .transpose()?;
    Ok(ForkCheckoutSettings {
        project_default,
        conversation_override,
        effective: conversation_override.unwrap_or(project_default),
    })
}

pub(super) fn checkout_identity(db: &Connection, checkout: &str) -> Result<(String, PathBuf)> {
    db.query_row(
        "SELECT project_id, root FROM checkouts WHERE id = ?1",
        [checkout],
        |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
    )
    .context("the selected checkout is unavailable")
}

pub(super) fn resolve_continuation_checkout(
    db: &Connection,
    source: &Conversation,
    requested: Option<&str>,
) -> Result<ContinuationCheckout> {
    let (project_id, source_root) = checkout_identity(db, &source.checkout_id)?;
    let requested = requested.unwrap_or(&source.checkout_id);
    let root = if requested == source.checkout_id {
        source_root
    } else {
        let (target_project, target_root) = checkout_identity(db, requested)?;
        ensure!(
            target_project == project_id,
            "select a continuation checkout in the conversation's project"
        );
        target_root
    };
    Ok(ContinuationCheckout {
        checkout_id: requested.into(),
        project_id,
        root,
    })
}
