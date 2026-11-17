use std::path::Path;

use serde_json::{Value, json};

use crate::{
    jobs::QueueState,
    workflows::{
        BuiltinNode, Endpoint, ManualWorkflowLaunch, NodeConfiguration, NodeInputs, NodeInstance,
        Wire, WorkflowGraph, WorkflowLaunchSource, WorkflowRevision,
        mcp_tools::{McpToolConfiguration, WorkflowMcpToolSubmission, persist_mcp_binding_fixture},
        tests::approved_delivery_repository,
        triggers::{TriggerWorkKind, WorkflowTriggerEvent, WorkflowTriggerSpec},
    },
};

use super::*;

const MEMBER: &str = "reusable-tool";
const NODE: &str = "invoke-tool";

fn member(key: &str, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn declaration(tool: &Value) -> Result<PackageMember> {
    let mut member = member(
        MEMBER,
        PackageArtifactBody::McpTool {
            binding: "Destination MCP server".into(),
            tool: tool["name"]
                .as_str()
                .context("Missing fixture tool")?
                .into(),
            input_schema: tool["inputSchema"]
                .as_object()
                .context("Missing fixture input")?
                .clone(),
            output_schema: tool.get("outputSchema").and_then(Value::as_object).cloned(),
        },
    );
    member.destination = PackageDestination::Library;
    Ok(member)
}

fn workflow(config: McpToolConfiguration, version: PackageArtifactVersion) -> PackageMember {
    let mut workflow = member(
        "pipeline",
        PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: "Explicit reusable MCP binding".into(),
                nodes: vec![
                    NodeInstance {
                        id: "start".into(),
                        kind: BuiltinNode::Start,
                        configuration: NodeConfiguration::None,
                    },
                    NodeInstance {
                        id: NODE.into(),
                        kind: BuiltinNode::McpTool,
                        configuration: NodeConfiguration::McpTool(config),
                    },
                ],
                wires: vec![Wire {
                    from: Endpoint {
                        node: "start".into(),
                        port: "start".into(),
                    },
                    to: Endpoint {
                        node: NODE.into(),
                        port: "ready".into(),
                    },
                }],
            },
        },
    );
    workflow
        .references
        .insert(NODE.into(), PackageReference::Published { version });
    workflow
}

async fn publish(
    store: &Store,
    project: &str,
    member: PackageMember,
) -> Result<PublishedPackageArtifact> {
    let draft = store
        .create_workflow_package(
            project.into(),
            None,
            WorkflowPackage {
                name: member.name.clone(),
                members: vec![member],
            },
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Expected the reviewed reusable MCP publication");
    };
    Ok(publication
        .members
        .into_iter()
        .next()
        .context("Missing published artifact")?
        .artifact)
}

