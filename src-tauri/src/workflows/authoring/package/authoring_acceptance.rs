use std::{fs, path::Path};

use serde_json::json;

use crate::workflows::{
    BuiltinNode, CommandConfiguration, CommandOutputFormat, DeliveryRoles, Endpoint,
    ManualWorkflowLaunch, NodeConfiguration, NodeInstance, Wire, WorkflowGraph,
    WorkflowLaunchSource, builtin_delivery_graph, tests::approved_delivery_repository,
};

use super::*;

#[tokio::test]
async fn one_package_creates_and_revises_all_six_artifact_types_with_scoped_exact_bases()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("package-authoring.sqlite");
    let store = Store::open(database.clone())?;
    let checkout = approved_delivery_repository(&store, temp.path()).await?;
    let foreign_dir = temp.path().join("foreign");
    fs::create_dir(&foreign_dir)?;
    let foreign = approved_delivery_repository(&store, &foreign_dir)
        .await?
        .project_id;
    let project = checkout.project_id;
    assert_ne!(project, foreign);
    let conversation = store
        .create_conversation(
            checkout.checkout_id.clone(),
            "Review the formatter workflow package".into(),
        )
        .await?;
    let chat = Some(conversation.id);

    let original = formatter_package("original")?;
    assert!(original.members.iter().all(|member| member.base.is_none()));
    let created = store
        .create_workflow_package(project.clone(), chat.clone(), original.clone())
        .await?;
    assert_eq!(&created.package, &original);
    assert_eq!(created.conversation_id, chat);
    assert!(created.publication.is_none());
    let initial = publish(&store, &project, &created).await?;
    assert_eq!(initial.reviewed_revision, created.revision);
    assert_eq!(initial.members.len(), 6);
    for member in &initial.members {
        assert!(member.previous.is_none());
        assert_eq!(member.artifact.version.kind, member.artifact.body.kind());
    }
    assert_draft_dependencies(&initial, "original", None);
    assert_named_bindings(&initial, "original");
    let first_pipeline = &published(&initial, "pipeline").version;
    let first_preset = &published(&initial, "preset").version;
    assert_pins_in_definition(&store, &project, &initial, "original").await?;

    // The workspace's published-artifact picker resolves the exact selected version.
    let mut edits = formatter_package("edited")?;
    let catalog = store.workflow_package_catalog(project.clone()).await?;
    for member in &mut edits.members {
        let expected = &published(&initial, &member.key).version;
        assert!(catalog.iter().any(|entry| &entry.version == expected));
        let base = store
            .published_workflow_package_artifact(project.clone(), expected.clone())
            .await?
            .context("The selected published base is unavailable")?;
        assert_eq!(base.version, *expected);
        assert_ne!(base.body, member.body);
        member.base = Some(base.version);
    }
    edits
        .members
        .iter_mut()
        .find(|member| member.key == "pipeline")
        .unwrap()
        .references
        .insert(
            "plan".into(),
            PackageReference::Published {
                version: first_preset.clone(),
            },
        );
    let blank = store
        .create_workflow_package(
            project.clone(),
            chat.clone(),
            WorkflowPackage {
                name: "Review related formatter edits".into(),
                members: vec![],
            },
        )
        .await?;
    let reviewed = store
        .save_workflow_package(
            project.clone(),
            blank.id.clone(),
            blank.revision.clone(),
            edits.clone(),
        )
        .await?;
    assert_eq!(reviewed.parent_revision.as_ref(), Some(&blank.revision));
    assert_eq!(reviewed.package, edits);
    assert!(
        store
            .save_workflow_package(
                project.clone(),
                blank.id.clone(),
                blank.revision,
                edits.clone()
            )
            .await
            .is_err()
    );
    let concurrent = store
        .create_workflow_package(project.clone(), chat, edits)
        .await?;

    let updated = publish(&store, &project, &reviewed).await?;
    assert_eq!(updated.reviewed_revision, reviewed.revision);
    assert_eq!(updated.members.len(), 6);
    for member in &updated.members {
        let before = published(&initial, &member.key);
        assert_eq!(member.previous.as_ref(), Some(&before.version));
        assert_eq!(member.artifact.version.id, before.version.id);
        assert_eq!(
            member.artifact.version.destination,
            before.version.destination
        );
        assert_ne!(member.artifact.version.revision, before.version.revision);
        assert_ne!(member.artifact.body, before.body);
    }
    assert_draft_dependencies(&updated, "edited", Some(first_preset));
    assert_named_bindings(&updated, "edited");
    assert_pins_in_definition(&store, &project, &updated, "original").await?;
    let head = store.workflow_package_catalog(project.clone()).await?;
    assert_eq!(head.len(), 6);
    assert!(
        head.iter()
            .any(|entry| entry.version == published(&updated, "preset").version)
    );
    assert!(!head.iter().any(|entry| &entry.version == first_preset));
    let conflict = store
        .publish_reviewed_workflow_package(
            project.clone(),
            concurrent.id.clone(),
            concurrent.revision.clone(),
        )
        .await?;
    let WorkflowPackagePublishOutcome::StaleBase { member, current } = conflict else {
        anyhow::bail!("A concurrent package replaced a newer published base");
    };
    assert_eq!(member, "pipeline");
    assert_eq!(current.version, published(&updated, "pipeline").version);
    assert_eq!(
        store
            .workflow_package(project.clone(), concurrent.id.clone())
            .await?,
        concurrent
    );

    let note = store
        .capture_repository_markdown(
            checkout.checkout_id.clone(),
            Path::new(&checkout.checkout_root).join("brief.md"),
        )
        .await?;
    let unavailable = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: workflow_version(published(&updated, "pipeline")),
            source: WorkflowLaunchSource {
                checkout_id: checkout.checkout_id,
                revision_id: note.revision.id,
            },
            request_id: "blocked-formatter-launch".into(),
            work_id: None,
        })
        .await
        .unwrap_err();
    assert!(
        unavailable.to_string().contains("formatter-local-edited"),
        "{unavailable:#}"
    );
    assert!(store.list_workflow_runs(project.clone()).await?.is_empty());

    assert!(
        store
            .workflow_package(foreign.clone(), reviewed.id.clone())
            .await
            .is_err()
    );
    assert!(
        store
            .published_workflow_package_artifact(foreign.clone(), first_pipeline.clone())
            .await?
            .is_none()
    );
    assert_eq!(
        store
            .published_workflow_package_artifact(foreign.clone(), first_preset.clone())
            .await?
            .as_ref(),
        Some(published(&initial, "preset"))
    );
    let foreign_catalog = store.workflow_package_catalog(foreign.clone()).await?;
    assert!(
        foreign_catalog
            .iter()
            .all(|entry| entry.version.destination == PackageDestination::Library)
    );
    reject_foreign_bases_and_references(&store, &project, &foreign).await?;
    assert_eq!(store.workflow_package_catalog(project.clone()).await?, head);

    drop(store);
    let reopened = Store::open(database)?;
    for (draft, publication) in [(created, initial), (reviewed, updated)] {
        let saved = reopened.workflow_package(project.clone(), draft.id).await?;
        assert_eq!(saved.revision, draft.revision);
        assert_eq!(saved.publication.as_ref(), Some(&publication));
        for member in publication.members {
            assert_eq!(
                reopened
                    .published_workflow_package_artifact(
                        project.clone(),
                        member.artifact.version.clone()
                    )
                    .await?,
                Some(member.artifact)
            );
        }
    }
    Ok(())
}

