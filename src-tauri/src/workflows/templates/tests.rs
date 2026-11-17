use crate::workflows::tests::approved_delivery_repository;

use super::{
    super::authoring::package::{PackageArtifactKind, WorkflowPackagePublishOutcome},
    *,
};

#[tokio::test]
async fn factory_copies_are_reviewable_and_leave_named_commands_without_local_authority()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("factory.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = &repo.project_id;
    let versions = workflow_factory_templates();
    let library_template = versions
        .iter()
        .find(|entry| entry.id == WorkflowFactoryTemplate::DeliveryLibrary)
        .unwrap();
    assert!(
        store
            .create_workflow_factory_draft(
                project.clone(),
                WorkflowFactoryTemplate::DeliveryLibrary,
                library_template.revision + 1,
            )
            .await
            .is_err()
    );
    let library = store
        .create_workflow_factory_draft(
            project.clone(),
            library_template.id,
            library_template.revision,
        )
        .await?;
    assert!(library.publication.is_none());
    assert!(
        store
            .workflow_package_catalog(project.clone())
            .await?
            .is_empty()
    );
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), library.id, library.revision)
        .await?
    else {
        anyhow::bail!("Expected an explicitly reviewed library");
    };
    assert_eq!(publication.members.len(), 7);
    assert_eq!(
        publication
            .members
            .iter()
            .filter(|item| item.artifact.version.kind == PackageArtifactKind::Preset)
            .count(),
        3
    );
    for label in ["Build", "Lint", "Test"] {
        let published = &publication
            .members
            .iter()
            .find(|member| member.artifact.name == label)
            .unwrap()
            .artifact;
        assert_eq!(published.version.destination, PackageDestination::Library);
        assert_eq!(published.version.kind, PackageArtifactKind::Command);
        assert!(matches!(&published.body, PackageArtifactBody::Command {
            binding: PackageCommandBinding::Required { name }, ..
        } if name == label));
        assert!(!published.blockers.is_empty());
    }
    let flow = publication
        .members
        .into_iter()
        .find(|member| member.artifact.version.kind == PackageArtifactKind::Subflow)
        .unwrap()
        .artifact;
    let metadata = store
        .describe_workflow_reusable_node(project.clone(), flow.version)
        .await?;
    assert_eq!(
        metadata
            .definition
            .outputs
            .iter()
            .map(|port| (port.name.as_str(), port.value))
            .collect::<Vec<_>>(),
        [
            ("checks", PortType::Data(ArtifactKind::Checks)),
            ("complete", PortType::Control),
            ("review", PortType::Data(ArtifactKind::Review))
        ]
    );
    assert!(
        store
            .list_workflow_command_registrations(project.clone())
            .await?
            .is_empty()
    );
    assert!(store.list_workflow_runs(project.clone()).await?.is_empty());
    let delivery = store
        .create_workflow_factory_draft(project.clone(), versions[0].id, versions[0].revision)
        .await?;
    let mut edited = delivery.package;
    edited.name = "Project task delivery".into();
    let saved = store
        .save_workflow_package(
            project.clone(),
            delivery.id,
            delivery.revision.clone(),
            edited,
        )
        .await?;
    assert_eq!(
        saved.parent_revision.as_deref(),
        Some(delivery.revision.as_str())
    );
    assert_eq!(saved.package.name, "Project task delivery");
    assert!(saved.publication.is_none());
    Ok(())
}
