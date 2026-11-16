use crate::{
    jobs::{AdmissionEnvironment, JobRole},
    model_catalog::ModelSelection,
    repository::ProjectCheckout,
    runs::RunStatus,
    workflows::{
        BuiltinNode, ConditionPredicate, NodeConfiguration, NodeInstance, WorkflowGraph,
        role_templates::{ConversationTemplate, TemplateReviewPolicy},
        tests::approved_delivery_repository,
    },
};

use super::*;

fn scope(repo: &ProjectCheckout) -> SessionScope {
    let root = Path::new(&repo.checkout_root);
    SessionScope {
        provider: Provider::Codex,
        profile_id: "editor-fixture".into(),
        checkout: root.into(),
        executable: root.join("codex.exe"),
        runner: "fixture-runner".into(),
        permission_revision: "fixture-policy".into(),
        cli_version: "0.169.0".into(),
        account_binding: Some("fixture".into()),
        mode: SessionMode::ReadOnly,
        model: Some("selected-model".into()),
        effort: Some("low".into()),
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

fn package(members: Vec<PackageMember>) -> WorkflowPackage {
    WorkflowPackage {
        name: "Editor assistance fixture".into(),
        members,
    }
}

fn preset(pointer: &str) -> PackageMember {
    member(
        "preset",
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::Condition,
            configuration: NodeConfiguration::Condition {
                predicate: ConditionPredicate::Exists {
                    pointer: pointer.into(),
                },
            },
            editable_fields: vec!["predicate".into()],
        },
    )
}

async fn publish(
    store: &Store,
    project: &str,
    members: Vec<PackageMember>,
) -> Result<WorkflowPackagePublication> {
    let draft = store
        .create_workflow_package(project.into(), None, package(members))
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The fixture publication was not saved");
    };
    Ok(publication)
}