fn formatter_package(edition: &str) -> Result<WorkflowPackage> {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.name = format!("Formatter delivery {edition}");
    graph.nodes.extend([
        node(
            "formatter-context",
            BuiltinNode::Subflow,
            NodeConfiguration::None,
        ),
        node(
            "format",
            BuiltinNode::Command,
            NodeConfiguration::Command(CommandConfiguration {
                registration_id: "unbound-formatter".into(),
                registration_revision: "0".repeat(64),
                arguments: vec![],
                input: Default::default(),
                output: CommandOutputFormat::Text,
            }),
        ),
    ]);
    graph.wires.extend([
        wire("start", "start", "formatter-context", "go"),
        wire("start", "document", "formatter-context", "document"),
        wire("prepare", "prepared", "format", "ready"),
        wire("prepare", "release", "format", "release"),
        wire("prepare", "worktree", "format", "worktree"),
    ]);
    let mut pipeline = member(
        "pipeline",
        PackageDestination::Project,
        PackageArtifactBody::Pipeline { graph },
    );
    pipeline.references = BTreeMap::from([
        ("plan".into(), draft_reference("preset")),
        ("review-plan".into(), draft_reference("role")),
        ("formatter-context".into(), draft_reference("subflow")),
        ("format".into(), draft_reference("command")),
    ]);
    let role = member(
        "role",
        PackageDestination::Project,
        serde_json::from_value(json!({
            "kind":"role", "template": {
                "name":format!("Formatter reviewer {edition}"), "role":"agent",
                "model":{"provider":"codex","profileId":"fixture","modelId":"review-model"},
                "effort":null, "instructions":format!("Review exact paths using the {edition} grading rule."),
                "toolScope":"read_only", "completionContract":"Report path-specific findings.", "reviewPolicy":"human_approval"
            }
        }))?,
    );
    let preset = member(
        "preset",
        PackageDestination::Library,
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::Plan,
            configuration: serde_json::from_value(
                json!({"kind":"agent_role","role":format!("formatter-planner-{edition}")}),
            )?,
            editable_fields: vec!["role".into()],
        },
    );
    let subflow = member(
        "subflow",
        PackageDestination::Library,
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: format!("Formatter context {edition}"),
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
            outputs: BTreeMap::from([
                ("done".into(), endpoint("load", "loaded")),
                ("context".into(), endpoint("load", "context")),
            ]),
        },
    );
    let command = member(
        "command",
        PackageDestination::Project,
        PackageArtifactBody::Command {
            binding: PackageCommandBinding::Required {
                name: format!("formatter-local-{edition}"),
            },
            arguments: vec![format!("--rules={edition}")],
            input: serde_json::from_value(json!({"returnPaths":true}))?,
            output: CommandOutputFormat::Json,
        },
    );
    let mcp = member(
        "mcp",
        PackageDestination::Project,
        PackageArtifactBody::McpTool {
            binding: format!("formatter-mcp-{edition}"),
            tool: "inspect_formatting".into(),
            input_schema: serde_json::from_value(
                json!({"type":"object","required":["relativePath"],
            "properties":{"relativePath":{"type":"string"},"rules":{"type":"string","enum":[edition]}}}),
            )?,
            output_schema: Some(serde_json::from_value(
                json!({"type":"object","properties":{"violations":{"type":"array"}}}),
            )?),
        },
    );
    Ok(WorkflowPackage {
        name: format!("Formatter package {edition}"),
        members: vec![pipeline, role, preset, subflow, command, mcp],
    })
}

