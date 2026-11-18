use std::path::Path;

use serde_json::json;

use crate::workflows::{
    ArtifactKind, BuiltinNode, CommandConfiguration, CommandOutputFormat, ConditionPredicate,
    DeliveryRoles, Endpoint, NodeConfiguration, NodeInstance, Port, PortType, Wire, WorkflowGraph,
    builtin_delivery_graph, mcp_tools::McpToolConfiguration, tests::approved_delivery_repository,
};

use super::*;

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn wire(source: &str, output: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(source, output),
        to: endpoint(target, input),
    }
}

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

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

async fn production_counts(store: &Store) -> Result<(u64, Vec<i64>)> {
    store
        .execute(|db| {
            Ok((
                db.total_changes(),
                [
                    "jobs",
                    "job_attempts",
                    "job_pipeline_runs",
                    "job_pipeline_releases",
                    "job_approvals",
                    "managed_task_worktrees",
                    "project_tasks",
                    "workflow_command_results",
                    "workflow_package_publications",
                    "workflow_artifacts",
                ]
                .into_iter()
                .map(|table| {
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?,
            ))
        })
        .await
}

#[tokio::test]
async fn previews_the_saved_field_diff_and_exact_published_consumers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = Store::open(temp.path().join("review.sqlite"))?;
    let project = approved_delivery_repository(&store, temp.path())
        .await?
        .project_id;
    let command = |arguments: Vec<String>| PackageArtifactBody::Command {
        binding: PackageCommandBinding::Required {
            name: "Repository checker".into(),
        },
        arguments,
        input: Default::default(),
        output: CommandOutputFormat::Json,
    };
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.push(node(
        "check",
        BuiltinNode::Command,
        NodeConfiguration::Command(CommandConfiguration {
            registration_id: "required".into(),
            registration_revision: "0".repeat(64),
            arguments: Vec::new(),
            input: Default::default(),
            output: CommandOutputFormat::Json,
        }),
    ));
    graph.wires.extend([
        wire("prepare", "prepared", "check", "ready"),
        wire("prepare", "release", "check", "release"),
        wire("prepare", "worktree", "check", "worktree"),
    ]);
    let mut pipeline = member("pipeline", PackageArtifactBody::Pipeline { graph });
    pipeline.references.insert(
        "check".into(),
        PackageReference::Draft {
            key: "checker".into(),
        },
    );
    let current = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Original checker".into(),
                members: vec![pipeline, member("checker", command(vec!["check".into()]))],
            },
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), current.id, current.revision)
        .await?
    else {
        anyhow::bail!("The initial package was not published");
    };
    let base = publication
        .members
        .iter()
        .find(|member| member.key == "checker")
        .context("Missing checker")?
        .artifact
        .version
        .clone();
    let consumer = publication
        .members
        .iter()
        .find(|member| member.key == "pipeline")
        .context("Missing pipeline")?
        .artifact
        .version
        .clone();
    let change = |argument: &str| {
        let mut changed = member("checker", command(vec!["check".into(), argument.into()]));
        changed.base = Some(base.clone());
        WorkflowPackage {
            name: "Checker revision".into(),
            members: vec![changed],
        }
    };
    let draft = store
        .create_workflow_package(project.clone(), None, change("--json"))
        .await?;
    let concurrent = store
        .create_workflow_package(project.clone(), None, change("--text"))
        .await?;
    let before = production_counts(&store).await?;
    let review = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone(),
            )
            .await?,
    )?;
    assert_eq!(
        review["members"][0]["affectedUses"][0]["consumer"],
        serde_json::to_value(&consumer)?
    );
    assert_eq!(
        review["members"][0]["affectedUses"][0]["pinnedRevision"],
        base.revision
    );
    assert!(
        review["members"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["path"] == "/body/arguments"
                && field["before"] == json!(["check"])
                && field["after"] == json!(["check", "--json"]))
    );
    assert_eq!(production_counts(&store).await?, before);
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), concurrent.id, concurrent.revision)
        .await?
    else {
        anyhow::bail!("The concurrent revision was not published");
    };
    let new_head = &publication.members[0].artifact.version.revision;
    let review = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone(),
            )
            .await?,
    )?;
    assert!(
        review["members"][0]["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains(new_head)
    );
    assert_eq!(
        review["members"][0]["affectedUses"][0]["pinnedRevision"],
        base.revision
    );
    assert!(matches!(
        store
            .publish_reviewed_workflow_package(project, draft.id, draft.revision)
            .await?,
        WorkflowPackagePublishOutcome::StaleBase { .. }
    ));
    Ok(())
}

