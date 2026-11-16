use std::{collections::BTreeMap, path::Path};

use crate::{
    jobs::{AdmissionEnvironment, JobRole},
    providers::Provider,
    store::Store,
    workflows::{
        DeliveryRoles, ManualWorkflowLaunch, NodeInputs, WorkflowLaunchSource, WorkflowRevision,
        authoring::package::{
            PackageMember, PackageReference, WorkflowPackage, WorkflowPackagePublishOutcome,
        },
        builtin_delivery_graph,
        role_templates::{SaveConversationTemplate, TemplateBase},
        tests::{approved_delivery_repository, submission},
    },
};

use super::*;

fn template(name: &str, model_id: &str) -> ConversationTemplate {
    ConversationTemplate {
        name: name.into(),
        role: JobRole::Pipeline,
        model: ModelSelection {
            provider: Provider::Codex,
            profile_id: "fixture".into(),
            model_id: model_id.into(),
        },
        effort: Some("medium".into()),
        instructions: format!("Read only with {name}."),
        tool_scope: SessionMode::ReadOnly,
        completion_contract: format!("Return evidence from {name}."),
        review_policy: TemplateReviewPolicy::Independent,
        requirements: Default::default(),
    }
}

#[tokio::test]
async fn published_roles_are_pinned_before_attempts_and_head_changes_do_not_change_dispatch()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("workflow-roles.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    let pinned = template("Old planner contract", "pinned-model");
    let reusable_role = |key: &str, destination, role| PackageMember {
        key: key.into(),
        name: key.into(),
        destination,
        base: None,
        body: PackageArtifactBody::Role { template: role },
        references: BTreeMap::new(),
    };
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Role pin proof".into(),
                members: vec![
                    reusable_role("planner", PackageDestination::Project, pinned.clone()),
                    reusable_role(
                        "reviewer",
                        PackageDestination::Library,
                        template("Library reviewer", "reviewer"),
                    ),
                    PackageMember {
                        key: "pipeline".into(),
                        name: "Pinned workflow roles".into(),
                        destination: PackageDestination::Project,
                        base: None,
                        body: PackageArtifactBody::Pipeline {
                            graph: builtin_delivery_graph(DeliveryRoles::default()),
                        },
                        references: [("plan", "planner"), ("review-plan", "reviewer")]
                            .map(|(node, key)| {
                                (node.into(), PackageReference::Draft { key: key.into() })
                            })
                            .into_iter()
                            .collect(),
                    },
                ],
            },
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The reviewed role package was not published");
    };
    let version = |key: &str| {
        publication
            .members
            .iter()
            .find(|member| member.key == key)
            .map(|member| member.artifact.version.clone())
            .context("The expected package member was missing")
    };
    let pipeline = version("pipeline")?;
    let planner = version("planner")?;
    let reviewer = version("reviewer")?;
    let note = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: WorkflowRevision {
                id: pipeline.id,
                revision: pipeline.revision,
            },
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: note.revision.id,
            },
            request_id: "workflow-role-pin".into(),
            work_id: None,
        })
        .await?;
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(
            project.clone(),
            Some(TemplateBase {
                template_id: planner.id.clone(),
                expected_revision: planner.revision.clone(),
            }),
            template("New planner contract", "new-model"),
        )
        .await?
    else {
        anyhow::bail!("The new template revision did not publish");
    };
    assert_ne!(detail.revision.version.revision, planner.revision);
    drop(store);

    let store = Store::open(database)?;
    let snapshot = store
        .inspect_workflow_execution(project.clone(), run.id.clone())
        .await?
        .context("The workflow execution did not survive restart")?;
    assert!(snapshot.jobs.is_empty());
    let plan = snapshot
        .role_pins
        .iter()
        .find(|pin| pin.node_id == "plan")
        .context("Missing planner revision")?;
    assert_eq!(plan.version, planner);
    assert_eq!(
        plan.role.as_ref().context("Missing planner template")?.name,
        pinned.name
    );
    let review = snapshot
        .role_pins
        .iter()
        .find(|pin| pin.node_id == "review-plan")
        .context("Missing reviewer revision")?;
    assert_eq!(review.version, reviewer);
    assert_eq!(
        review
            .role
            .as_ref()
            .context("Missing library template")?
            .name,
        "Library reviewer"
    );
    assert!(!plan.requirements_only && !review.requirements_only);
    store
        .execute({
            let (planner, reviewer) = (planner.clone(), reviewer.clone());
            move |db| {
                assert!(load_role(db, "another-project", &planner)?.is_none());
                assert!(load_role(db, "another-project", &reviewer)?.is_some());
                Ok(())
            }
        })
        .await?;

    let upstream = store
        .current_workflow_host_outputs(project, run.id.clone())
        .await?;
    let mut selected = submission(&repo, &run, "plan", None, SessionMode::Plan);
    selected.artifacts = NodeInputs::from([("context".into(), upstream["context"].clone())]);
    selected.scope.effort.clone_from(&pinned.effort);
    selected.scope.model = Some("new-model".into());
    assert!(store.submit_workflow_agent(selected).await.is_err());
    let mut selected = submission(&repo, &run, "plan", None, SessionMode::Plan);
    selected.artifacts = NodeInputs::from([("context".into(), upstream["context"].clone())]);
    selected.scope.effort.clone_from(&pinned.effort);
    selected.scope.model = Some(pinned.model.model_id.clone());
    store.submit_workflow_agent(selected).await?;
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("The pinned role stage did not reach the scheduler")?;
    assert_eq!(attempt.scope.model.as_deref(), Some("pinned-model"));
    assert!(attempt.input[0].text.contains(&pinned.instructions));
    assert!(attempt.input[0].text.contains(&pinned.completion_contract));
    assert!(!attempt.input[0].text.contains("New planner contract"));
    assert!(
        store
            .execute(move |db| crate::jobs::validate_attempt_dispatch(db, &attempt.run.id))
            .await?
    );
    Ok(())
}

#[test]
fn a_bound_role_never_grants_write_or_waives_its_review_requirement() -> Result<()> {
    let role = template("Read-only role", "pinned-model");
    let mut scope = SessionScope {
        provider: role.model.provider,
        profile_id: role.model.profile_id.clone(),
        account_binding: None,
        executable: "codex.exe".into(),
        checkout: Default::default(),
        cli_version: "fixture".into(),
        runner: "fixture".into(),
        permission_revision: "fixture".into(),
        mode: SessionMode::ReadOnly,
        model: Some(role.model.model_id.clone()),
        effort: role.effort.clone(),
    };
    qualify_bound_role(&role, None, &scope)?;
    scope.mode = SessionMode::ApprovedWrite;
    let error = qualify_bound_role(&role, None, &scope).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not permit repository writes")
    );

    scope.mode = SessionMode::ReadOnly;
    for policy in [
        TemplateReviewPolicy::CrossReview,
        TemplateReviewPolicy::HumanApproval,
    ] {
        let role = ConversationTemplate {
            review_policy: policy,
            ..role.clone()
        };
        let error = qualify_bound_role(&role, None, &scope).unwrap_err();
        assert!(error.to_string().contains("does not bind that"));
    }
    Ok(())
}
