use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::json;

use crate::{
    model_catalog::ModelSelection,
    providers::Provider,
    workflows::{
        CommandConfiguration, CommandOutputFormat, CommandRegistrationRequest, CommandTarget,
        DeliveryRoles, NodeInputs, NodeInstance, Wire, WorkflowGraph, WorkflowHostRequest,
        WorkflowHostSubmission, builtin_delivery_graph,
        mcp_tools::{McpServer, McpServerRequest, McpToolConfiguration, WorkflowMcpToolSubmission},
        tests::approved_delivery_repository,
    },
};

use super::*;

#[tokio::test]
async fn local_exchange_resolves_registry_and_schema_references_without_grants_or_execution()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_root = temp.path().join("source-installation");
    let dest_root = temp.path().join("destination-installation");
    fs::create_dir(&source_root)?;
    fs::create_dir(&dest_root)?;
    let source_store = Store::open(source_root.join("store.sqlite"))?;
    let source_repo = approved_delivery_repository(&source_store, &source_root).await?;
    let source = source_repo.project_id.clone();
    let dest_database = dest_root.join("store.sqlite");
    let dest_store = Store::open(dest_database.clone())?;
    let dest_repo = approved_delivery_repository(&dest_store, &dest_root).await?;
    let dest = dest_repo.project_id.clone();
    let executable = source_root.join("source-mcp-executable-never-export.exe");
    build_stdio_fixture(&executable)?;
    let local_executable = dest_root.join("destination-mcp.exe");
    fs::copy(&executable, &local_executable)?;
    let original_command = source_store
        .register_workflow_command(
            source.clone(),
            CommandRegistrationRequest {
                name: "Portable formatter".into(),
                target: CommandTarget::Executable {
                    path: executable.clone(),
                },
            },
        )
        .await?;
    let original_server = register_server(
        &source_store,
        &source,
        &executable,
        Path::new(&source_repo.checkout_root).join("brief.md"),
        false,
    )
    .await?;
    let original_tool = tool(&original_server, "source-destination-never-export")?;
    source_store
        .set_workflow_mcp_grant(source.clone(), original_tool.clone(), true)
        .await?;
    let native_command = CommandConfiguration {
        registration_id: original_command.id.clone(),
        registration_revision: original_command.revision.clone(),
        arguments: vec!["--token=source-argument-never-export".into()],
        input: serde_json::from_value(json!({"api_key":"source-input-never-export"}))?,
        output: CommandOutputFormat::Json,
    };
    let mut package = sample_package(native_command, original_tool)?;
    let contract = &original_server.tools[0];
    package.members.push(member(
        "reusable-tool",
        PackageDestination::Library,
        PackageArtifactBody::McpTool {
            binding: "Tools for reviewed work".into(),
            tool: contract.name.clone(),
            input_schema: contract
                .input_schema
                .as_object()
                .context("fixture input schema missing")?
                .clone(),
            output_schema: contract
                .output_schema
                .as_ref()
                .and_then(|schema| schema.as_object())
                .cloned(),
        },
    ));
    let initial = publish(
        &source_store,
        &source,
        source_store
            .create_workflow_package(source.clone(), None, package)
            .await?,
    )
    .await?;
    let selected = [PackageArtifactKind::Pipeline, PackageArtifactKind::McpTool]
        .map(|kind| by_kind(&initial, kind).version.clone())
        .to_vec();
    let transfer = source_store
        .export_workflow_exchange(source.clone(), selected)
        .await?;
    assert_eq!(transfer.package.members.len(), 5);
    assert_eq!(
        transfer
            .bindings
            .values()
            .filter(|slot| slot.required_before_import)
            .count(),
        3
    );
    let document = serde_json::to_string(&transfer)?;
    for forbidden in [
        &original_command.id,
        &original_server.id,
        "source-mcp-executable-never-export.exe",
        "source-destination-never-export",
        "source-argument-never-export",
        "source-input-never-export",
        "source-cli-profile-never-export",
        "workflow_mcp_grants",
    ] {
        assert!(!document.contains(forbidden));
    }
    let missing = dest_store
        .preview_workflow_exchange(dest.clone(), document.clone(), BTreeMap::new())
        .await?;
    assert_eq!(missing.missing_bindings.len(), 3);
    assert!(!missing.can_import);
    assert!(missing.bindings.values().any(|requirement| {
        requirement.name == "Tools for reviewed work"
            && !requirement.required_before_import
            && matches!(&requirement.kind, ExchangeBindingKind::McpServer { tool, .. } if tool == &contract.name)
    }));
    assert!(
        missing
            .destination_notes
            .iter()
            .any(|note| note.contains("Bind Tools for reviewed work in its consuming workflow"))
    );
    assert!(
        missing
            .permissions
            .contains(&ExchangePermission::RepositoryWritesRequireRelease)
    );
    assert!(
        missing
            .permissions
            .contains(&ExchangePermission::ExternalMcpRequiresLocalApproval)
    );
    assert!(
        missing
            .permissions
            .contains(&ExchangePermission::ModelRequiresLocalAuthentication)
    );
    assert!(
        dest_store
            .import_reviewed_workflow_exchange(
                dest.clone(),
                document.clone(),
                BTreeMap::new(),
                missing.review_digest
            )
            .await
            .is_err()
    );
    assert!(
        dest_store
            .workflow_packages(dest.clone(), None)
            .await?
            .is_empty()
    );

    let local_command = dest_store
        .register_workflow_command(
            dest.clone(),
            CommandRegistrationRequest {
                name: "Destination formatter".into(),
                target: CommandTarget::Executable {
                    path: local_executable.clone(),
                },
            },
        )
        .await?;
    let local_server = register_server(
        &dest_store,
        &dest,
        &local_executable,
        Path::new(&dest_repo.checkout_root).join("brief.md"),
        false,
    )
    .await?;
    let changed_server = register_server(
        &dest_store,
        &dest,
        &local_executable,
        Path::new(&dest_repo.checkout_root).join("brief.md"),
        true,
    )
    .await?;
    let mut choices = BTreeMap::new();
    for (id, requirement) in &transfer.bindings {
        if !requirement.required_before_import {
            continue;
        }
        let binding = match &requirement.kind {
            ExchangeBindingKind::Command { .. } => ExchangeBinding::Command {
                registration_id: local_command.id.clone(),
                registration_revision: local_command.revision.clone(),
                arguments: vec!["--configured-locally".into()],
                input: serde_json::from_value(json!({"api_key":"destination input"}))?,
            },
            ExchangeBindingKind::McpServer { .. } => ExchangeBinding::McpServer {
                server_id: local_server.id.clone(),
                server_revision: local_server.revision.clone(),
                arguments: serde_json::from_value(
                    json!({"destination":"confirmed-local-destination","action":"mutate"}),
                )?,
            },
            ExchangeBindingKind::ModelProfile { provider, model_id } => {
                ExchangeBinding::ModelProfile {
                    selection: ModelSelection {
                        provider: *provider,
                        model_id: model_id.clone(),
                        profile_id: "destination-cli-profile".into(),
                    },
                }
            }
        };
        choices.insert(id.clone(), binding);
    }
    let mut incompatible = choices.clone();
    for choice in incompatible.values_mut() {
        if let ExchangeBinding::McpServer {
            server_id,
            server_revision,
            ..
        } = choice
        {
            *server_id = changed_server.id.clone();
            *server_revision = changed_server.revision.clone();
        }
    }
    let changed = dest_store
        .preview_workflow_exchange(dest.clone(), document.clone(), incompatible)
        .await?;
    assert!(!changed.can_import);
    assert!(
        changed
            .conflicts
            .iter()
            .any(|issue| issue.node.as_deref() == Some("local-tool")
                && issue.blocking
                && issue.message.contains("incompatible"))
    );
    let mut foreign = choices.clone();
    for choice in foreign.values_mut() {
        if let ExchangeBinding::Command {
            registration_id,
            registration_revision,
            ..
        } = choice
        {
            *registration_id = original_command.id.clone();
            *registration_revision = original_command.revision.clone();
        }
    }
    assert!(
        !dest_store
            .preview_workflow_exchange(dest.clone(), document.clone(), foreign)
            .await?
            .can_import
    );

    let reviewed = dest_store
        .preview_workflow_exchange(dest.clone(), document.clone(), choices.clone())
        .await?;
    assert!(reviewed.can_import, "{reviewed:?}");
    assert!(reviewed.missing_bindings.is_empty());
    let mut incompatible_ports = transfer.clone();
    let pipeline = incompatible_ports
        .sources
        .iter()
        .find(|(_, origin)| origin.kind == PackageArtifactKind::Pipeline)
        .map(|(key, _)| key.clone())
        .context("The pipeline revision disappeared from the exchange")?;
    let pins = incompatible_ports
        .definitions
        .get_mut(&pipeline)
        .context("The pipeline definitions were not exported")?;
    let start = pins
        .get_mut("start")
        .context("The Start definition was not exported")?;
    let port = start
        .outputs
        .iter_mut()
        .find(|port| port.name == "document")
        .context("The Start document port was not exported")?;
    port.name = "other-document".into();
    let blocked = dest_store
        .preview_workflow_exchange(
            dest.clone(),
            serde_json::to_string(&incompatible_ports)?,
            choices.clone(),
        )
        .await?;
    assert!(!blocked.can_import);
    assert!(blocked.conflicts.iter().any(|issue| issue.blocking
        && issue.node.as_deref() == Some("start")
        && issue.port.as_deref() == Some("document")));

    let mut machine_plugin = transfer.clone();
    let pipeline = machine_plugin
        .package
        .members
        .iter_mut()
        .find(|member| member.key == pipeline)
        .context("The pipeline disappeared from the exchange")?;
    let PackageArtifactBody::Pipeline { graph } = &mut pipeline.body else {
        unreachable!()
    };
    let plan = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .context("Missing Plan")?;
    let NodeConfiguration::AgentRole { requirements, .. } = &mut plan.configuration else {
        unreachable!()
    };
    requirements.stage.sources.push(
        crate::workflows::stage_requirements::RequiredStageSource::Plugin {
            provider: Provider::Codex,
            identity: crate::workflows::stage_requirements::StagePluginIdentity {
                id: "machine-local-plugin".into(),
                scope: None,
                source: None,
                marketplace: None,
                version: None,
                source_location: Some("C:/source-private-plugin-registry".into()),
                install_path: Some("C:/source-private-plugin-install".into()),
            },
        },
    );
    let blocked = dest_store
        .preview_workflow_exchange(
            dest.clone(),
            serde_json::to_string(&machine_plugin)?,
            choices.clone(),
        )
        .await
        .expect_err("An exchange cannot silently copy machine-local plugin paths");
    assert!(
        blocked
            .to_string()
            .contains("configured in the destination")
    );

    let mut altered: serde_json::Value = serde_json::from_str(&document)?;
    altered["package"]["name"] = json!("Altered after review");
    assert!(
        dest_store
            .import_reviewed_workflow_exchange(
                dest.clone(),
                altered.to_string(),
                choices.clone(),
                reviewed.review_digest.clone()
            )
            .await
            .is_err()
    );
    altered["grants"] = json!(["transfer-unauthorized-grant"]);
    assert!(
        dest_store
            .preview_workflow_exchange(dest.clone(), altered.to_string(), choices.clone())
            .await
            .is_err()
    );
    assert!(
        dest_store
            .workflow_packages(dest.clone(), None)
            .await?
            .is_empty()
    );
    let draft = dest_store
        .import_reviewed_workflow_exchange(dest.clone(), document, choices, reviewed.review_digest)
        .await?;
    assert!(draft.publication.is_none());
    assert!(
        dest_store
            .workflow_package_catalog(dest.clone())
            .await?
            .is_empty()
    );
    assert!(
        dest_store
            .list_workflow_runs(dest.clone())
            .await?
            .is_empty()
    );
    let published = publish(&dest_store, &dest, draft).await?;
    let version = by_kind(&published, PackageArtifactKind::Pipeline)
        .version
        .clone();
    assert_ne!(
        version.id,
        by_kind(&initial, PackageArtifactKind::Pipeline).version.id
    );
    let old = dest_store
        .start_workflow_run(
            dest.clone(),
            WorkflowRevision {
                id: version.id.clone(),
                revision: version.revision.clone(),
            },
        )
        .await?;
    let definition = dest_store
        .workflow_definition(dest.clone(), old.version.clone())
        .await?
        .context("imported definition missing")?;
    assert!(definition.nodes.iter().any(|node| node.instance.id == "write-command" &&
        matches!(&node.instance.configuration, NodeConfiguration::Command(config) if config.registration_id == local_command.id && config.arguments == ["--configured-locally"])));
    assert!(definition.nodes.iter().any(|node| node.instance.id == "local-tool" &&
        matches!(&node.instance.configuration, NodeConfiguration::McpTool(config) if config.server_id == local_server.id && config.arguments["destination"] == "confirmed-local-destination")));
    assert!(definition.nodes.iter().any(|node| node.instance.id == "plan" &&
        matches!(&node.instance.configuration, NodeConfiguration::AgentRole { model: Some(model), .. } if model.profile_id == "destination-cli-profile")));
    assert_eq!(
        definition.subflows[0].version,
        by_kind(&published, PackageArtifactKind::Subflow).version
    );
    assert_eq!(grant_count(&source_store).await?, 1);
    assert_eq!(grant_count(&dest_store).await?, 0);
    let captured = dest_store
        .capture_repository_markdown(
            dest_repo.checkout_id.clone(),
            Path::new(&dest_repo.checkout_root).join("brief.md"),
        )
        .await?;
    dest_store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: dest.clone(),
            run_id: old.id.clone(),
            node_id: "start".into(),
            inputs: NodeInputs::new(),
            request: WorkflowHostRequest::Start {
                document: captured.revision,
            },
        })
        .await?;
    let invocation = dest_store
        .submit_workflow_mcp_tool(WorkflowMcpToolSubmission {
            project_id: dest.clone(),
            run_id: old.id.clone(),
            node_id: "local-tool".into(),
            checkout_id: dest_repo.checkout_id.clone(),
            inputs: NodeInputs::new(),
        })
        .await?;
    assert_eq!(invocation.queue_state, crate::jobs::QueueState::Paused);
    assert!(invocation.latest_attempt.is_none());

    let prior = by_kind(&published, PackageArtifactKind::Subflow)
        .version
        .clone();
    let mut changed_interface = dest_store
        .export_workflow_exchange(dest.clone(), vec![prior.clone()])
        .await?;
    let subflow = changed_interface
        .package
        .members
        .iter_mut()
        .find(|member| member.body.kind() == PackageArtifactKind::Subflow)
        .context("subflow was not exported")?;
    let PackageArtifactBody::Subflow { outputs, .. } = &mut subflow.body else {
        unreachable!()
    };
    let port = outputs
        .remove("done")
        .context("initial public port missing")?;
    outputs.insert("loaded".into(), port);
    let changed_interface = serde_json::to_string(&changed_interface)?;
    let preview = dest_store
        .preview_workflow_exchange(dest.clone(), changed_interface.clone(), BTreeMap::new())
        .await?;
    assert!(preview.can_import, "{preview:?}");
    assert!(
        preview
            .conflicts
            .iter()
            .any(|issue| !issue.blocking && issue.port.as_deref() == Some("done"))
    );
    assert!(
        preview
            .conflicts
            .iter()
            .any(|issue| !issue.blocking && issue.port.as_deref() == Some("loaded"))
    );
    let edited = dest_store
        .import_reviewed_workflow_exchange(
            dest.clone(),
            changed_interface,
            BTreeMap::new(),
            preview.review_digest,
        )
        .await?;
    let edited = publish(&dest_store, &dest, edited).await?;
    assert_ne!(
        by_kind(&edited, PackageArtifactKind::Subflow).version.id,
        prior.id
    );
    drop(dest_store);
    let reopened = Store::open(dest_database)?;
    assert_eq!(
        reopened
            .workflow_run(dest.clone(), old.id)
            .await?
            .context("original run missing")?
            .version,
        old.version
    );
    assert_eq!(
        reopened.workflow_definition(dest, old.version).await?,
        Some(definition)
    );
    assert_eq!(grant_count(&reopened).await?, 0);
    Ok(())
}

