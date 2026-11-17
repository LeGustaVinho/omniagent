use serde_json::{Map, Value};

use crate::{
    model_catalog::ModelSelection,
    providers::Provider,
    workflows::{
        CommandConfiguration, CommandRegistration,
        mcp_tools::{McpServer, McpToolConfiguration, McpToolContract},
        stage_requirements::{RequiredStageSource, StageRequirements, StageSourcePath},
    },
};

use super::*;

const LOCAL: &str = "exchange-local-binding";
const EMPTY_REVISION: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const EMPTY_SERVER: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExchangeBindingRequirement {
    pub member: String,
    pub node: Option<String>,
    pub name: String,
    pub required_before_import: bool,
    pub kind: ExchangeBindingKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExchangeBindingKind {
    Command {
        argument_count: usize,
        input_fields: Vec<String>,
    },
    McpServer {
        tool: String,
        contract_revision: String,
        input_schema: Value,
        output_schema: Option<Value>,
        argument_fields: Vec<String>,
    },
    ModelProfile {
        provider: Provider,
        model_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExchangeBinding {
    Command {
        registration_id: String,
        registration_revision: String,
        arguments: Vec<String>,
        input: Map<String, Value>,
    },
    McpServer {
        server_id: String,
        server_revision: String,
        arguments: Map<String, Value>,
    },
    ModelProfile {
        selection: ModelSelection,
    },
}

pub(super) struct LocalCatalog {
    commands: Vec<CommandRegistration>,
    mcp_servers: Vec<McpServer>,
}

impl LocalCatalog {
    pub(super) async fn load(store: &Store, project: &str) -> Result<Self> {
        let (commands, mcp_servers) = tokio::try_join!(
            store.list_workflow_command_registrations(project.into()),
            store.workflow_mcp_servers(project.into()),
        )?;
        Ok(Self {
            commands,
            mcp_servers,
        })
    }

    fn command(&self, id: &str, revision: &str) -> Option<&CommandRegistration> {
        self.commands
            .iter()
            .find(|command| command.id == id && command.revision == revision)
    }

    fn mcp_server(&self, id: &str, revision: &str) -> Option<&McpServer> {
        self.mcp_servers
            .iter()
            .find(|server| server.id == id && server.revision == revision)
    }
}

#[derive(Clone, Copy)]
struct Location<'a> {
    member: &'a str,
    node: Option<&'a str>,
    destination: PackageDestination,
    reference: Option<PackageArtifactKind>,
}

impl Location<'_> {
    fn named_command(&self) -> bool {
        matches!(
            self.reference,
            Some(PackageArtifactKind::Command | PackageArtifactKind::Preset)
        )
    }
}

enum Slot<'a> {
    CommandMember(
        &'a mut PackageCommandBinding,
        &'a mut Vec<String>,
        &'a mut Map<String, Value>,
    ),
    CommandNode(&'a mut CommandConfiguration),
    McpMember(
        &'a mut String,
        &'a str,
        &'a Map<String, Value>,
        &'a Option<Map<String, Value>>,
    ),
    McpNode(&'a mut McpToolConfiguration),
    Model(&'a mut ModelSelection),
}

fn visit(
    member: &mut PackageMember,
    kinds: &BTreeMap<String, PackageArtifactKind>,
    mut visit: impl FnMut(Location<'_>, Slot<'_>) -> Result<()>,
) -> Result<()> {
    let local = Location {
        member: &member.key,
        node: None,
        destination: member.destination,
        reference: None,
    };
    let reference_kind = |slot: &str| match member.references.get(slot) {
        Some(PackageReference::Draft { key }) => kinds.get(key).copied(),
        Some(PackageReference::Parameter { kind, .. })
            if matches!(
                kind,
                PackageArtifactKind::Command | PackageArtifactKind::McpTool
            ) =>
        {
            Some(*kind)
        }
        _ => None,
    };
    match &mut member.body {
        PackageArtifactBody::Pipeline { graph } | PackageArtifactBody::Subflow { graph, .. } => {
            for node in &mut graph.nodes {
                config(
                    Location {
                        node: Some(&node.id),
                        reference: reference_kind(&node.id),
                        ..local
                    },
                    &mut node.configuration,
                    &mut visit,
                )?;
            }
        }
        PackageArtifactBody::Preset { configuration, .. } => config(
            Location {
                node: Some("$preset"),
                reference: reference_kind("$preset"),
                ..local
            },
            configuration,
            &mut visit,
        )?,
        PackageArtifactBody::Prompt { prompt } => {
            if let Some(configuration) = &mut prompt.configuration {
                config(
                    Location {
                        node: Some("execute"),
                        reference: reference_kind("role"),
                        ..local
                    },
                    configuration,
                    &mut visit,
                )?;
            }
        }
        PackageArtifactBody::Role { template } => {
            validate_sources(&template.requirements)?;
            visit(local, Slot::Model(&mut template.model))?;
        }
        PackageArtifactBody::Command {
            binding,
            arguments,
            input,
            ..
        } => visit(local, Slot::CommandMember(binding, arguments, input))?,
        PackageArtifactBody::McpTool {
            binding,
            tool,
            input_schema,
            output_schema,
        } => visit(
            local,
            Slot::McpMember(binding, tool, input_schema, output_schema),
        )?,
    }
    Ok(())
}

fn config(
    location: Location<'_>,
    configuration: &mut NodeConfiguration,
    visit: &mut impl FnMut(Location<'_>, Slot<'_>) -> Result<()>,
) -> Result<()> {
    match configuration {
        NodeConfiguration::AgentRole {
            model,
            requirements,
            ..
        } => {
            ensure!(
                requirements.bound_role.is_none() && requirements.role_template.is_none(),
                "Use an exact package role reference before exchanging a graph with a directly bound private role"
            );
            validate_sources(&requirements.stage)?;
            if let Some(model) = model {
                visit(location, Slot::Model(model))?;
            }
        }
        NodeConfiguration::Command(command) => {
            ensure!(
                location.destination == PackageDestination::Project
                    || location.node != Some("$preset") && location.named_command(),
                "A library Command node requires an exact Command reference or named parameter"
            );
            visit(location, Slot::CommandNode(command))?;
        }
        NodeConfiguration::McpTool(tool) => {
            if location.destination == PackageDestination::Project {
                visit(location, Slot::McpNode(tool))?;
            } else {
                ensure!(
                    location.reference == Some(PackageArtifactKind::McpTool)
                        && tool.server_id == EMPTY_SERVER
                        && tool.server_revision == EMPTY_REVISION
                        && tool.arguments.is_empty(),
                    "Library MCP configurations require a named declaration with server and arguments supplied by consumers"
                );
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_sources(requirements: &StageRequirements) -> Result<()> {
    ensure!(
        requirements.is_valid(),
        "The stage requirements are invalid or exceed the limits"
    );
    ensure!(
        !requirements.sources.iter().any(|source| matches!(
            source,
            RequiredStageSource::Instruction {
                path: StageSourcePath::Absolute { .. },
                ..
            } | RequiredStageSource::Skill {
                path: StageSourcePath::Absolute { .. },
                ..
            }
        )),
        "Absolute instruction and skill paths cannot be exchanged; use a checkout-relative reference"
    );
    ensure!(
        !requirements.sources.iter().any(|source| matches!(
            source,
            RequiredStageSource::Plugin { identity, .. }
                if identity.source_location.is_some() || identity.install_path.is_some()
        )),
        "Plugin requirements with installation paths or source locations must be configured in the destination"
    );
    Ok(())
}

pub(super) fn export_member(
    local: &LocalCatalog,
    member: &mut PackageMember,
    kinds: &BTreeMap<String, PackageArtifactKind>,
    requirements: &mut BTreeMap<String, ExchangeBindingRequirement>,
) -> Result<()> {
    let default_name = member.name.clone();
    visit(member, kinds, |at, slot| {
        let label = at.node.map_or_else(
            || default_name.clone(),
            |node| format!("{default_name} / {node}"),
        );
        let (name, required, kind) = match slot {
            Slot::CommandMember(binding, arguments, input) => {
                let name = match &*binding {
                    PackageCommandBinding::Registered { id, revision } => local
                        .command(id, revision)
                        .map_or(&label, |command| &command.name)
                        .clone(),
                    PackageCommandBinding::Required { name } => name.clone(),
                };
                *binding = PackageCommandBinding::Required { name: LOCAL.into() };
                (
                    name,
                    at.destination == PackageDestination::Project,
                    clear_command(arguments, input),
                )
            }
            Slot::CommandNode(command) => {
                let name = local
                    .command(&command.registration_id, &command.registration_revision)
                    .map_or(&label, |registered| &registered.name)
                    .clone();
                command.registration_id = LOCAL.into();
                command.registration_revision = EMPTY_REVISION.into();
                (
                    name,
                    !at.named_command(),
                    clear_command(&mut command.arguments, &mut command.input),
                )
            }
            Slot::McpMember(name, tool, input, output) => {
                let input_schema = Value::Object(input.clone());
                let output_schema = output.clone().map(Value::Object);
                let revision = digest(&serde_json::to_vec(&(tool, &input_schema, &output_schema))?);
                let name = std::mem::replace(name, LOCAL.into());
                (
                    name,
                    false,
                    ExchangeBindingKind::McpServer {
                        tool: tool.into(),
                        contract_revision: revision,
                        input_schema,
                        output_schema,
                        argument_fields: vec![],
                    },
                )
            }
            Slot::McpNode(tool) => {
                let server = local
                    .mcp_server(&tool.server_id, &tool.server_revision)
                    .context("An exact locally registered MCP definition is required for export")?;
                let contract = server
                    .tools
                    .iter()
                    .find(|entry| {
                        entry.name == tool.tool_name && entry.revision == tool.contract_revision
                    })
                    .context("The published MCP tool contract is no longer available")?;
                tool.server_id = EMPTY_SERVER.into();
                tool.server_revision = EMPTY_REVISION.into();
                let argument_fields = std::mem::take(&mut tool.arguments)
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();
                (
                    server.name.clone(),
                    true,
                    ExchangeBindingKind::McpServer {
                        tool: contract.name.clone(),
                        contract_revision: contract.revision.clone(),
                        input_schema: contract.input_schema.clone(),
                        output_schema: contract.output_schema.clone(),
                        argument_fields,
                    },
                )
            }
            Slot::Model(model) => {
                model.profile_id = LOCAL.into();
                let kind = ExchangeBindingKind::ModelProfile {
                    provider: model.provider,
                    model_id: model.model_id.clone(),
                };
                (label, true, kind)
            }
        };
        bounded_name(&name)?;
        let key = format!("binding-{}", requirements.len() + 1);
        requirements.insert(
            key,
            ExchangeBindingRequirement {
                member: at.member.into(),
                node: at.node.map(str::to_owned),
                name,
                required_before_import: required,
                kind,
            },
        );
        Ok(())
    })
}

fn clear_command(
    arguments: &mut Vec<String>,
    input: &mut Map<String, Value>,
) -> ExchangeBindingKind {
    let argument_count = std::mem::take(arguments).len();
    let input_fields = std::mem::take(input)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    ExchangeBindingKind::Command {
        argument_count,
        input_fields,
    }
}

pub(super) struct DestinationBindings<'a> {
    db: &'a Connection,
    project: &'a str,
    local: &'a LocalCatalog,
    requirements: &'a BTreeMap<String, ExchangeBindingRequirement>,
    choices: &'a BTreeMap<String, ExchangeBinding>,
    locations: BTreeMap<(&'a str, Option<&'a str>), &'a str>,
    used: BTreeSet<&'a str>,
    missing: Vec<String>,
    notes: Vec<String>,
    conflicts: Vec<ExchangeIssue>,
}

impl<'a> DestinationBindings<'a> {
    pub(super) fn new(
        db: &'a Connection,
        project: &'a str,
        local: &'a LocalCatalog,
        requirements: &'a BTreeMap<String, ExchangeBindingRequirement>,
        choices: &'a BTreeMap<String, ExchangeBinding>,
    ) -> Result<Self> {
        let mut locations = BTreeMap::new();
        for (id, requirement) in requirements {
            bounded_name(id)?;
            bounded_name(&requirement.name)?;
            ensure!(
                locations
                    .insert(
                        (requirement.member.as_str(), requirement.node.as_deref()),
                        id.as_str()
                    )
                    .is_none(),
                "Each package location needs at most one local binding"
            );
        }
        ensure!(
            choices.keys().all(|id| requirements.contains_key(id)),
            "The destination choices name an unknown binding"
        );
        Ok(Self {
            db,
            project,
            local,
            requirements,
            choices,
            locations,
            used: BTreeSet::new(),
            missing: vec![],
            notes: vec![],
            conflicts: vec![],
        })
    }

    pub(super) fn apply(
        &mut self,
        member: &mut PackageMember,
        kinds: &BTreeMap<String, PackageArtifactKind>,
    ) -> Result<()> {
        visit(member, kinds, |location, slot| {
            self.apply_slot(location, slot)
        })
    }

    fn apply_slot(&mut self, at: Location<'_>, slot: Slot<'_>) -> Result<()> {
        let key = *self
            .locations
            .get(&(at.member, at.node))
            .context("The exchange omitted a required local binding declaration")?;
        let requirement = &self.requirements[key];
        ensure!(
            self.used.insert(key),
            "The exchange reused a local binding in multiple locations"
        );
        let choice = self.choices.get(key);
        let needed = validate_placeholder(at, &slot, &requirement.kind)?;
        ensure!(
            requirement.required_before_import == needed,
            "The exchange misclassified a local binding"
        );
        if needed && choice.is_none() {
            self.missing.push(key.into());
        }
        if !needed {
            ensure!(
                choice.is_none(),
                "A named package binding is resolved by its consuming workflow"
            );
            self.notes.push(format!(
                "Bind {} in its consuming workflow before execution.",
                requirement.name
            ));
        }
        if let Err(error) = self.bind(at, slot, requirement, choice) {
            self.conflicts.push(issue(
                at.member,
                at.node,
                None,
                crate::diagnostics::preview(&error.to_string()),
            ));
        }
        Ok(())
    }

    fn bind(
        &mut self,
        at: Location<'_>,
        slot: Slot<'_>,
        requirement: &ExchangeBindingRequirement,
        choice: Option<&ExchangeBinding>,
    ) -> Result<()> {
        match slot {
            Slot::CommandMember(binding, arguments, input) => {
                let Some(choice) = choice else {
                    *binding = PackageCommandBinding::Required {
                        name: requirement.name.clone(),
                    };
                    return Ok(());
                };
                let configuration = self.command(
                    choice,
                    &requirement.kind,
                    crate::workflows::CommandOutputFormat::Json,
                )?;
                *binding = PackageCommandBinding::Registered {
                    id: configuration.registration_id,
                    revision: configuration.registration_revision,
                };
                *arguments = configuration.arguments;
                *input = configuration.input;
            }
            Slot::CommandNode(command) => {
                if let Some(choice) = choice {
                    *command = self.command(choice, &requirement.kind, command.output)?;
                }
            }
            Slot::McpMember(name, ..) => {
                name.clone_from(&requirement.name);
            }
            Slot::McpNode(tool) => {
                let Some(choice) = choice else {
                    return Ok(());
                };
                let (server, contract, arguments) = self.mcp(choice, &requirement.kind)?;
                tool.server_id.clone_from(&server.id);
                tool.server_revision.clone_from(&server.revision);
                tool.contract_revision.clone_from(&contract.revision);
                tool.arguments.clone_from(arguments);
                crate::workflows::mcp_tools::validate_mcp_binding(self.db, self.project, tool)?;
                if server.outbound_network {
                    self.notes.push(format!(
                        "Local MCP server {} can use outbound network access.",
                        server.name
                    ));
                }
                self.notes.push(format!(
                    "{} uses destination-scoped MCP authorization.",
                    at.node.unwrap_or(at.member)
                ));
            }
            Slot::Model(model) => {
                let Some(ExchangeBinding::ModelProfile { selection }) = choice else {
                    ensure!(
                        choice.is_none(),
                        "Select a local CLI profile for the required provider and model"
                    );
                    return Ok(());
                };
                bounded_name(&selection.profile_id)?;
                ensure!(
                    selection.profile_id != LOCAL
                        && selection.provider == model.provider
                        && selection.model_id == model.model_id,
                    "The selected CLI profile must keep the required provider and model"
                );
                model.profile_id.clone_from(&selection.profile_id);
                self.notes.push(format!("The local profile for {} is qualified and authenticated by the normal launch flow.", at.node.unwrap_or(at.member)));
            }
        }
        Ok(())
    }

    fn command(
        &self,
        choice: &ExchangeBinding,
        requirement: &ExchangeBindingKind,
        output: crate::workflows::CommandOutputFormat,
    ) -> Result<CommandConfiguration> {
        let ExchangeBinding::Command {
            registration_id,
            registration_revision,
            arguments,
            input,
        } = choice
        else {
            anyhow::bail!("Select a local Command registration");
        };
        let ExchangeBindingKind::Command {
            argument_count,
            input_fields,
        } = requirement
        else {
            anyhow::bail!("The required Command contract is unavailable");
        };
        ensure!(
            arguments.len() == *argument_count && matches_fields(input, input_fields),
            "The local Command needs the declared number of arguments and exact input fields"
        );
        self.local
            .command(registration_id, registration_revision)
            .context("The Command registration is unavailable in this project")?;
        let command = CommandConfiguration {
            registration_id: registration_id.clone(),
            registration_revision: registration_revision.clone(),
            arguments: arguments.clone(),
            input: input.clone(),
            output,
        };
        crate::workflows::command::validate_command_binding(self.db, self.project, &command)?;
        Ok(command)
    }

    fn mcp<'b>(
        &self,
        choice: &'b ExchangeBinding,
        requirement: &ExchangeBindingKind,
    ) -> Result<(&'a McpServer, &'a McpToolContract, &'b Map<String, Value>)> {
        let ExchangeBinding::McpServer {
            server_id,
            server_revision,
            arguments,
        } = choice
        else {
            anyhow::bail!("Select a registered local MCP server");
        };
        let ExchangeBindingKind::McpServer {
            tool,
            contract_revision,
            input_schema,
            output_schema,
            argument_fields,
        } = requirement
        else {
            anyhow::bail!("The required MCP tool contract is unavailable");
        };
        ensure!(
            matches_fields(arguments, argument_fields),
            "The local MCP binding needs the declared argument fields"
        );
        let server = self
            .local
            .mcp_server(server_id, server_revision)
            .context("The MCP server registration is unavailable in this project")?;
        let contract = server
            .tools
            .iter()
            .find(|contract| &contract.name == tool)
            .context("The destination MCP server does not provide the selected tool")?;
        ensure!(
            &contract.revision == contract_revision
                && &contract.input_schema == input_schema
                && &contract.output_schema == output_schema,
            "The destination MCP tool has incompatible input or output schemas"
        );
        Ok((server, contract, arguments))
    }

    pub(super) fn finish(self) -> Result<(Vec<String>, Vec<String>, Vec<ExchangeIssue>)> {
        ensure!(
            self.used.len() == self.requirements.len(),
            "The exchange contains a binding for an unknown artifact location"
        );
        Ok((self.missing, self.notes, self.conflicts))
    }
}

fn matches_fields(values: &Map<String, Value>, fields: &[String]) -> bool {
    fields.len() == values.len() && fields.iter().all(|field| values.contains_key(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn library_mcp_presets_preserve_a_typed_placeholder_for_the_consumer() -> Result<()> {
        let local = LocalCatalog {
            commands: vec![],
            mcp_servers: vec![],
        };
        let config = McpToolConfiguration {
            server_id: EMPTY_SERVER.into(),
            server_revision: EMPTY_REVISION.into(),
            tool_name: "review".into(),
            contract_revision: "a".repeat(64),
            arguments: Map::new(),
            argument_mappings: BTreeMap::new(),
            destination_pointer: None,
        };
        let mut preset = PackageMember {
            key: "review-preset".into(),
            name: "Review".into(),
            destination: PackageDestination::Library,
            base: None,
            body: PackageArtifactBody::Preset {
                node_kind: BuiltinNode::McpTool,
                configuration: NodeConfiguration::McpTool(config.clone()),
                editable_fields: vec![],
            },
            references: BTreeMap::from([(
                "$preset".into(),
                PackageReference::Draft {
                    key: "review-tool".into(),
                },
            )]),
        };
        let kinds = BTreeMap::from([("review-tool".into(), PackageArtifactKind::McpTool)]);
        let mut requirements = BTreeMap::new();
        let before = preset.clone();
        export_member(&local, &mut preset, &kinds, &mut requirements)?;
        assert_eq!(preset, before);
        assert!(requirements.is_empty());

        preset.references.insert(
            "$preset".into(),
            PackageReference::Parameter {
                name: "Review".into(),
                kind: PackageArtifactKind::Command,
            },
        );
        assert!(export_member(&local, &mut preset, &kinds, &mut requirements).is_err());

        preset.references.clone_from(&before.references);
        let PackageArtifactBody::Preset {
            configuration: NodeConfiguration::McpTool(tool),
            ..
        } = &mut preset.body
        else {
            unreachable!()
        };
        tool.arguments
            .insert("destination".into(), json!("machine-local"));
        assert!(export_member(&local, &mut preset, &kinds, &mut requirements).is_err());
        Ok(())
    }
}

fn validate_placeholder(
    at: Location<'_>,
    slot: &Slot<'_>,
    kind: &ExchangeBindingKind,
) -> Result<bool> {
    let required = match (slot, kind) {
        (Slot::CommandMember(binding, args, input), ExchangeBindingKind::Command { .. }) => {
            ensure!(
                matches!(binding, PackageCommandBinding::Required { name } if name == LOCAL)
                    && args.is_empty()
                    && input.is_empty(),
                "Portable Command members use named bindings without local arguments"
            );
            at.destination == PackageDestination::Project
        }
        (Slot::CommandNode(command), ExchangeBindingKind::Command { .. }) => {
            ensure!(
                command.registration_id == LOCAL
                    && command.registration_revision == EMPTY_REVISION
                    && command.arguments.is_empty()
                    && command.input.is_empty(),
                "Portable Command nodes cannot contain machine registrations or local arguments"
            );
            !at.named_command()
        }
        (
            Slot::McpMember(name, tool, input, output),
            ExchangeBindingKind::McpServer {
                tool: expected_tool,
                contract_revision,
                input_schema,
                output_schema,
                ..
            },
        ) => {
            let input = Value::Object((*input).clone());
            let output = (*output).clone().map(Value::Object);
            ensure!(
                name.as_str() == LOCAL
                    && *tool == expected_tool.as_str()
                    && &input == input_schema
                    && &output == output_schema
                    && &digest(&serde_json::to_vec(&(tool, input, output))?) == contract_revision,
                "The named MCP artifact differs from its declared contract"
            );
            false
        }
        (
            Slot::McpNode(tool),
            ExchangeBindingKind::McpServer {
                tool: expected_tool,
                contract_revision,
                ..
            },
        ) => {
            ensure!(
                tool.server_id == EMPTY_SERVER
                    && tool.server_revision == EMPTY_REVISION
                    && tool.arguments.is_empty()
                    && &tool.tool_name == expected_tool
                    && &tool.contract_revision == contract_revision,
                "Portable MCP nodes cannot include machine registrations or local arguments"
            );
            true
        }
        (Slot::Model(model), ExchangeBindingKind::ModelProfile { provider, model_id }) => {
            ensure!(
                model.profile_id == LOCAL
                    && model.provider == *provider
                    && &model.model_id == model_id,
                "Portable model selections need a named local profile"
            );
            true
        }
        _ => anyhow::bail!("The named binding does not match the artifact's local resource type"),
    };
    Ok(required)
}