fn port(name: &str, value: PortType) -> Port {
    Port {
        name: name.into(),
        value,
    }
}

fn conditional_package() -> WorkflowPackage {
    let mut outer = builtin_delivery_graph(DeliveryRoles::default());
    outer.nodes.retain(|node| {
        ["start", "context", "plan", "review-plan", "work"].contains(&node.id.as_str())
    });
    outer
        .wires
        .retain(|wire| outer.nodes.iter().any(|node| node.id == wire.to.node));
    outer.nodes.push(node(
        "delivery",
        BuiltinNode::Subflow,
        NodeConfiguration::None,
    ));
    outer.wires.extend([
        wire("work", "linked", "delivery", "ready"),
        wire("work", "task", "delivery", "task"),
        wire("plan", "plan", "delivery", "plan"),
    ]);

    let mut pipeline = member("pipeline", PackageArtifactBody::Pipeline { graph: outer });
    pipeline.references.insert(
        "delivery".into(),
        PackageReference::Draft {
            key: "reviewed-delivery".into(),
        },
    );
    let graph = WorkflowGraph {
        name: "Conditional delivery".into(),
        nodes: vec![
            node(
                "entry",
                BuiltinNode::Start,
                NodeConfiguration::SubflowEntry {
                    outputs: vec![
                        port("ready", PortType::Control),
                        port("plan", PortType::Data(ArtifactKind::Plan)),
                        port("task", PortType::Data(ArtifactKind::Task)),
                    ],
                },
            ),
            node(
                "select",
                BuiltinNode::Condition,
                NodeConfiguration::Condition {
                    predicate: ConditionPredicate::Equals {
                        pointer: "/evidence/taskId".into(),
                        value: json!("sample-task"),
                    },
                },
            ),
            node(
                "approval",
                BuiltinNode::ApproveImplementation,
                NodeConfiguration::None,
            ),
            node(
                "prepare",
                BuiltinNode::PrepareWorktree,
                NodeConfiguration::None,
            ),
            node(
                "check",
                BuiltinNode::Command,
                NodeConfiguration::Command(CommandConfiguration {
                    registration_id: "sample-only".into(),
                    registration_revision: "0".repeat(64),
                    arguments: Vec::new(),
                    input: Default::default(),
                    output: CommandOutputFormat::Json,
                }),
            ),
            node(
                "inspect",
                BuiltinNode::McpTool,
                NodeConfiguration::McpTool(McpToolConfiguration {
                    server_id: "00000000-0000-0000-0000-000000000000".into(),
                    server_revision: "0".repeat(64),
                    tool_name: "inspect_file".into(),
                    contract_revision: "1".repeat(64),
                    arguments: Default::default(),
                    argument_mappings: Default::default(),
                    destination_pointer: None,
                }),
            ),
            node("alternate", BuiltinNode::Parallel, NodeConfiguration::None),
            node("diagnostic", BuiltinNode::Parallel, NodeConfiguration::None),
        ],
        wires: vec![
            wire("entry", "ready", "select", "ready"),
            wire("entry", "task", "select", "value"),
            wire("select", "matched", "approval", "ready"),
            wire("entry", "task", "approval", "task"),
            wire("entry", "plan", "approval", "plan"),
            wire("approval", "approved", "prepare", "ready"),
            wire("entry", "task", "prepare", "task"),
            wire("approval", "decision", "prepare", "decision"),
            wire("prepare", "prepared", "check", "ready"),
            wire("prepare", "release", "check", "release"),
            wire("prepare", "worktree", "check", "worktree"),
            wire("check", "completed", "inspect", "ready"),
            wire("check", "failed", "diagnostic", "ready"),
            wire("select", "unmatched", "alternate", "ready"),
        ],
    };
    let child = member(
        "reviewed-delivery",
        PackageArtifactBody::Subflow {
            graph,
            inputs: ["ready", "plan", "task"]
                .into_iter()
                .map(|name| (name.into(), endpoint("entry", name)))
                .collect(),
            outputs: BTreeMap::from([
                ("test-result".into(), endpoint("check", "result")),
                ("inspection".into(), endpoint("inspect", "result")),
            ]),
        },
    );
    let tool = member(
        "local-mcp",
        PackageArtifactBody::McpTool {
            binding: "fixture-server".into(),
            tool: "inspect_file".into(),
            input_schema: json!({"type":"object", "properties":{"path":{"type":"string"}}})
                .as_object()
                .unwrap()
                .clone(),
            output_schema: None,
        },
    );
    WorkflowPackage {
        name: "Review before publication".into(),
        members: vec![pipeline, child, tool],
    }
}

