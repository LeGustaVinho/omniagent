use std::path::Path;

use crate::{
    store::Store,
    workflows::{
        ArtifactKind, ConditionPredicate, ManualWorkflowLaunch, WorkflowLaunchSource,
        WorkflowRevision, tests::approved_delivery_repository,
    },
};
use serde_json::json;

use super::{
    super::{
        PackageArtifactVersion, WorkflowPackage, WorkflowPackageDraft, WorkflowPackagePublication,
        WorkflowPackagePublishOutcome,
    },
    *,
};

#[tokio::test]
async fn derives_scoped_exact_revisions_and_publishes_only_typed_editable_overrides() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("derive.sqlite");
    let store = Store::open(database.clone())?;
    let repository = approved_delivery_repository(&store, directory.path()).await?;
    let project = &repository.project_id;
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![pipeline(
                "saved",
                BuiltinNode::Condition,
                condition("source.md"),
                None,
            )]),
        )
        .await?;

    let authored_preset = store
        .derive_workflow_reusable(
            project.clone(),
            on_revision(&draft, "saved"),
            preset("condition"),
        )
        .await?;
    assert_eq!(authored_preset.id, draft.id);
    assert_eq!(
        authored_preset.parent_revision.as_deref(),
        Some(draft.revision.as_str())
    );
    assert!(
        store
            .derive_workflow_reusable(
                project.clone(),
                on_revision(&draft, "saved"),
                preset("stale")
            )
            .await
            .is_err()
    );
    assert!(
        store
            .describe_workflow_reusable_boundary(
                "other-project".into(),
                on_revision(&draft, "saved"),
                None,
            )
            .await
            .is_err()
    );

    let boundary = store
        .describe_workflow_reusable_boundary(project.clone(), on_revision(&draft, "saved"), None)
        .await?;
    assert_eq!(boundary.inputs.len(), 2);
    let authored_subflow = store
        .derive_workflow_reusable(
            project.clone(),
            on_revision(&authored_preset, "saved"),
            subflow("condition-flow"),
        )
        .await?;
    assert_eq!(authored_subflow.package.members.len(), 3);
    let source = publish(&store, project, authored_subflow).await?;
    let source_version = version(&source, "saved");
    let preset_version = version(&source, "condition");
    let child_version = version(&source, "condition-flow");
    let preset_info = store
        .describe_workflow_reusable_node(project.clone(), preset_version.clone())
        .await?;
    assert_eq!(preset_info.editable_overrides, ["predicate"]);
    let child_info = store
        .describe_workflow_reusable_node(project.clone(), child_version.clone())
        .await?;
    assert!(
        child_info.definition.inputs.iter().any(
            |port| port.name == "value" && port.value == PortType::Data(ArtifactKind::Markdown)
        )
    );
    assert!(
        child_info
            .definition
            .outputs
            .iter()
            .any(|port| port.name == "done" && port.value == PortType::Control)
    );
    assert_eq!(
        child_info.graph_definitions["node"].kind,
        BuiltinNode::Condition
    );

    let standalone = store
        .derive_workflow_reusable(
            project.clone(),
            ReusableSource::Published {
                version: source_version.clone(),
            },
            preset("from-published"),
        )
        .await?;
    assert_ne!(standalone.id, draft.id);
    assert!(standalone.publication.is_none());
    assert_eq!(standalone.package.members.len(), 1);
    assert_eq!(
        store
            .workflow_package(project.clone(), draft.id.clone())
            .await?
            .package
            .members
            .len(),
        3
    );

    for unsafe_fields in [
        json!({"registrationId":"foreign-local-executable"}),
        json!({"predicate":{"op":"exists","pointer":5}}),
    ] {
        let invalid = store
            .create_workflow_package(
                project.clone(),
                None,
                package(vec![pipeline(
                    "blocked",
                    BuiltinNode::Condition,
                    overrides(unsafe_fields)?,
                    Some(preset_version.clone()),
                )]),
            )
            .await?;
        assert!(
            store
                .publish_reviewed_workflow_package(
                    project.clone(),
                    invalid.id.clone(),
                    invalid.revision.clone(),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .workflow_package(project.clone(), invalid.id)
                .await?
                .publication
                .is_none()
        );
    }

    let approved = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![
                pipeline(
                    "explicit",
                    BuiltinNode::Condition,
                    overrides(json!({"predicate": {
                        "op":"equals","pointer":"/evidence/relativePath","value":"brief.md"
                    }}))?,
                    Some(preset_version.clone()),
                ),
                pipeline(
                    "legacy",
                    BuiltinNode::Condition,
                    condition("ignored.md"),
                    Some(preset_version),
                ),
                pipeline(
                    "parent",
                    BuiltinNode::Subflow,
                    NodeConfiguration::None,
                    Some(child_version.clone()),
                ),
            ]),
        )
        .await?;
    let approved = publish(&store, project, approved).await?;
    let run_version = |key| {
        let package = version(&approved, key);
        WorkflowRevision {
            id: package.id,
            revision: package.revision,
        }
    };
    let derived = store
        .workflow_definition(project.clone(), run_version("explicit"))
        .await?
        .context("Missing explicit workflow")?;
    let legacy = store
        .workflow_definition(project.clone(), run_version("legacy"))
        .await?
        .context("Missing legacy workflow")?;
    assert_eq!(
        derived.nodes[1].instance.configuration,
        condition("brief.md")
    );
    assert_eq!(
        legacy.nodes[1].instance.configuration,
        condition("source.md")
    );

    let note = store
        .capture_repository_markdown(
            repository.checkout_id.clone(),
            Path::new(&repository.checkout_root).join("brief.md"),
        )
        .await?;
    let parent = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: run_version("parent"),
            source: WorkflowLaunchSource {
                checkout_id: repository.checkout_id,
                revision_id: note.revision.id,
            },
            request_id: "reusable-node-acceptance".into(),
            work_id: None,
        })
        .await?;
    drop(store);
    let reopened = Store::open(database)?;
    let execution = reopened
        .inspect_workflow_execution(project.clone(), parent.id)
        .await?
        .context("Missing persisted parent execution")?;
    assert_eq!(execution.definition.subflows[0].version, child_version);
    assert!(
        execution.definition.subflows[0]
            .nodes
            .iter()
            .any(|node| node == "node::node")
    );
    assert!(
        execution
            .control
            .iter()
            .any(|control| control.node_id == "node::node")
    );
    Ok(())
}

