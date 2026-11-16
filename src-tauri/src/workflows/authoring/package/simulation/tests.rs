use crate::workflows::{
    CommandConfiguration, DeliveryRoles, NodeInstance, Wire, builtin_delivery_graph,
    tests::approved_delivery_repository,
};

use super::*;

pub(super) fn endpoint(node: &str, port: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: port.into(),
    }
}

pub(super) fn wire(source: &str, output: &str, target: &str, input: &str) -> Wire {
    Wire {
        from: endpoint(source, output),
        to: endpoint(target, input),
    }
}

pub(super) fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

pub(super) fn member(key: &str, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

pub(super) async fn production_counts(store: &Store) -> Result<(u64, Vec<i64>)> {
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