fn member(key: &str, destination: PackageDestination, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: format!("Formatter {key}"),
        destination,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn draft_reference(key: &str) -> PackageReference {
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

fn wire(from_node: &str, from_port: &str, to_node: &str, to_port: &str) -> Wire {
    Wire {
        from: endpoint(from_node, from_port),
        to: endpoint(to_node, to_port),
    }
}

async fn publish(
    store: &Store,
    project: &str,
    draft: &WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id.clone(), draft.revision.clone())
        .await?
    else {
        anyhow::bail!("The exact selected package revision did not publish");
    };
    Ok(publication)
}

fn published<'a>(
    receipt: &'a WorkflowPackagePublication,
    key: &str,
) -> &'a PublishedPackageArtifact {
    &receipt
        .members
        .iter()
        .find(|member| member.key == key)
        .unwrap()
        .artifact
}

fn assert_draft_dependencies(
    receipt: &WorkflowPackagePublication,
    edition: &str,
    pinned_preset: Option<&PackageArtifactVersion>,
) {
    let pipeline = published(receipt, "pipeline");
    for (slot, target) in [
        ("plan", "preset"),
        ("review-plan", "role"),
        ("formatter-context", "subflow"),
        ("format", "command"),
    ] {
        let mut version = &published(receipt, target).version;
        if target == "preset"
            && let Some(pinned) = pinned_preset
        {
            version = pinned;
        }
        assert_eq!(
            pipeline.references[slot],
            ResolvedPackageReference::Published {
                version: version.clone()
            }
        );
    }
    assert!(
        pipeline
            .blockers
            .iter()
            .any(|reason| reason.contains(&format!("formatter-local-{edition}")))
    );
}