#[tokio::test]
async fn editor_assistance_links_exact_pipeline_role_and_reusable_revisions_to_a_visible_chat()
-> Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("editor-assistance.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temporary.path()).await?;
    let project = &repo.project_id;
    let pipeline = member(
        "pipeline",
        PackageArtifactBody::Pipeline {
            graph: WorkflowGraph {
                name: "Review pipeline".into(),
                nodes: vec![NodeInstance {
                    id: "start".into(),
                    kind: BuiltinNode::Start,
                    configuration: NodeConfiguration::None,
                }],
                wires: Vec::new(),
            },
        },
    );
    let role = member(
        "role",
        PackageArtifactBody::Role {
            template: ConversationTemplate {
                name: "Review role".into(),
                role: JobRole::Agent,
                model: ModelSelection {
                    provider: Provider::Codex,
                    profile_id: "editor-fixture".into(),
                    model_id: "selected-model".into(),
                },
                effort: None,
                instructions: "Preserve the selected role revision".into(),
                tool_scope: SessionMode::ReadOnly,
                completion_contract: "Propose a review update".into(),
                review_policy: TemplateReviewPolicy::HumanApproval,
                requirements: Default::default(),
            },
        },
    );
    let initial = publish(&store, project, vec![pipeline, role, preset("/old")]).await?;
    let original = initial
        .members
        .iter()
        .find(|entry| entry.key == "preset")
        .context("Missing reusable node")?;
    let mut edited = preset("/new");
    edited.base = Some(original.artifact.version.clone());
    let newer = publish(&store, project, vec![edited]).await?;
    assert_ne!(
        newer.members[0].artifact.version.revision,
        original.artifact.version.revision
    );

    let mut retained = Vec::new();
    for member in &initial.members {
        let version = member.artifact.version.clone();
        let draft = store
            .start_workflow_editor_authoring(
                project.clone(),
                repo.checkout_id.clone(),
                scope(&repo),
                Some(version.clone()),
                "Propose an explicit editor change".into(),
            )
            .await?;
        assert_eq!(draft.package.members[0].base.as_ref(), Some(&version));
        assert_eq!(draft.package.members[0].body, member.artifact.body);
        let conversation_id = draft
            .conversation_id
            .clone()
            .context("Missing source conversation")?;
        assert!(
            store
                .list_conversations(repo.checkout_id.clone())
                .await?
                .iter()
                .any(|chat| chat.id == conversation_id)
        );
        assert_eq!(
            store
                .workflow_packages(project.clone(), Some(conversation_id.clone()))
                .await?[0]
                .id,
            draft.id
        );
        let request = draft
            .request
            .context("Missing explicit authoring request")?;
        assert_eq!(request.state, "queued");
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-runner".into(),
                bridge_available: true,
            })
            .await?
            .context("The editor request did not enter the scheduler")?;
        assert_eq!(attempt.scope.provider, Provider::Codex);
        assert_eq!(attempt.scope.profile_id, "editor-fixture");
        assert_eq!(attempt.scope.model.as_deref(), Some("selected-model"));
        assert_eq!(attempt.scope.effort.as_deref(), Some("low"));
        assert_eq!(attempt.scope.mode, SessionMode::ReadOnly);
        let attempt_id = attempt.run.id;
        store.mark_agent_running(attempt_id.clone()).await?;
        store
            .finish_agent_attempt(
                attempt_id.clone(),
                RunStatus::Failed,
                Some("Test fixture stop".into()),
            )
            .await?;
        let queued = store
            .job(project.clone(), request.job_id.clone())
            .await?
            .context("Missing exact authoring job")?;
        assert_eq!(
            queued
                .latest_attempt
                .context("Missing claimed attempt")?
                .run_id,
            attempt_id
        );
        retained.push((draft.id, conversation_id, request.id, version, attempt_id));
    }

    let mut unsupported = scope(&repo);
    unsupported.provider = Provider::Antigravity;
    let count = store
        .list_conversations(repo.checkout_id.clone())
        .await?
        .len();
    let drafts = store.workflow_packages(project.clone(), None).await?.len();
    assert!(
        store
            .start_workflow_editor_authoring(
                project.clone(),
                repo.checkout_id.clone(),
                unsupported,
                Some(original.artifact.version.clone()),
                "Unsupported request".into()
            )
            .await
            .is_err()
    );
    assert!(
        store
            .start_workflow_editor_authoring(
                "another project".into(),
                repo.checkout_id.clone(),
                scope(&repo),
                None,
                "Foreign checkout".into()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .list_conversations(repo.checkout_id.clone())
            .await?
            .len(),
        count
    );
    assert_eq!(
        store.workflow_packages(project.clone(), None).await?.len(),
        drafts
    );
    let manual = store
        .create_workflow_package(project.clone(), None, package(vec![preset("/manual")]))
        .await?;
    let linked = store
        .request_workflow_package_changes(
            project.clone(),
            manual.id.clone(),
            manual.revision.clone(),
            repo.checkout_id.clone(),
            scope(&repo),
            "Revise this saved package".into(),
        )
        .await?;
    let linked_chat = linked
        .conversation_id
        .context("An editor request needs a source conversation")?;
    assert_eq!(
        store
            .workflow_packages(project.clone(), Some(linked_chat))
            .await?[0]
            .id,
        manual.id
    );
    assert!(
        store
            .request_workflow_package_changes(
                project.clone(),
                manual.id,
                manual.revision,
                repo.checkout_id.clone(),
                scope(&repo),
                "Duplicate while queued".into()
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .list_conversations(repo.checkout_id.clone())
            .await?
            .len(),
        count + 1
    );
    drop(store);

    let reopened = Store::open(database)?;
    for (id, conversation_id, request_id, version, attempt_id) in retained {
        let saved = reopened.workflow_package(project.clone(), id).await?;
        assert_eq!(
            saved.conversation_id.as_deref(),
            Some(conversation_id.as_str())
        );
        assert_eq!(saved.package.members[0].base.as_ref(), Some(&version));
        let request = saved.request.context("Missing reopened request")?;
        assert_eq!(request.id, request_id);
        assert_eq!(request.run_id.as_deref(), Some(attempt_id.as_str()));
    }
    Ok(())
}
