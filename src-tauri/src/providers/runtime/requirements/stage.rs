use std::fs::File;

use anyhow::{Context, Result, bail, ensure};

use crate::{
    jobs::DispatchRejection,
    providers::{
        Provider,
        capabilities::CapabilityAssessment,
        composer_customizations::{SelectedComposerSkill, current_skill},
        configuration::{InstructionInventory, InstructionSelection, InstructionSource},
        plugins::{PluginInventory, PluginInventoryStatus},
        skills::SkillCatalog,
    },
    workflows::stage_requirements::{
        RequiredStageSource, StagePluginIdentity, StageRequirementDecision, StageSourceEvidence,
        WorkflowStageAssessment, check_capability,
    },
};

use super::TurnExecution;

#[cfg(test)]
mod tests;

impl TurnExecution {
    pub(in crate::providers::runtime) fn check_stage_requirements(
        &self,
        assessment: &CapabilityAssessment,
        instructions: &InstructionInventory,
    ) -> Result<Vec<(SelectedComposerSkill, File)>> {
        let run = self.plan.run.id.clone();
        let Some(pinned) = self
            .app
            .block_on(self.store.stage_requirements_for_attempt(run.clone()))?
        else {
            return Ok(Vec::new());
        };
        let launch = self
            .scoped_launch
            .as_ref()
            .context("Missing scoped launch evidence")?;
        let applied = if launch.provider == Provider::Antigravity {
            &instructions.sources
        } else {
            &launch.instruction_sources
        };
        let mut decisions = pinned
            .requirements
            .capabilities
            .iter()
            .map(|subject| check_capability(assessment, subject))
            .collect::<Vec<_>>();
        let mut selected = Vec::new();
        for source in pinned.requirements.sources {
            let result = qualify_source(
                &source,
                launch.provider,
                instructions,
                applied,
                self.reported_skills.as_ref(),
                self.plugin_inventory.as_ref(),
                self.plan.native_goal_seed.is_none(),
            );
            let (evidence, blocker) = match result {
                Ok((evidence, invocation)) => {
                    selected.extend(invocation);
                    (Some(evidence), None)
                }
                Err(error) => (None, Some(error.to_string())),
            };
            decisions.push(StageRequirementDecision::Source {
                source,
                evidence,
                blocker,
            });
        }
        let blocked = decisions
            .iter()
            .any(|decision| decision.blocker().is_some());
        self.app.block_on(self.store.save_stage_assessment(
            run,
            WorkflowStageAssessment {
                node_id: pinned.node_id,
                instance_revision: pinned.instance_revision,
                role: pinned.role,
                role_template: pinned.role_template,
                scope: assessment.scope.clone(),
                decisions,
            },
        ))?;
        if blocked {
            self.reject_dispatch(DispatchRejection::StageRequirements)?;
            bail!(
                "The effective stage profile or worktree failed its saved requirements. See the attempt's stage assessment"
            );
        }
        Ok(selected)
    }
}

fn qualify_source(
    source: &RequiredStageSource,
    provider: Provider,
    instructions: &InstructionInventory,
    applied: &[InstructionSource],
    skills: Option<&SkillCatalog>,
    plugins: Option<&PluginInventory>,
    standard_turn: bool,
) -> Result<(StageSourceEvidence, Option<(SelectedComposerSkill, File)>)> {
    ensure!(
        source.provider() == provider,
        "The required source belongs to a different provider"
    );
    let root = &instructions.checkout_root;
    let reported = skills.filter(|catalog| catalog.reported);
    match source {
        RequiredStageSource::Instruction { path, revision, .. } => {
            let mut matching = applied.iter().filter(|instruction| {
                path.matches(root, &instruction.path)
                    && instruction.selection == InstructionSelection::Expected
                    && instruction.revision.as_ref() == Some(revision)
                    && instructions.sources.iter().any(|current| {
                        current.path == instruction.path && current.revision == instruction.revision
                    })
            });
            let instruction = matching.next().context(
                "The required instruction revision was not qualified and applied to this checkout",
            )?;
            ensure!(
                matching.next().is_none(),
                "The qualified instruction source is ambiguous"
            );
            Ok((
                StageSourceEvidence::Instruction {
                    instruction: instruction.clone(),
                },
                None,
            ))
        }
        RequiredStageSource::Skill {
            path,
            namespace,
            revision,
            invoke,
            ..
        } => {
            let catalog =
                reported.context("The CLI did not report a skill inventory for this session")?;
            let mut matching = catalog.entries.iter().filter(|entry| {
                entry.provider == provider
                    && entry.enabled == Some(true)
                    && &entry.plugin_id == namespace
                    && path.matches(root, &entry.path)
            });
            let entry = matching
                .next()
                .context("The exact skill path and namespace were not reported as enabled")?;
            ensure!(
                matching.next().is_none(),
                "The reported skill identity is ambiguous"
            );
            let (skill, file) = current_skill(entry)?
                .context("The selected SKILL.md is unavailable as a bounded local file")?;
            ensure!(
                &skill.revision == revision,
                "The selected SKILL.md contents changed"
            );
            if *invoke && (provider != Provider::Codex || !standard_turn) {
                bail!("This adapter route cannot invoke a selected skill");
            }
            Ok((
                StageSourceEvidence::Skill {
                    skill: skill.clone(),
                },
                invoke.then_some((skill, file)),
            ))
        }
        RequiredStageSource::Plugin { identity, .. } => {
            let inventory = plugins
                .filter(|inventory| {
                    inventory.report.provider == provider
                        && inventory.report.status == PluginInventoryStatus::Reported
                })
                .context("The CLI did not report a complete plugin inventory")?;
            let mut reported_id = inventory
                .report
                .plugins
                .iter()
                .filter(|plugin| plugin.enabled == Some(true) && plugin.id == identity.id);
            let plugin = reported_id
                .next()
                .filter(|plugin| StagePluginIdentity::from(*plugin) == *identity)
                .context("The exact plugin installation was not reported as enabled")?;
            ensure!(
                reported_id.next().is_none(),
                "Multiple enabled installations share this plugin ID, so session skills cannot identify its source"
            );
            let catalog = reported.context(
                "The CLI did not report a skill inventory to qualify the plugin in this session",
            )?;
            let skill_ids = catalog
                .entries
                .iter()
                .filter(|entry| {
                    entry.provider == provider
                        && entry.enabled == Some(true)
                        && entry.plugin_id.as_ref() == Some(&identity.id)
                })
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            ensure!(
                !skill_ids.is_empty(),
                "No enabled session skill could be attributed to this plugin installation"
            );
            Ok((
                StageSourceEvidence::Plugin {
                    inventory_id: inventory.id.clone(),
                    plugin: plugin.clone(),
                    skill_ids,
                },
                None,
            ))
        }
    }
}