fn assert_named_bindings(receipt: &WorkflowPackagePublication, edition: &str) {
    let command = published(receipt, "command");
    let name = format!("formatter-local-{edition}");
    assert!(
        matches!(&command.body, PackageArtifactBody::Command { binding: PackageCommandBinding::Required { name: binding }, .. } if binding == &name)
    );
    assert!(command.blockers.iter().any(|reason| reason.contains(&name)));
    let mcp = published(receipt, "mcp");
    let name = format!("formatter-mcp-{edition}");
    assert!(
        matches!(&mcp.body, PackageArtifactBody::McpTool { binding, tool, input_schema, output_schema: Some(_) }
        if binding == &name && tool == "inspect_formatting" && input_schema["properties"]["rules"]["enum"] == json!([edition]))
    );
    assert!(mcp.blockers.iter().any(|reason| reason.contains(&name)));
}

fn workflow_version(artifact: &PublishedPackageArtifact) -> WorkflowRevision {
    WorkflowRevision {
        id: artifact.version.id.clone(),
        revision: artifact.version.revision.clone(),
    }
}

async fn assert_pins_in_definition(
    store: &Store,
    project: &str,
    receipt: &WorkflowPackagePublication,
    expected_preset: &str,
) -> Result<()> {
    let definition = store
        .workflow_definition(
            project.into(),
            workflow_version(published(receipt, "pipeline")),
        )
        .await?
        .context("The published workflow definition is missing")?;
    assert!(definition.nodes.iter().any(|node| node.instance.id == "plan"
        && matches!(&node.instance.configuration, NodeConfiguration::AgentRole { role, .. } if role == &format!("formatter-planner-{expected_preset}"))));
    assert!(
        definition
            .subflows
            .iter()
            .any(|scope| scope.version == published(receipt, "subflow").version)
    );
    Ok(())
}

async fn reject_foreign_bases_and_references(
    store: &Store,
    project: &str,
    foreign: &str,
) -> Result<()> {
    let mut foreign_role = formatter_package("private")?
        .members
        .into_iter()
        .find(|member| member.key == "role")
        .unwrap();
    foreign_role.name = "Private role of the second project".into();
    let draft = store
        .create_workflow_package(
            foreign.into(),
            None,
            WorkflowPackage {
                name: "Private scope".into(),
                members: vec![foreign_role.clone()],
            },
        )
        .await?;
    let receipt = publish(store, foreign, &draft).await?;
    foreign_role.base = Some(published(&receipt, "role").version.clone());
    let foreign_edit = store
        .create_workflow_package(
            project.into(),
            None,
            WorkflowPackage {
                name: "Foreign base test".into(),
                members: vec![foreign_role.clone()],
            },
        )
        .await?;
    assert!(
        store
            .publish_reviewed_workflow_package(
                project.into(),
                foreign_edit.id,
                foreign_edit.revision
            )
            .await
            .is_err()
    );

    let mut reference = formatter_package("rejected")?;
    reference
        .members
        .iter_mut()
        .find(|member| member.key == "pipeline")
        .unwrap()
        .references
        .insert(
            "review-plan".into(),
            PackageReference::Published {
                version: foreign_role
                    .base
                    .context("the foreign fixture role has no version")?,
            },
        );
    let foreign_reference = store
        .create_workflow_package(project.into(), None, reference)
        .await?;
    let error = store
        .publish_reviewed_workflow_package(
            project.into(),
            foreign_reference.id,
            foreign_reference.revision,
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("published reference review-plan is unavailable in this scope"),
        "{error:#}"
    );
    Ok(())
}