#[tokio::test]
async fn portable_export_blocks_secret_text_without_changing_the_published_role() -> Result<()> {
    use crate::{
        jobs::JobRole,
        providers::SessionMode,
        workflows::role_templates::{ConversationTemplate, TemplateReviewPolicy},
    };

    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("store.sqlite"))?;
    let checkout = approved_delivery_repository(&store, directory.path()).await?;
    let project = checkout.project_id;
    let with_reference = "Set OPENAI_API_KEY=${OPENAI_API_KEY}.";
    let with_literal = "Set OPENAI_API_KEY=fixture-credential-never-export";
    let role = |name: &str, instructions: &str| {
        member(
            name,
            PackageDestination::Library,
            PackageArtifactBody::Role {
                template: ConversationTemplate {
                    name: name.into(),
                    role: JobRole::Agent,
                    model: ModelSelection {
                        provider: Provider::Codex,
                        model_id: "fixture".into(),
                        profile_id: "local-profile".into(),
                    },
                    effort: None,
                    instructions: instructions.into(),
                    tool_scope: SessionMode::ReadOnly,
                    completion_contract: "Use Bearer authentication for local requests.".into(),
                    review_policy: TemplateReviewPolicy::Independent,
                    requirements: Default::default(),
                },
            },
        )
    };
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Export with credential reference".into(),
                members: vec![
                    role("portable-role", with_reference),
                    role("local-role", with_literal),
                ],
            },
        )
        .await?;
    let published = publish(&store, &project, draft).await?;
    let versions = published
        .members
        .iter()
        .map(|entry| (entry.artifact.name.as_str(), entry.artifact.version.clone()))
        .collect::<BTreeMap<_, _>>();
    let portable = versions["portable-role"].clone();
    let local = versions["local-role"].clone();

    let export = store
        .export_workflow_exchange(project.clone(), vec![portable.clone()])
        .await?;
    assert!(export.sources.values().any(|value| value == &portable));
    let PackageArtifactBody::Role { template } = &export.package.members[0].body else {
        anyhow::bail!("The published portable role is missing");
    };
    assert_eq!(template.instructions, with_reference);
    let error = store
        .export_workflow_exchange(project.clone(), vec![local.clone()])
        .await
        .err()
        .context("Export wrote a literal credential")?;
    assert!(
        error
            .to_string()
            .contains("A workflow export contains a credential")
    );
    assert!(!format!("{error:#}").contains("fixture-credential-never-export"));
    let original = store
        .published_workflow_package_artifact(project, local.clone())
        .await?
        .context("Export changed the local published revision")?;
    assert_eq!(original.version, local);
    assert!(
        matches!(original.body, PackageArtifactBody::Role { template } if template.instructions == with_literal)
    );
    Ok(())
}