#[tokio::test]
async fn exact_local_mcp_binding_unblocks_an_explicit_upgrade_and_keeps_grants_project_local()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = Store::open(temporary.path().join("mcp-binding.sqlite"))?;
    let origin = approved_delivery_repository(&store, temporary.path()).await?;
    let target_directory = temporary.path().join("destination");
    std::fs::create_dir(&target_directory)?;
    let target = approved_delivery_repository(&store, &target_directory).await?;
    let target_project = &target.project_id;
    let registered_tool = json!({ "name": "record", "inputSchema": {"type": "object", "required": ["destination"],
        "properties": {"destination": {"type": "string"}}},
        "outputSchema": {"type": "object", "properties": {"recorded": {"type": "boolean"}}} });
    let executable = Path::new(&origin.checkout_root).join("codex.exe");
    let arguments = json!({"destination": "named-local-target"})
        .as_object()
        .unwrap()
        .clone();
    let origin_binding = persist_mcp_binding_fixture(
        &store,
        origin.project_id.clone(),
        executable.clone(),
        registered_tool.clone(),
        arguments.clone(),
        Some("/destination".into()),
    )
    .await?;
    let local_binding = persist_mcp_binding_fixture(
        &store,
        target_project.clone(),
        executable,
        registered_tool.clone(),
        arguments,
        Some("/destination".into()),
    )
    .await?;
    assert_eq!(origin_binding, local_binding);
    store
        .set_workflow_mcp_grant(origin.project_id.clone(), origin_binding, true)
        .await?;

    let library = publish(&store, &origin.project_id, declaration(&registered_tool)?).await?;
    assert_eq!(library.version.destination, PackageDestination::Library);
    assert!(
        library
            .blockers
            .iter()
            .any(|reason| reason.contains("Destination MCP server"))
    );
    let mut missing_binding = local_binding.clone();
    missing_binding.server_id = "00000000-0000-0000-0000-000000000000".into();
    missing_binding.server_revision = "0".repeat(64);
    let library_preset = |config| {
        let mut preset = member(
            "library-preset",
            PackageArtifactBody::Preset {
                node_kind: BuiltinNode::McpTool,
                configuration: NodeConfiguration::McpTool(config),
                editable_fields: Vec::new(),
            },
        );
        preset.destination = PackageDestination::Library;
        preset.references.insert(
            "$preset".into(),
            PackageReference::Parameter {
                name: "Local reusable tool".into(),
                kind: PackageArtifactKind::McpTool,
            },
        );
        preset
    };
    let error = publish(
        &store,
        target_project,
        library_preset(local_binding.clone()),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("without a project server pin"));
    let portable = publish(
        &store,
        target_project,
        library_preset(missing_binding.clone()),
    )
    .await?;
    let PackageArtifactBody::Preset {
        configuration: NodeConfiguration::McpTool(placeholder),
        ..
    } = portable.body
    else {
        anyhow::bail!("The portable preset lost its binding parameter");
    };
    assert!(placeholder.is_package_placeholder());
    assert!(
        portable
            .blockers
            .iter()
            .any(|reason| reason.contains("Local reusable tool"))
    );
    let blocked = publish(
        &store,
        target_project,
        workflow(missing_binding, library.version.clone()),
    )
    .await?;
    assert!(
        blocked
            .blockers
            .iter()
            .any(|reason| reason.contains("Destination MCP server"))
    );
    let note = store
        .capture_repository_markdown(
            target.checkout_id.clone(),
            Path::new(&target.checkout_root).join("brief.md"),
        )
        .await?;
    let launch = |version: &PackageArtifactVersion, request_id: &str| ManualWorkflowLaunch {
        project_id: target_project.clone(),
        workflow: WorkflowRevision {
            id: version.id.clone(),
            revision: version.revision.clone(),
        },
        source: WorkflowLaunchSource {
            checkout_id: target.checkout_id.clone(),
            revision_id: note.revision.id.clone(),
        },
        request_id: request_id.into(),
        work_id: None,
    };
    let error = store
        .launch_workflow_manual(launch(&blocked.version, "blocked-original"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Destination MCP server"));
    let trigger_spec = |version: &PackageArtifactVersion| {
        let selected = launch(version, "unused-on-trigger");
        WorkflowTriggerSpec {
            name: "Use selected reusable MCP revision".into(),
            enabled: true,
            event: WorkflowTriggerEvent::Created,
            work: TriggerWorkKind::Task,
            target_state: None,
            pipeline: selected.workflow,
            source: selected.source.into(),
        }
    };
    let rule = store
        .save_workflow_trigger_rule(
            target_project.clone(),
            None,
            None,
            trigger_spec(&blocked.version),
        )
        .await?;

    let mut altered_tool = registered_tool.clone();
    altered_tool["outputSchema"]["properties"]["recorded"]["type"] = json!("string");
    let mismatch = publish(&store, &origin.project_id, declaration(&altered_tool)?).await?;
    let error = publish(
        &store,
        target_project,
        workflow(local_binding.clone(), mismatch.version),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("schema differs from the reusable tool declaration"));

    let mut upgrade = workflow(local_binding.clone(), library.version.clone());
    upgrade.base = Some(blocked.version.clone());
    let upgrade_draft = store
        .create_workflow_package(
            target_project.clone(),
            None,
            WorkflowPackage {
                name: upgrade.name.clone(),
                members: vec![upgrade],
            },
        )
        .await?;
    let preview = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                target_project.clone(),
                upgrade_draft.id.clone(),
                upgrade_draft.revision.clone(),
            )
            .await?,
    )?;
    assert_eq!(preview["members"][0]["requirements"], json!([]));
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(
            target_project.clone(),
            upgrade_draft.id,
            upgrade_draft.revision,
        )
        .await?
    else {
        anyhow::bail!("Expected the explicit local binding upgrade");
    };
    let ready = publication
        .members
        .into_iter()
        .next()
        .context("Missing upgraded workflow")?
        .artifact;
    assert!(ready.blockers.is_empty(), "{:?}", ready.blockers);
    assert_eq!(ready.version.id, blocked.version.id);
    assert_ne!(ready.version.revision, blocked.version.revision);
    let legacy_revision = crate::workflows::digest(&serde_json::to_vec(&(
        &ready.name,
        &ready.body,
        &ready.references,
        &ready.definitions,
    ))?);
    assert_ne!(
        ready.version.revision, legacy_revision,
        "The corrected binding needs an executable revision distinct from the previous digest"
    );
    assert_eq!(
        ready.references.get(NODE),
        Some(&ResolvedPackageReference::Published {
            version: library.version.clone()
        })
    );
    assert!(
        store
            .launch_workflow_manual(launch(&blocked.version, "still-blocked-old-pin"))
            .await
            .is_err()
    );
    let task = store
        .create_project_task(
            target_project.clone(),
            "Still using the original pin".into(),
        )
        .await?;
    let activations = store
        .workflow_trigger_activations(target_project.clone())
        .await?;
    let old_activation = activations
        .iter()
        .find(|activation| activation.work_id == task.id)
        .context("Missing original rule outcome")?;
    assert_eq!(old_activation.outcome, "failed");
    assert_eq!(old_activation.rule_revision, rule.revision);
    assert!(
        old_activation
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Destination MCP server"))
    );
    let retargeted = store
        .save_workflow_trigger_rule(
            target_project.clone(),
            Some(rule.id),
            Some(rule.revision),
            trigger_spec(&ready.version),
        )
        .await?;
    let task = store
        .create_project_task(
            target_project.clone(),
            "Use the explicitly selected revision".into(),
        )
        .await?;
    let activations = store
        .workflow_trigger_activations(target_project.clone())
        .await?;
    let new_activation = activations
        .iter()
        .find(|activation| activation.work_id == task.id)
        .context("Missing revised rule outcome")?;
    assert_eq!(new_activation.outcome, "launched");
    assert_eq!(new_activation.rule_revision, retargeted.revision);
    let launch_receipt = store
        .workflow_launch(
            target_project.clone(),
            new_activation
                .run_id
                .clone()
                .context("The revised rule missed its run")?,
        )
        .await?
        .context("Missing rule launch receipt")?;
    assert_eq!(launch_receipt.version.revision, ready.version.revision);
    let run = store
        .launch_workflow_manual(launch(&ready.version, "locally-bound"))
        .await?;
    let inspection = store
        .inspect_workflow_execution(target_project.clone(), run.id.clone())
        .await?
        .context("Missing bound execution")?;
    let node = inspection
        .definition
        .nodes
        .iter()
        .find(|node| node.instance.id == NODE)
        .context("Missing reusable MCP node")?;
    assert_eq!(
        node.instance.configuration,
        NodeConfiguration::McpTool(local_binding.clone())
    );
    let submitted = store
        .submit_workflow_mcp_tool(WorkflowMcpToolSubmission {
            project_id: target_project.clone(),
            run_id: run.id,
            node_id: NODE.into(),
            checkout_id: target.checkout_id.clone(),
            inputs: NodeInputs::new(),
        })
        .await?;
    assert_eq!(submitted.queue_state, QueueState::Paused);
    let review = store
        .workflow_mcp_job_review(target_project.clone(), submitted.id)
        .await?;
    assert_eq!(review.decision, "pending");
    assert_eq!(review.destination.as_deref(), Some("named-local-target"));
    assert_eq!(
        store
            .published_workflow_package_artifact(target_project.clone(), library.version.clone())
            .await?,
        Some(library)
    );

    store
        .set_workflow_mcp_grant(target_project.clone(), local_binding, true)
        .await?;
    let authorized_run = store
        .launch_workflow_manual(launch(&ready.version, "locally-authorized"))
        .await?;
    let authorized = store
        .submit_workflow_mcp_tool(WorkflowMcpToolSubmission {
            project_id: target_project.clone(),
            run_id: authorized_run.id,
            node_id: NODE.into(),
            checkout_id: target.checkout_id,
            inputs: NodeInputs::new(),
        })
        .await?;
    let review = store
        .workflow_mcp_job_review(target_project.clone(), authorized.id)
        .await?;
    assert_eq!(review.decision, "preauthorized");
    Ok(())
}
