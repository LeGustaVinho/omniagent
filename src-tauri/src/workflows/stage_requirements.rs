use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    model_catalog::ModelSelection,
    protection,
    providers::{
        Capability, Provider,
        capabilities::{
            CapabilityAssessment, CapabilityScope, CapabilityStatus, CapabilitySubject,
            ReportedCapabilities,
        },
        composer_customizations::{SelectedComposerSkill, current_skill, probe_codex_skills},
        configuration::{self, InstructionSelection, InstructionSource},
        plugins::{PluginInventoryStatus, ReportedPlugin, probe_run_plugins},
    },
    repository,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
};

use super::{
    NodeConfiguration,
    authoring::package::{PackageArtifactVersion, roles},
    runtime::load_pinned_node,
};

mod history;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilitySubject>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<RequiredStageSource>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageRequirementConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_template: Option<StageRoleRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_role: Option<PackageArtifactVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<super::stage_goal::StageGoalConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<super::stage_plan::StagePlanConfiguration>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub review_reuse: bool,
    #[serde(default, skip_serializing_if = "StageRequirements::is_empty")]
    pub stage: StageRequirements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageRoleRevision {
    pub template_id: String,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageSourcePath {
    Checkout { path: PathBuf },
    Absolute { path: PathBuf },
}

impl StageSourcePath {
    fn display(&self) -> std::path::Display<'_> {
        match self {
            Self::Checkout { path } | Self::Absolute { path } => path.display(),
        }
    }

    pub(crate) fn from_inventory(path: &Path, checkout: &Path) -> Self {
        match path.strip_prefix(checkout) {
            Ok(relative) => Self::Checkout {
                path: relative.to_owned(),
            },
            Err(_) => Self::Absolute {
                path: path.to_owned(),
            },
        }
    }

    pub(crate) fn matches(&self, checkout: &Path, actual: &Path) -> bool {
        match self {
            Self::Checkout { path } => actual.strip_prefix(checkout).ok() == Some(path.as_path()),
            Self::Absolute { path } => dunce::simplified(actual) == dunce::simplified(path),
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Checkout { path } => {
                !path.as_os_str().is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
            }
            Self::Absolute { path } => {
                path.is_absolute()
                    && !path
                        .components()
                        .any(|part| matches!(part, Component::ParentDir))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RequiredStageSource {
    Instruction {
        provider: Provider,
        path: StageSourcePath,
        revision: String,
    },
    Skill {
        provider: Provider,
        path: StageSourcePath,
        namespace: Option<String>,
        revision: String,
        #[serde(default)]
        invoke: bool,
    },
    Plugin {
        provider: Provider,
        identity: StagePluginIdentity,
    },
}

impl RequiredStageSource {
    pub(crate) fn provider(&self) -> Provider {
        match self {
            Self::Instruction { provider, .. }
            | Self::Skill { provider, .. }
            | Self::Plugin { provider, .. } => *provider,
        }
    }
}

/// Match the provider's exact reported source and installation scope. Plugin
/// IDs by themselves can be repeated across registries and scopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagePluginIdentity {
    pub id: String,
    pub scope: Option<String>,
    pub source: Option<String>,
    pub marketplace: Option<String>,
    pub version: Option<String>,
    pub source_location: Option<String>,
    pub install_path: Option<String>,
}

impl From<&ReportedPlugin> for StagePluginIdentity {
    fn from(plugin: &ReportedPlugin) -> Self {
        Self {
            id: plugin.id.clone(),
            scope: plugin.scope.clone(),
            source: plugin.source.clone(),
            marketplace: plugin.marketplace.clone(),
            version: plugin.version.clone(),
            source_location: plugin.source_location.clone(),
            install_path: plugin.install_path.clone(),
        }
    }
}

impl StageRequirements {
    pub(crate) fn is_empty(&self) -> bool {
        self.capabilities.is_empty() && self.sources.is_empty()
    }

    pub(crate) fn is_valid(&self) -> bool {
        fn named(value: &str) -> bool {
            !value.trim().is_empty() && value.len() <= 1024
        }
        fn digest(value: &str) -> bool {
            value.len() == 64 && value.bytes().all(|item| item.is_ascii_hexdigit())
        }
        self.capabilities.len() <= 32
            && self.sources.len() <= 32
            && self
                .sources
                .iter()
                .filter(|item| matches!(item, RequiredStageSource::Skill { invoke: true, .. }))
                .count()
                <= 8
            && self.capabilities.iter().all(|item| match item {
                CapabilitySubject::Protocol { .. } => true,
                CapabilitySubject::Tool {
                    id,
                    version,
                    operation,
                } => named(id) && named(version) && named(operation),
                _ => false,
            })
            && self.sources.iter().all(|item| match item {
                RequiredStageSource::Instruction { path, revision, .. }
                | RequiredStageSource::Skill { path, revision, .. } => {
                    path.is_valid() && digest(revision)
                }
                RequiredStageSource::Plugin { identity, .. } => named(&identity.id),
            })
    }
}

impl StageRequirementConfiguration {
    pub(crate) fn is_empty(&self) -> bool {
        self.role_template.is_none()
            && self.bound_role.is_none()
            && self.goal.is_none()
            && self.plan.is_none()
            && !self.review_reuse
            && self.stage.is_empty()
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.stage.is_valid()
            && self
                .goal
                .as_ref()
                .is_none_or(|goal| goal.validate().is_ok())
            && self.plan.as_ref().is_none_or(|plan| plan.is_valid())
            && self.bound_role.as_ref().is_none_or(roles::valid_bound_role)
            && self.role_template.as_ref().is_none_or(|role| {
                !role.template_id.trim().is_empty() && !role.revision.trim().is_empty()
            })
    }
}

pub(crate) struct PinnedStageRequirements {
    pub node_id: String,
    pub instance_revision: String,
    pub role: String,
    pub role_template: Option<StageRoleRevision>,
    pub requirements: StageRequirements,
}

fn effective_requirements(
    db: &Connection,
    project: &str,
    configured: &StageRequirementConfiguration,
) -> Result<StageRequirements> {
    let requirements_only = configured
        .role_template
        .as_ref()
        .map(roles::role_version)
        .filter(|version| configured.bound_role.as_ref() != Some(version));
    let mut effective = StageRequirements::default();
    for version in configured.bound_role.iter().chain(requirements_only.iter()) {
        let template = roles::load_role(db, project, version)?
            .context("A pinned workflow role revision is unavailable")?;
        merge_requirements(&mut effective, &template.requirements);
    }
    merge_requirements(&mut effective, &configured.stage);
    if let Some(plan) = configured.plan {
        plan.require_capabilities(&mut effective);
    }
    ensure!(
        effective.is_valid(),
        "The combined role and stage requirements are invalid or exceed the limits"
    );
    Ok(effective)
}

fn merge_requirements(effective: &mut StageRequirements, next: &StageRequirements) {
    for subject in &next.capabilities {
        if !effective.capabilities.contains(subject) {
            effective.capabilities.push(subject.clone());
        }
    }
    for source in &next.sources {
        if !effective.sources.contains(source) {
            effective.sources.push(source.clone());
        }
    }
}

pub(crate) async fn validate_stage_requirements(
    store: &Store,
    project: String,
    configured: StageRequirementConfiguration,
) -> Result<()> {
    store
        .execute(move |db| effective_requirements(db, &project, &configured).map(|_| ()))
        .await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StageRequirementDecision {
    Capability {
        subject: CapabilitySubject,
        status: Option<CapabilityStatus>,
        evidence_id: Option<String>,
        blocker: Option<String>,
    },
    Source {
        source: RequiredStageSource,
        evidence: Option<StageSourceEvidence>,
        blocker: Option<String>,
    },
}

impl StageRequirementDecision {
    pub(crate) fn blocker(&self) -> Option<&str> {
        match self {
            Self::Capability { blocker, .. } | Self::Source { blocker, .. } => blocker.as_deref(),
        }
    }
}

pub(crate) fn check_capability(
    assessment: &CapabilityAssessment,
    subject: &CapabilitySubject,
) -> StageRequirementDecision {
    let matching = assessment
        .entries
        .iter()
        .find(|entry| entry.descriptor.subject == *subject);
    let status = matching.map(|entry| entry.status());
    let blocker = match status {
        Some(CapabilityStatus::Verified) => None,
        Some(CapabilityStatus::Blocked) => Some("The effective execution policy blocks this capability".into()),
        Some(CapabilityStatus::Unavailable) => Some("The effective CLI or model does not expose this capability".into()),
        Some(_) if subject.is_codex_image_generation() => Some("No qualified Codex image matches the effective profile, model, mode, settings and worktree".into()),
        Some(_) => Some("No successful authenticated fixture matches the effective CLI, model, settings and worktree".into()),
        None => Some("The effective adapter did not report this exact capability subject".into()),
    };
    StageRequirementDecision::Capability {
        subject: subject.clone(),
        status,
        evidence_id: matching
            .and_then(|entry| entry.evidence.as_ref())
            .map(|evidence| evidence.id.clone()),
        blocker,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StageSourceEvidence {
    Instruction {
        instruction: InstructionSource,
    },
    Skill {
        skill: SelectedComposerSkill,
    },
    Plugin {
        inventory_id: String,
        plugin: ReportedPlugin,
        skill_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStageAssessment {
    pub node_id: String,
    pub instance_revision: String,
    pub role: String,
    pub role_template: Option<StageRoleRevision>,
    pub scope: CapabilityScope,
    pub decisions: Vec<StageRequirementDecision>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageInventoryChoice {
    pub label: String,
    pub source: RequiredStageSource,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageRequirementInventory {
    pub provider: Provider,
    pub cli_version: String,
    pub choices: Vec<StageInventoryChoice>,
    pub protocol_capabilities: Vec<Capability>,
    pub capability_preview: Vec<StageRequirementDecision>,
    pub notices: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageProfileMatch {
    pub selection: ModelSelection,
    pub attempt_id: Option<String>,
    pub checked_at: Option<String>,
    pub blockers: Vec<String>,
}

impl Store {
    pub async fn workflow_stage_inventory(
        &self,
        project: String,
        checkout_id: String,
        scope: SessionScope,
        configured: StageRequirementConfiguration,
    ) -> Result<StageRequirementInventory> {
        self.validate_conversation_template_checkout(project.clone(), checkout_id.clone())
            .await?;
        let configured = self
            .execute(move |db| effective_requirements(db, &project, &configured))
            .await?;
        let (mut inventory, evidence_scope) =
            self.stage_inventory_with_scope(checkout_id, scope).await?;
        let descriptors = ReportedCapabilities::default().descriptors(&evidence_scope);
        let assessment = self
            .resolve_capability_assessment(evidence_scope, descriptors)
            .await?;
        inventory.capability_preview = configured
            .capabilities
            .iter()
            .map(|subject| check_capability(&assessment, subject))
            .collect();
        Ok(inventory)
    }

    pub async fn workflow_stage_profile_match(
        &self,
        project: String,
        checkout_id: String,
        scope: SessionScope,
        selection: ModelSelection,
        configured: StageRequirementConfiguration,
    ) -> Result<StageProfileMatch> {
        self.validate_conversation_template_checkout(project.clone(), checkout_id.clone())
            .await?;
        ensure!(
            selection.provider == scope.provider
                && selection.profile_id == scope.profile_id
                && Some(selection.model_id.as_str()) == scope.model.as_deref(),
            "The model profile changed before evaluation"
        );
        let (inventory, evidence_scope) =
            self.stage_inventory_with_scope(checkout_id, scope).await?;
        self.execute(move |db| {
            history::match_profile(
                db,
                &project,
                selection,
                &configured,
                &evidence_scope,
                &inventory.choices,
            )
        })
        .await
    }

    async fn stage_inventory_with_scope(
        &self,
        checkout_id: String,
        scope: SessionScope,
    ) -> Result<(StageRequirementInventory, CapabilityScope)> {
        let checkout =
            repository::verify_checkout(self, checkout_id.clone(), scope.checkout.clone()).await?;
        tokio::task::spawn_blocking(move || {
            let runner = DirectWindowsRunner;
            ensure!(scope.runner == runner.identity(), "The selected runner changed");
            let executable = runner.executable_revision(&scope.executable).context("The selected CLI executable is unavailable")?;
            let reported = ReportedCapabilities::default();
            let (configuration, discovered, instructions) = configuration::inspect(&scope, &checkout, &reported)?;
            let mut choices = Vec::new();
            let mut notices = Vec::new();
            for instruction in instructions.sources {
                let Some(revision) = instruction.revision else { continue; };
                if !matches!(instruction.selection, InstructionSelection::Expected | InstructionSelection::Candidate) {
                    continue;
                }
                let path = StageSourcePath::from_inventory(&instruction.path, &checkout.root);
                choices.push(StageInventoryChoice {
                    label: format!("Instruction · {} · {:?}", path.display(), instruction.selection),
                    source: RequiredStageSource::Instruction { provider: scope.provider, path, revision },
                });
            }
            let provider_skills = if scope.provider == Provider::Codex {
                probe_codex_skills(&scope, &checkout).ok().flatten().filter(|catalog| catalog.reported)
            } else {
                None
            };
            let (skills, provenance) = if let Some(catalog) = provider_skills {
                (catalog.entries.into_iter().filter(|entry| entry.enabled == Some(true)).collect(), "CLI reported")
            } else {
                notices.push("The current preview only found filesystem skill candidates. Each required skill needs confirmation by the CLI session.".into());
                (discovered, "filesystem candidate")
            };
            for entry in &skills {
                let Some((skill, _)) = current_skill(entry)? else { continue; };
                let path = StageSourcePath::from_inventory(&skill.path, &checkout.root);
                let namespace = skill.namespace.as_deref().unwrap_or("local");
                choices.push(StageInventoryChoice {
                    label: format!("Skill · {} · {namespace} · {} · {provenance}", skill.name, path.display()),
                    source: RequiredStageSource::Skill { provider: scope.provider, path, namespace: skill.namespace, revision: skill.revision, invoke: false },
                });
            }
            let plugins = probe_run_plugins(&scope, |request| runner.launch(request));
            if plugins.status != PluginInventoryStatus::Reported {
                notices.push(format!("The CLI plugin inventory could not be confirmed ({:?}).", plugins.status));
            } else {
                for plugin in plugins.plugins.iter().filter(|plugin| plugin.enabled == Some(true)) {
                    let identity = StagePluginIdentity::from(plugin);
                    let scope_label = plugin.scope.as_deref().unwrap_or("scope not reported");
                    let origin = plugin.source_location.as_deref().or(plugin.marketplace.as_deref())
                        .or(plugin.install_path.as_deref()).unwrap_or("source location not reported");
                    choices.push(StageInventoryChoice {
                        label: format!("Plugin · {} · {scope_label} · {origin}", plugin.id),
                        source: RequiredStageSource::Plugin { provider: scope.provider, identity },
                    });
                }
            }
            let inventory = StageRequirementInventory {
                provider: scope.provider, cli_version: scope.cli_version.clone(), choices,
                protocol_capabilities: Capability::ALL.to_vec(),
                capability_preview: Vec::new(), notices,
            };
            let evidence_scope = CapabilityScope::from_dispatch(
                &scope, checkout_id, Some(executable), runner.execution_platform(), configuration, None, None,
            );
            Ok::<_, anyhow::Error>((inventory, evidence_scope))
        }).await?
    }

    pub(crate) async fn stage_requirements_for_attempt(
        &self,
        run: String,
    ) -> Result<Option<PinnedStageRequirements>> {
        self.execute(move |db| {
            let pinned: Option<(String, String, String)> = db
                .query_row(
                    "SELECT o.project_id, o.pipeline_run_id, o.node_instance_id FROM job_attempts a
                 JOIN workflow_job_inputs i ON i.job_id = a.job_id
                 JOIN job_pipeline_origins o ON o.job_id = a.job_id WHERE a.run_id = ?1",
                    [&run],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((project, workflow, node_id)) = pinned else {
                return Ok(None);
            };
            let node = load_pinned_node(db, &project, &workflow, &node_id)?
                .context("The scheduled workflow attempt lost its pinned node")?;
            let NodeConfiguration::AgentRole {
                role, requirements, ..
            } = node.instance.configuration
            else {
                return Ok(None);
            };
            let effective = effective_requirements(db, &project, &requirements)?;
            Ok((!effective.is_empty()).then_some(PinnedStageRequirements {
                node_id,
                instance_revision: node.revision,
                role,
                role_template: requirements.role_template,
                requirements: effective,
            }))
        })
        .await
    }

    pub(crate) async fn save_stage_assessment(
        &self,
        run: String,
        assessment: WorkflowStageAssessment,
    ) -> Result<()> {
        self.execute(move |db| {
            db.execute(
                "INSERT INTO workflow_stage_assessments (run_id, payload) VALUES (?1, ?2)
                 ON CONFLICT(run_id) DO UPDATE SET payload = excluded.payload, checked_at = excluded.checked_at",
                rusqlite::params![run, protection::protect(&serde_json::to_vec(&assessment)?)?],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn workflow_stage_assessment(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<WorkflowStageAssessment>> {
        self.execute(move |db| {
            let saved: Option<Vec<u8>> = db.query_row(
                "SELECT s.payload FROM workflow_stage_assessments s JOIN job_attempts a ON a.run_id = s.run_id
                 JOIN jobs j ON j.id = a.job_id WHERE j.project_id = ?1 AND s.run_id = ?2",
                [&project, &run], |row| row.get(0),
            ).optional()?;
            saved.map(|bytes| Ok(serde_json::from_slice(&protection::unprotect(&bytes)?)?)).transpose()
        }).await
    }
}
