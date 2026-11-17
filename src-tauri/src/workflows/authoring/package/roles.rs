use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::Serialize;

use crate::{
    conversations::SessionScope,
    model_catalog::ModelSelection,
    providers::SessionMode,
    workflows::{
        NodeConfiguration, WorkflowDefinition,
        role_templates::{ConversationTemplate, TemplateReviewPolicy},
        runtime::load_pinned_node,
        stage_requirements::StageRoleRevision,
    },
};

use super::{
    PackageArtifactBody, PackageArtifactKind, PackageArtifactVersion, PackageDestination,
    PublishedPackageArtifact, bounded_name, publish::load_published,
};

#[cfg(test)]
mod tests;

pub(super) fn validate_authoring_body(
    body: &PackageArtifactBody,
    destination: PackageDestination,
) -> Result<()> {
    let check = |configuration: &NodeConfiguration| -> Result<()> {
        if let NodeConfiguration::AgentRole { requirements, .. } = configuration {
            ensure!(
                requirements.bound_role.is_none(),
                "Select the role as an exact package reference on the Agent"
            );
            ensure!(
                !requirements.prompt_delegate,
                "Set invited Roles on the Prompt card"
            );
            ensure!(
                destination != PackageDestination::Library || requirements.role_template.is_none(),
                "A library Agent needs a library role reference or a binding parameter"
            );
        }
        Ok(())
    };
    match body {
        PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } => {
            for node in &graph.nodes {
                check(&node.configuration)?;
            }
        }
        PackageArtifactBody::Preset { configuration, .. } => check(configuration)?,
        PackageArtifactBody::Prompt { prompt } => {
            if let Some(configuration) = &prompt.configuration {
                check(configuration)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn valid_bound_role(version: &PackageArtifactVersion) -> bool {
    version.kind == PackageArtifactKind::Role
        && bounded_name(&version.id).is_ok()
        && bounded_name(&version.revision).is_ok()
}

pub(super) fn has_bound_roles(definition: &WorkflowDefinition) -> bool {
    definition.nodes.iter().any(|node| {
        matches!(&node.instance.configuration,
        NodeConfiguration::AgentRole { requirements, .. } if requirements.bound_role.is_some())
    })
}

pub(super) fn bind(
    configuration: &mut NodeConfiguration,
    artifact: &PublishedPackageArtifact,
) -> Result<()> {
    let template = role_body(artifact)?;
    let NodeConfiguration::AgentRole {
        model,
        requirements,
        ..
    } = configuration
    else {
        anyhow::bail!("A reusable role needs an Agent node");
    };
    ensure!(
        requirements
            .bound_role
            .as_ref()
            .is_none_or(|previous| *previous == artifact.version),
        "The Agent already selects a different role revision"
    );
    if model.is_none() {
        *model = Some(template.model.clone());
    }
    requirements.bound_role = Some(artifact.version.clone());
    Ok(())
}

pub(crate) fn role_version(requirement: &StageRoleRevision) -> PackageArtifactVersion {
    PackageArtifactVersion {
        destination: PackageDestination::Project,
        kind: PackageArtifactKind::Role,
        id: requirement.template_id.clone(),
        revision: requirement.revision.clone(),
    }
}

pub(crate) fn load_role(
    db: &Connection,
    project: &str,
    version: &PackageArtifactVersion,
) -> Result<Option<ConversationTemplate>> {
    ensure!(
        valid_bound_role(version),
        "A pinned Agent role must refer to an exact role revision"
    );
    load_published(db, project, version)?
        .map(|artifact| {
            ensure!(
                artifact.version == *version,
                "The stored role is a different revision"
            );
            Ok(role_body(&artifact)?.clone())
        })
        .transpose()
}

fn role_body(artifact: &PublishedPackageArtifact) -> Result<&ConversationTemplate> {
    ensure!(
        artifact.version.kind == PackageArtifactKind::Role,
        "The referenced artifact is not a role"
    );
    match &artifact.body {
        PackageArtifactBody::Role { template } => Ok(template),
        _ => anyhow::bail!("The referenced role has no template"),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRolePin {
    pub node_id: String,
    pub node_revision: String,
    pub version: PackageArtifactVersion,
    pub requirements_only: bool,
    pub role: Option<WorkflowRoleDetails>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoleDetails {
    pub name: String,
    pub model: ModelSelection,
    pub effort: Option<String>,
    pub tool_scope: SessionMode,
    pub review_policy: TemplateReviewPolicy,
}

pub(crate) fn role_pins(
    db: &Connection,
    project: &str,
    definition: &WorkflowDefinition,
) -> Result<Vec<WorkflowRolePin>> {
    let mut pins = Vec::new();
    for node in &definition.nodes {
        let NodeConfiguration::AgentRole {
            model,
            requirements,
            ..
        } = &node.instance.configuration
        else {
            continue;
        };
        let bound = requirements.bound_role.clone();
        let inherited = requirements
            .role_template
            .as_ref()
            .map(role_version)
            .filter(|version| bound.as_ref() != Some(version));
        for (version, requirements_only) in bound
            .into_iter()
            .map(|bound| (bound, false))
            .chain(inherited.into_iter().map(|inherited| (inherited, true)))
        {
            let role = load_role(db, project, &version)?.map(|template| WorkflowRoleDetails {
                name: template.name,
                model: if requirements_only {
                    template.model
                } else {
                    model.clone().unwrap_or(template.model)
                },
                effort: template.effort,
                tool_scope: template.tool_scope,
                review_policy: template.review_policy,
            });
            pins.push(WorkflowRolePin {
                node_id: node.instance.id.clone(),
                node_revision: node.revision.clone(),
                version,
                requirements_only,
                role,
            });
        }
    }
    Ok(pins)
}

pub(crate) fn validate_role_pins(
    db: &Connection,
    project: &str,
    definition: &WorkflowDefinition,
) -> Result<()> {
    for pin in role_pins(db, project, definition)? {
        pin.role.with_context(|| {
            format!(
                "Agent {} refers to an unavailable role revision {}",
                pin.node_id, pin.version.revision
            )
        })?;
    }
    Ok(())
}

pub(crate) fn qualify_bound_role(
    template: &ConversationTemplate,
    model_override: Option<&ModelSelection>,
    scope: &SessionScope,
) -> Result<()> {
    ensure!(
        scope.mode != SessionMode::ApprovedWrite
            || template.tool_scope == SessionMode::ApprovedWrite,
        "The pinned role does not permit repository writes"
    );
    let model = model_override.unwrap_or(&template.model);
    ensure!(
        scope.provider == model.provider
            && scope.profile_id == model.profile_id
            && scope.model.as_deref() == Some(model.model_id.as_str())
            && scope.effort == template.effort,
        "The qualified CLI profile, model, and effort must match the pinned role"
    );
    match template.review_policy {
        TemplateReviewPolicy::Independent => Ok(()),
        TemplateReviewPolicy::CrossReview => anyhow::bail!(
            "The pinned role requires cross-review. This Agent does not bind that review gate."
        ),
        TemplateReviewPolicy::HumanApproval => anyhow::bail!(
            "The pinned role requires human approval. This Agent does not bind that approval gate."
        ),
    }
}

pub(crate) fn validate_attempt_role(
    db: &Connection,
    project: &str,
    run: &str,
    node_id: &str,
    attempt: &str,
) -> Result<()> {
    let node = load_pinned_node(db, project, run, node_id)?
        .context("The workflow attempt lost its pinned node")?;
    let NodeConfiguration::AgentRole {
        model,
        requirements,
        ..
    } = &node.instance.configuration
    else {
        return Ok(());
    };
    let Some(version) = &requirements.bound_role else {
        return Ok(());
    };
    let template =
        load_role(db, project, version)?.context("The pinned workflow role is unavailable")?;
    let session: String = db.query_row(
        "SELECT session.scope FROM agent_attempts attempt
         JOIN agent_sessions session ON session.id = attempt.agent_session_id
         WHERE attempt.run_id = ?1",
        [attempt],
        |row| row.get(0),
    )?;
    qualify_bound_role(&template, model.as_ref(), &serde_json::from_str(&session)?)
}

pub(crate) fn role_instructions(template: &ConversationTemplate, instructions: &str) -> String {
    format!(
        "Pinned workflow role instructions\n{}\n\nPinned role completion contract\n{}\n\nStage instructions\n{}",
        template.instructions, template.completion_contract, instructions
    )
}