#[tokio::test]
async fn reviews_the_saved_subflow_interface_and_keeps_old_revisions_after_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("subflow-review.sqlite");
    let store = Store::open(db.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id;
    let original = store
        .create_workflow_package(project.clone(), None, conditional_package())
        .await?;
    let draft = original.id;
    let revision = original.revision;
    let before = production_counts(&store).await?;
    let file = Path::new(&repo.checkout_root).join("brief.md");
    let content = std::fs::read(&file)?;
    let preview = store
        .preview_workflow_package_revision(project.clone(), draft.clone(), revision.clone())
        .await?;
    let preview = serde_json::to_value(preview)?;
    assert_eq!(preview["members"][0]["diagnostics"], json!([]));
    let authored = &preview["members"][0]["graphDefinitions"]["delivery"];
    assert!(authored["inputs"].as_array().is_some_and(|ports| {
        ports
            .iter()
            .any(|port| port["name"] == "plan" && port["value"]["artifact"] == "plan")
    }));
    assert!(
        authored["outputs"].as_array().is_some_and(|ports| ports
            .iter()
            .any(|port| port["name"] == "test-result"
                && port["value"]["artifact"] == "command_result"))
    );
    assert!(
        preview["members"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["path"] == "/body/graph/nodes/delivery")
    );
    assert_eq!(
        preview["members"][0]["dependencies"]["delivery"]["key"],
        "reviewed-delivery"
    );
    assert!(
        store
            .preview_workflow_package_revision("unrelated".into(), draft.clone(), revision.clone())
            .await
            .is_err()
    );
    assert_eq!(production_counts(&store).await?, before);
    assert_eq!(std::fs::read(file)?, content);

    let mut changed = conditional_package();
    changed.members[0].references.insert(
        "delivery".into(),
        PackageReference::Draft {
            key: "unavailable".into(),
        },
    );
    let mut concurrent = changed.clone();
    concurrent.name = "Concurrent package edit".into();
    let (first, second) = tokio::join!(
        store.save_workflow_package(project.clone(), draft.clone(), revision.clone(), changed),
        store.save_workflow_package(project.clone(), draft.clone(), revision.clone(), concurrent),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let next = first.or(second)?;
    let next_preview = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                project.clone(),
                draft.clone(),
                next.revision.clone(),
            )
            .await?,
    )?;
    assert!(
        next_preview["members"][0]["diagnostics"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
    );
    assert_eq!(next_preview["revisions"], json!([next.revision, revision]));
    drop(store);
    let reopened = Store::open(db)?;
    let retained = serde_json::to_value(
        reopened
            .preview_workflow_package_revision(project, draft, revision)
            .await?,
    )?;
    assert_eq!(retained["members"], preview["members"]);
    Ok(())
}
