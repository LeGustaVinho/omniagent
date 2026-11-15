use std::{io, path::Path, time::Duration};

use anyhow::{Context, Result, bail, ensure};

use crate::{
    jobs::{DispatchRejection, RequiredCustomization},
    process::TerminalOutcome,
    repository::CheckoutLayout,
    runner::Runner,
};

use super::{
    super::{
        Provider,
        capabilities::{CapabilityAssessment, CapabilityStatus, CapabilitySubject},
        configuration::{InstructionInventory, InstructionSelection},
        plugins::PluginInventoryStatus,
        profiles::{DESKTOP_PERMISSION_REVISION, command_probe_with_limits, published_version},
    },
    TurnExecution,
};

impl TurnExecution {
    pub(super) fn verify_effective_profile(&self, runner: &dyn Runner) -> Result<()> {
        let scope = &self.plan.scope;
        ensure!(
            scope.runner == runner.identity(),
            "The selected runner changed"
        );
        if self.admission.is_some() {
            ensure!(
                scope.executable.is_file(),
                "The selected CLI executable is unavailable"
            );
        }
        let Some(gate) = self
            .admission
            .as_ref()
            .filter(|_| scope.permission_revision == DESKTOP_PERMISSION_REVISION)
        else {
            return Ok(());
        };
        let configured = self
            .app
            .block_on(self.store.configured_executable(scope.provider))?;
        let adapter = scope.provider.adapter();
        let effective = adapter.discover(configured.executable.as_deref().map(Path::new))?;
        ensure!(
            effective == scope.executable,
            "The configured CLI executable changed"
        );
        let neutral = tempfile::tempdir()?;
        let cwd = dunce::canonicalize(neutral.path())?;
        let output = command_probe_with_limits(
            |request| {
                let _guard = self.app.block_on(gate.enter()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Interrupted, "The scheduler is exiting")
                })?;
                runner.launch(request)
            },
            &effective,
            &cwd,
            adapter.version_arguments(),
            Duration::from_secs(5),
            64 * 1024,
        )?;
        let version = published_version(scope.provider, &output.stdout)
            .or_else(|| published_version(scope.provider, &output.stderr));
        ensure!(
            output.status == TerminalOutcome::Exited(0)
                && version.as_ref() == Some(&scope.cli_version),
            "The configured CLI build changed or could not be verified"
        );
        Ok(())
    }

    pub(super) fn reject_dispatch(&self, rejection: DispatchRejection) -> Result<()> {
        self.app.block_on(
            self.store
                .record_dispatch_rejection(self.plan.run.id.clone(), rejection),
        )
    }

    pub(super) fn check_dispatch_requirements(
        &self,
        assessment: &CapabilityAssessment,
        instructions: &InstructionInventory,
        checkout: &CheckoutLayout,
    ) -> Result<()> {
        let requirements = self.app.block_on(
            self.store
                .job_dispatch_requirements(self.plan.run.id.clone()),
        )?;
        let capabilities = requirements
            .capabilities
            .into_iter()
            .filter(|capability| {
                !assessment.entries.iter().any(|entry| {
                    entry.descriptor.subject
                        == CapabilitySubject::Protocol {
                            capability: *capability,
                        }
                        && entry.status() == CapabilityStatus::Verified
                })
            })
            .collect::<Vec<_>>();
        let mut customizations = Vec::new();
        let launch = self
            .scoped_launch
            .as_ref()
            .context("Missing scoped launch evidence")?;
        let current_review = requirements
            .customizations
            .iter()
            .any(|requirement| matches!(requirement, RequiredCustomization::ExecutableEntry { .. }))
            .then(|| {
                self.app.block_on(self.store.resolve_launch_review(
                    self.plan.run.checkout_id.clone(),
                    checkout,
                    self.plan.scope.provider,
                ))
            })
            .transpose()?;
        for requirement in requirements.customizations {
            let verified = match &requirement {
                RequiredCustomization::Instruction { path } => {
                    let path = instructions.checkout_root.join(path);
                    let applied = if launch.provider == Provider::Antigravity {
                        &instructions.sources
                    } else {
                        &launch.instruction_sources
                    };
                    applied.iter().any(|source| {
                        source.path == path
                            && source.revision.is_some()
                            && source.selection == InstructionSelection::Expected
                            && instructions.sources.iter().any(|current| {
                                current.path == source.path && current.revision == source.revision
                            })
                    })
                }
                RequiredCustomization::ExecutableEntry { id } => {
                    !launch.excluded.contains_key(id)
                        && current_review.as_ref().is_some_and(|review| {
                            review.approved.iter().any(|(entry, _)| {
                                &entry.id == id
                                    && launch.approved.get(id) == Some(&entry.fingerprint)
                            })
                        })
                }
                RequiredCustomization::Skill { id } => {
                    self.reported_skills.as_ref().is_some_and(|catalog| {
                        catalog.reported
                            && catalog.entries.iter().any(|entry| {
                                entry.id == *id
                                    && entry.enabled == Some(true)
                                    && entry.revision.is_some()
                            })
                    })
                }
                RequiredCustomization::Plugin { id } => {
                    self.plugin_inventory.as_ref().is_some_and(|inventory| {
                        inventory.report.status == PluginInventoryStatus::Reported
                            && inventory
                                .report
                                .plugins
                                .iter()
                                .any(|plugin| plugin.id == *id && plugin.enabled == Some(true))
                            && self.reported_skills.as_ref().is_some_and(|catalog| {
                                catalog.reported
                                    && catalog.entries.iter().any(|entry| {
                                        entry.plugin_id.as_ref() == Some(id)
                                            && entry.enabled == Some(true)
                                    })
                            })
                    })
                }
            };
            if !verified {
                customizations.push(requirement);
            }
        }
        if !capabilities.is_empty() || !customizations.is_empty() {
            self.reject_dispatch(DispatchRejection::Requirements {
                capabilities,
                customizations,
            })?;
            bail!("The current worktree or CLI profile could not verify the required job evidence");
        }
        Ok(())
    }
}
