use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::SessionScope,
    jobs::JobRole,
    model_catalog::ModelSelection,
    protection,
    providers::{
        Capability, Provider, SessionIntent, SessionMode, SessionRequest, Transport,
        capabilities::{
            CapabilityScope, CapabilityStatus, CapabilitySubject, ReportedCapabilities,
        },
        configuration, sandbox,
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationTemplate {
    pub name: String,
    /// The saved role describes the template. The scheduler authorizes each job from its origin.
    pub role: JobRole,
    pub model: ModelSelection,
    pub effort: Option<String>,
    pub instructions: String,
    pub tool_scope: SessionMode,
    pub completion_contract: String,
    pub review_policy: TemplateReviewPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateReviewPolicy {
    Independent,
    CrossReview,
    HumanApproval,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTemplateVersion {
    pub revision: String,
    pub ordinal: u32,
    pub base_revision: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTemplateRevision {
    pub template_id: String,
    pub version: ConversationTemplateVersion,
    pub template: ConversationTemplate,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTemplateHead {
    pub template_id: String,
    pub version: ConversationTemplateVersion,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationTemplateDetail {
    pub revision: ConversationTemplateRevision,
    pub history: Vec<ConversationTemplateVersion>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateBase {
    pub template_id: String,
    pub expected_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SaveConversationTemplate {
    Saved { detail: ConversationTemplateDetail },
    Conflict { current: ConversationTemplateHead },
}

impl Store {
    pub async fn list_conversation_templates(
        &self,
        project: String,
    ) -> Result<Vec<ConversationTemplateHead>> {
        self.execute(move |db| {
            let mut statement = db.prepare(
                "SELECT id, head_revision FROM conversation_templates
                 WHERE project_id = ?1 ORDER BY id",
            )?;
            let mut rows = statement.query([&project])?;
            let mut heads = Vec::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let revision: String = row.get(1)?;
                heads.push(head(load_revision(db, &project, &id, &revision)?));
            }
            heads.sort_by(|left, right| {
                left.name
                    .cmp(&right.name)
                    .then(left.template_id.cmp(&right.template_id))
            });
            Ok(heads)
        })
        .await
    }

    pub async fn conversation_template_revision(
        &self,
        project: String,
        template_id: String,
        revision: String,
    ) -> Result<ConversationTemplateDetail> {
        self.execute(move |db| {
            detail(
                db,
                load_revision(db, &project, &template_id, &revision)?,
                &project,
            )
        })
        .await
    }

    pub async fn save_conversation_template(
        &self,
        project: String,
        base: Option<TemplateBase>,
        template: ConversationTemplate,
    ) -> Result<SaveConversationTemplate> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let outcome = save_conversation_template_in_tx(&tx, &project, base, template, false)?;
            tx.commit()?;
            Ok(outcome)
        })
        .await
    }

    pub async fn validate_conversation_template_checkout(
        &self,
        project: String,
        checkout: String,
    ) -> Result<()> {
        self.execute(move |db| {
            let belongs: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE id = ?1 AND project_id = ?2)",
                params![checkout, project],
                |row| row.get(0),
            )?;
            ensure!(
                belongs,
                "The template checkout belongs to a different project"
            );
            Ok(())
        })
        .await
    }

    pub async fn conversation_template_profile(
        &self,
        checkout: String,
        scope: SessionScope,
    ) -> Result<TemplateProfileMatrix> {
        let layout =
            repository::verify_checkout(self, checkout.clone(), scope.checkout.clone()).await?;
        let (scope, configuration, executable_revision, platform) =
            tokio::task::spawn_blocking(move || {
                let runner = DirectWindowsRunner;
                ensure!(
                    runner.identity() == scope.runner,
                    "The selected runner changed"
                );
                let executable = runner
                    .executable_revision(&scope.executable)
                    .context("The selected CLI executable is unavailable")?;
                let mut scope = scope;
                scope.checkout.clone_from(&layout.cwd);
                let (configuration, _, _) =
                    configuration::inspect(&scope, &layout, &ReportedCapabilities::default())?;
                Ok::<_, anyhow::Error>((
                    scope,
                    configuration,
                    executable,
                    runner.execution_platform(),
                ))
            })
            .await??;
        let mut tool_scopes = Vec::new();
        let mut capabilities = Vec::new();
        for mode in [SessionMode::ReadOnly, SessionMode::ApprovedWrite] {
            let mut operation = scope.clone();
            operation.mode = mode;
            let blocker = route_blocker(&operation, platform.as_ref());
            tool_scopes.push(TemplateToolScope { mode, blocker });
            if mode == scope.mode {
                let evidence_scope = CapabilityScope::from_dispatch(
                    &operation,
                    checkout.clone(),
                    Some(executable_revision.clone()),
                    platform.clone(),
                    configuration.clone(),
                    None,
                    None,
                );
                let descriptors = ReportedCapabilities::default().descriptors(&evidence_scope);
                let assessment = self
                    .resolve_capability_assessment(evidence_scope, descriptors)
                    .await?;
                capabilities.extend(assessment.entries.into_iter().filter_map(|entry| {
                    let CapabilitySubject::Protocol { capability } = entry.descriptor.subject
                    else {
                        return None;
                    };
                    Some(TemplateCapability {
                        capability,
                        status: entry.status(),
                    })
                }));
            }
        }
        Ok(TemplateProfileMatrix {
            cli_version: scope.cli_version,
            transport: scope.provider.adapter().transport(),
            tool_scopes,
            capabilities,
        })
    }
}

pub(crate) fn validate_conversation_template(template: &mut ConversationTemplate) -> Result<()> {
    template.name = template.name.trim().to_owned();
    ensure!(!template.name.is_empty(), "The template needs a name");
    ensure!(
        !template.model.profile_id.trim().is_empty() && !template.model.model_id.trim().is_empty(),
        "The template needs a CLI profile and model"
    );
    ensure!(
        matches!(
            template.tool_scope,
            SessionMode::ReadOnly | SessionMode::ApprovedWrite
        ),
        "Choose repository read or approved worktree write for this template"
    );
    Ok(())
}

pub(crate) fn save_conversation_template_in_tx(
    db: &Connection,
    project: &str,
    base: Option<TemplateBase>,
    mut template: ConversationTemplate,
    force_revision: bool,
) -> Result<SaveConversationTemplate> {
    validate_conversation_template(&mut template)?;
    let payload = protection::protect(&serde_json::to_vec(&template)?)?;
    let revision = Uuid::new_v4().to_string();
    let (id, ordinal, base_revision) = if let Some(base) = base {
        let current: Option<String> = db.query_row(
                    "SELECT head_revision FROM conversation_templates WHERE project_id = ?1 AND id = ?2",
                    params![project, base.template_id],
                    |row| row.get(0),
                ).optional()?;
        let current =
            current.context("The conversation template is unavailable in this project")?;
        let saved = load_revision(db, project, &base.template_id, &current)?;
        if current != base.expected_revision {
            return Ok(SaveConversationTemplate::Conflict {
                current: head(saved),
            });
        }
        if !force_revision && saved.template == template {
            return Ok(SaveConversationTemplate::Saved {
                detail: detail(db, saved, project)?,
            });
        }
        let ordinal = saved
            .version
            .ordinal
            .checked_add(1)
            .context("Template revision count exceeded")?;
        db.execute(
                    "UPDATE conversation_templates SET head_revision = ?1 WHERE project_id = ?2 AND id = ?3",
                    params![revision, project, base.template_id],
                )?;
        (base.template_id, ordinal, Some(current))
    } else {
        let id = Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO conversation_templates(project_id, id, head_revision) VALUES (?1, ?2, ?3)",
            params![project, id, revision],
        )?;
        (id, 1, None)
    };
    db.execute(
                "INSERT INTO conversation_template_revisions(project_id, template_id, revision, ordinal, base_revision, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![project, id, revision, ordinal, base_revision, payload],
            )?;
    let detail = detail(
        db,
        ConversationTemplateRevision {
            template_id: id,
            version: ConversationTemplateVersion {
                revision,
                ordinal,
                base_revision,
            },
            template,
        },
        project,
    )?;
    Ok(SaveConversationTemplate::Saved { detail })
}

fn head(revision: ConversationTemplateRevision) -> ConversationTemplateHead {
    ConversationTemplateHead {
        template_id: revision.template_id,
        version: revision.version,
        name: revision.template.name,
    }
}

pub(crate) fn load_revision(
    db: &Connection,
    project: &str,
    template_id: &str,
    revision: &str,
) -> Result<ConversationTemplateRevision> {
    let (ordinal, base_revision, payload): (u32, Option<String>, Vec<u8>) = db
        .query_row(
            "SELECT ordinal, base_revision, payload FROM conversation_template_revisions
         WHERE project_id = ?1 AND template_id = ?2 AND revision = ?3",
            params![project, template_id, revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("The conversation template revision is unavailable in this project")?;
    Ok(ConversationTemplateRevision {
        template_id: template_id.into(),
        version: ConversationTemplateVersion {
            revision: revision.into(),
            ordinal,
            base_revision,
        },
        template: serde_json::from_slice(&protection::unprotect(&payload)?)?,
    })
}

fn detail(
    db: &Connection,
    revision: ConversationTemplateRevision,
    project: &str,
) -> Result<ConversationTemplateDetail> {
    let mut statement = db.prepare(
        "SELECT revision, ordinal, base_revision FROM conversation_template_revisions
         WHERE project_id = ?1 AND template_id = ?2 ORDER BY ordinal DESC",
    )?;
    let history = statement
        .query_map(params![project, revision.template_id], |row| {
            Ok(ConversationTemplateVersion {
                revision: row.get(0)?,
                ordinal: row.get(1)?,
                base_revision: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ConversationTemplateDetail { revision, history })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateProfileMatrix {
    cli_version: String,
    transport: Transport,
    pub tool_scopes: Vec<TemplateToolScope>,
    capabilities: Vec<TemplateCapability>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateToolScope {
    pub mode: SessionMode,
    pub blocker: Option<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TemplateCapability {
    capability: Capability,
    status: CapabilityStatus,
}

fn route_blocker(
    scope: &SessionScope,
    platform: Option<&crate::runner::ExecutionPlatform>,
) -> Option<&'static str> {
    if scope.mode == SessionMode::ApprovedWrite
        && scope.provider == Provider::Antigravity
        && let Some(reason) = sandbox::admission_blocker(scope, platform, true)
    {
        return Some(reason);
    }
    scope
        .provider
        .adapter()
        .create_session(SessionRequest {
            checkout: scope.checkout.clone(),
            intent: SessionIntent::Start,
            mode: scope.mode,
            model: scope.model.clone(),
            effort: scope.effort.clone(),
        })
        .err()
        .map(|_| "This CLI cannot prepare a session with the selected tool scope")
}

#[cfg(test)]
mod tests;