fn on_revision(draft: &WorkflowPackageDraft, key: &str) -> ReusableSource {
    ReusableSource::PackageRevision {
        draft_id: draft.id.clone(),
        revision: draft.revision.clone(),
        member_key: key.into(),
    }
}

fn preset(key: &str) -> ReusableDerivation {
    ReusableDerivation {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Library,
        selection: ReusableSelection::Preset {
            node_id: "node".into(),
            editable_fields: vec!["predicate".into()],
        },
    }
}

fn subflow(key: &str) -> ReusableDerivation {
    ReusableDerivation {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Library,
        selection: ReusableSelection::Subflow {
            node_ids: None,
            inputs: BTreeMap::from([
                ("ready".into(), vec![link("start", "ready")]),
                ("value".into(), vec![link("document", "value")]),
            ]),
            outputs: BTreeMap::from([("done".into(), endpoint("node", "matched"))]),
        },
    }
}

fn condition(path: &str) -> NodeConfiguration {
    NodeConfiguration::Condition {
        predicate: ConditionPredicate::Equals {
            pointer: "/evidence/relativePath".into(),
            value: json!(path),
        },
    }
}

fn overrides(fields: serde_json::Value) -> Result<NodeConfiguration> {
    Ok(NodeConfiguration::PresetOverrides {
        fields: serde_json::from_value(fields)?,
    })
}

fn link(output: &str, input: &str) -> Wire {
    Wire {
        from: endpoint("start", output),
        to: endpoint("node", input),
    }
}

fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

fn package(members: Vec<PackageMember>) -> WorkflowPackage {
    WorkflowPackage {
        name: "Reusable workflow acceptance".into(),
        members,
    }
}

fn pipeline(
    key: &str,
    kind: BuiltinNode,
    configuration: NodeConfiguration,
    reference: Option<PackageArtifactVersion>,
) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body: PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: key.into(),
                nodes: vec![
                    NodeInstance {
                        id: "start".into(),
                        kind: BuiltinNode::Start,
                        configuration: NodeConfiguration::None,
                    },
                    NodeInstance {
                        id: "node".into(),
                        kind,
                        configuration,
                    },
                ],
                wires: vec![link("start", "ready"), link("document", "value")],
            },
        },
        references: reference
            .into_iter()
            .map(|version| ("node".into(), PackageReference::Published { version }))
            .collect(),
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
        anyhow::bail!("Expected a reviewed publication");
    };
    Ok(publication)
}

fn version(publication: &WorkflowPackagePublication, key: &str) -> PackageArtifactVersion {
    publication
        .members
        .iter()
        .find(|entry| entry.key == key)
        .unwrap()
        .artifact
        .version
        .clone()
}