fn sample_package(
    command: CommandConfiguration,
    tool: McpToolConfiguration,
) -> Result<WorkflowPackage> {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let plan = graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .context("plan node missing")?;
    let NodeConfiguration::AgentRole { model, .. } = &mut plan.configuration else {
        unreachable!()
    };
    *model = Some(ModelSelection {
        provider: Provider::Codex,
        profile_id: "source-cli-profile-never-export".into(),
        model_id: "same-model".into(),
    });
    graph.nodes.extend([
        node(
            "write-command",
            BuiltinNode::Command,
            NodeConfiguration::Command(command.clone()),
        ),
        node(
            "local-tool",
            BuiltinNode::McpTool,
            NodeConfiguration::McpTool(tool),
        ),
        node("reuse", BuiltinNode::Subflow, NodeConfiguration::None),
    ]);
    graph.wires.extend([
        wire("start", "start", "local-tool", "ready"),
        wire("start", "start", "reuse", "go"),
        wire("start", "document", "reuse", "document"),
        wire("prepare", "prepared", "write-command", "ready"),
        wire("prepare", "release", "write-command", "release"),
        wire("prepare", "worktree", "write-command", "worktree"),
    ]);
    let mut pipeline = member(
        "pipeline",
        PackageDestination::Project,
        PackageArtifactBody::Pipeline { graph },
    );
    pipeline
        .references
        .insert("reuse".into(), reference("subflow"));
    pipeline
        .references
        .insert("write-command".into(), reference("command"));
    let mut subflow = member(
        "subflow",
        PackageDestination::Library,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Portable source subflow".into(),
                nodes: vec![
                    node("start", BuiltinNode::Start, NodeConfiguration::None),
                    node("load", BuiltinNode::LoadContext, NodeConfiguration::None),
                ],
                wires: vec![
                    wire("start", "start", "load", "ready"),
                    wire("start", "document", "load", "document"),
                ],
            },
            inputs: BTreeMap::from([
                ("go".into(), endpoint("start", "start")),
                ("document".into(), endpoint("start", "document")),
            ]),
            outputs: BTreeMap::from([("done".into(), endpoint("load", "loaded"))]),
        },
    );
    subflow
        .references
        .insert("load".into(), reference("preset"));
    let preset = member(
        "preset",
        PackageDestination::Library,
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::LoadContext,
            configuration: NodeConfiguration::None,
            editable_fields: vec![],
        },
    );
    let command = member(
        "command",
        PackageDestination::Project,
        PackageArtifactBody::Command {
            binding: PackageCommandBinding::Registered {
                id: command.registration_id,
                revision: command.registration_revision,
            },
            arguments: command.arguments,
            input: command.input,
            output: command.output,
        },
    );
    Ok(WorkflowPackage {
        name: "Portable workflow closure".into(),
        members: vec![pipeline, preset, subflow, command],
    })
}

