use serde_json::{Value, json};

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

#[tokio::test]
async fn super_ai_audit_uses_verified_remediation_sha_for_the_next_round() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("super-ai-audit.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id;
    let draft = store
        .create_workflow_factory_draft(
            project.clone(),
            WorkflowFactoryTemplate::SuperAiDevelopmentAudit,
            FACTORY_REVISION,
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(
            project.clone(),
            draft.id.clone(),
            draft.revision.clone(),
        )
        .await?
    else {
        anyhow::bail!("Expected an explicitly reviewed integration audit");
    };
    let root = version_of(&publication, "planning");
    let definition = store
        .workflow_definition(project.clone(), root)
        .await?
        .unwrap();
    let trace = store
        .simulate_workflow_package_revision(
            project,
            draft.id,
            draft.revision,
            "planning".into(),
            serde_json::from_value(json!({
                "inputs": { "inputs": { "prompt": "Add a documented export command" } },
                "outcomes": {}
            }))?,
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "The {}-node published audit must be simulatable: {error:#}",
                definition.nodes.len()
            )
        })?;
    let trace = serde_json::to_value(trace)?;
    let steps = trace["steps"].as_array().expect("simulation steps");
    let agent = steps
        .iter()
        .find(|step| step["nodeId"] == "analyze-repository-agent")
        .expect("repository analyst in simulation");
    assert_eq!(agent["state"], Value::String("waiting".into()));
    for port in ["design", "strategy"] {
        let source = endpoint("specifications::stop", port);
        assert!(definition.wires.iter().any(|wire| {
            wire.from == source && wire.to == endpoint("materialize-backlog", port)
        }));
        for round in 1..=4 {
            assert!(definition.wires.iter().any(|wire| {
                wire.from == source
                    && wire.to
                        == endpoint(
                            &format!("integration-audit-{round}::remediation-backlog"),
                            port,
                        )
            }));
        }
    }
    for (round, source) in [
        (1, "deliver-backlog"),
        (2, "integration-audit-1::remediate"),
        (3, "integration-audit-2::remediate"),
        (4, "integration-audit-3::remediate"),
    ] {
        let docs = format!("integration-audit-{round}::documentation-changes");
        let build = format!("integration-audit-{round}-build");
        assert!(definition.wires.iter().any(|wire| {
            wire.from == endpoint(&docs, "unmatched") && wire.to == endpoint(&build, "ready")
        }));
        assert!(definition.wires.iter().any(|wire| {
            wire.from == endpoint(&docs, "matched")
                && wire.to
                    == endpoint(
                        &format!("integration-audit-{round}::remediation-needed"),
                        "branches",
                    )
        }));
        for target in [
            format!("integration-audit-{round}::regression"),
            format!("integration-audit-{round}-build"),
        ] {
            assert!(
                definition.wires.iter().any(|wire| {
                    wire.from == endpoint(source, "integrated")
                        && wire.to == endpoint(&target, "integrated")
                }),
                "{target} must consume the exact integrated SHA"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn super_ai_delivery_pins_the_reviewed_child_and_same_tree_build() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("super-ai-delivery.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id;
    let draft = store
        .create_workflow_factory_draft(
            project.clone(),
            WorkflowFactoryTemplate::SuperAiDevelopmentDelivery,
            FACTORY_REVISION,
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Expected an explicitly reviewed delivery template");
    };
    let root = version_of(&publication, "planning");
    let child = version_of(&publication, "task-delivery");
    let handoff = version_of(&publication, "pull-request-attempt");
    let definition = store
        .workflow_definition(project.clone(), root)
        .await?
        .unwrap();
    let dispatch = definition
        .nodes
        .iter()
        .find(|node| node.instance.kind == BuiltinNode::RunBacklog)
        .unwrap();
    let NodeConfiguration::RunBacklog(dispatch) = &dispatch.instance.configuration else {
        anyhow::bail!("Expected a pinned task workflow");
    };
    assert_eq!(dispatch.child, child);
    let child = store.workflow_definition(project, child).await?.unwrap();
    for round in 1..=3 {
        let attempt = format!("pull-request-{round}");
        let pinned = child
            .subflows
            .iter()
            .find(|flow| flow.path == attempt)
            .unwrap();
        assert_eq!(pinned.version.id, handoff.id);
        assert_eq!(pinned.version.revision, handoff.revision);
        assert!(child.wires.iter().any(|wire| {
            wire.from == endpoint("verify-commit", "git")
                && wire.to == endpoint(&format!("{attempt}::open-pr"), "git")
        }));
        assert!(child.wires.iter().any(|wire| {
            wire.from == endpoint(&format!("{attempt}::wait-for-ci"), "ci")
                && wire.to == endpoint(&format!("{attempt}::approve-high-risk-merge"), "evidence")
        }));
        let scope = format!("implementation::iteration-{round}::");
        let reviews = child
            .nodes
            .iter()
            .filter(|node| {
                node.instance.id.starts_with(&scope)
                    && node.instance.kind == BuiltinNode::ReviewCandidate
            })
            .count();
        assert_eq!(reviews, 5);
        assert!(child.wires.iter().any(|wire| {
            wire.from == endpoint(&format!("{scope}quality-gate"), "passed")
                && wire.to == endpoint(&format!("{scope}build-approved-tree"), "ready")
        }));
        assert!(child.wires.iter().any(|wire| {
            wire.from == endpoint("approved-test-strategy", "object")
                && wire.to == endpoint(&format!("{scope}verify-quality"), "strategy")
        }));
        let build = format!("{scope}build-approved-tree");
        let build = child
            .nodes
            .iter()
            .find(|node| node.instance.id == build)
            .unwrap();
        assert!(matches!(
            &build.instance.configuration,
            NodeConfiguration::RequiredChecks { names, .. } if names == &["Pipeline build"]
        ));
    }
    Ok(())
}

fn version_of(
    publication: &super::super::authoring::package::WorkflowPackagePublication,
    key: &str,
) -> super::super::WorkflowRevision {
    let version = &publication
        .members
        .iter()
        .find(|item| item.key == key)
        .unwrap()
        .artifact
        .version;
    super::super::WorkflowRevision {
        id: version.id.clone(),
        revision: version.revision.clone(),
    }
}

#[tokio::test]
async fn super_ai_planning_publishes_with_versioned_review_rounds() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("super-ai-planning.sqlite"))?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id;
    let draft = store
        .create_workflow_factory_draft(
            project.clone(),
            WorkflowFactoryTemplate::SuperAiDevelopmentPlanning,
            FACTORY_REVISION,
        )
        .await?;
    assert!(draft.publication.is_none());
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Expected an explicitly reviewed planning template");
    };
    let root = &publication.members[0].artifact;
    assert_eq!(root.version.kind, PackageArtifactKind::Pipeline);
    assert!(root.blockers.is_empty(), "{:?}", root.blockers);
    let version = super::super::WorkflowRevision {
        id: root.version.id.clone(),
        revision: root.version.revision.clone(),
    };
    let definition = store
        .workflow_definition(project, version)
        .await?
        .expect("the planning pipeline was published");
    assert_eq!(
        definition
            .nodes
            .iter()
            .filter(|node| node.instance.kind == BuiltinNode::MaterializeBacklog)
            .count(),
        1
    );
    for section in ["master-plan", "specifications", "backlog"] {
        let rounds = definition
            .nodes
            .iter()
            .filter(|node| {
                node.instance.id.starts_with(&format!("{section}::"))
                    && node.instance.kind == BuiltinNode::QualityGate
            })
            .count();
        assert_eq!(
            rounds, 3,
            "{section} should have three bounded review rounds"
        );
    }
    Ok(())
}
