use crate::{
    model_catalog::ModelSelection, providers::Provider,
    workflows::tests::approved_delivery_repository,
};

use super::*;

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