fn member(key: &str, destination: PackageDestination, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn reference(key: &str) -> PackageReference {
    PackageReference::Draft { key: key.into() }
}
fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}
fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}
fn wire(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(from, output),
        to: endpoint(to, input),
    }
}

async fn publish(
    store: &Store,
    project: &str,
    draft: WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("the exact local draft did not publish");
    };
    Ok(publication)
}

fn by_kind(
    receipt: &WorkflowPackagePublication,
    kind: PackageArtifactKind,
) -> &PublishedPackageArtifact {
    &receipt
        .members
        .iter()
        .find(|entry| entry.artifact.version.kind == kind)
        .unwrap()
        .artifact
}

fn build_stdio_fixture(target: &Path) -> Result<()> {
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_mcp_tool_server.rs");
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "-C", "opt-level=1"])
        .arg(source)
        .arg("-o")
        .arg(target)
        .output()?;
    ensure!(
        compiled.status.success(),
        "Cannot build MCP fixture: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    Ok(())
}

async fn register_server(
    store: &Store,
    project: &str,
    executable: &Path,
    protected: PathBuf,
    incompatible: bool,
) -> Result<McpServer> {
    let mut arguments = vec![
        protected
            .to_str()
            .context("fixture path is unavailable")?
            .into(),
    ];
    if incompatible {
        arguments.push("--changed-schema".into());
    }
    store
        .register_workflow_mcp_server(
            project.into(),
            McpServerRequest {
                name: "Portable scoped fixture server".into(),
                executable: executable.into(),
                arguments,
                outbound_network: false,
            },
        )
        .await
}

fn tool(server: &McpServer, destination: &str) -> Result<McpToolConfiguration> {
    let contract = &server.tools[0];
    Ok(McpToolConfiguration {
        server_id: server.id.clone(),
        server_revision: server.revision.clone(),
        tool_name: contract.name.clone(),
        contract_revision: contract.revision.clone(),
        arguments: serde_json::from_value(json!({"destination":destination,"action":"mutate"}))?,
        argument_mappings: BTreeMap::new(),
        destination_pointer: Some("/destination".into()),
    })
}

async fn grant_count(store: &Store) -> Result<u32> {
    store
        .execute(|db| {
            Ok(
                db.query_row("SELECT count(*) FROM workflow_mcp_grants", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
}
