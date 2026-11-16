use super::*;
use crate::{
    jobs::AdmissionEnvironment,
    providers::{
        SessionMode,
        capabilities::{ConfigurationEvidence, ConfigurationFile, EvidenceOrigin, ObservedResult},
    },
    workflows::{
        DeliveryRoles, ManualWorkflowLaunch, NodeInputs, WorkflowLaunchSource,
        builtin_delivery_graph,
        tests::{approved_delivery_repository, submission},
    },
};

#[tokio::test]
async fn stage_requirements_profile_match_rejects_changed_settings_and_superseded_qualification()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("profile-matches.sqlite");
    let store = Store::open(database.clone())?;
    let repository = approved_delivery_repository(&store, directory.path()).await?;
    let project = repository.project_id.clone();
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let NodeConfiguration::AgentRole { requirements, .. } = &mut graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap()
        .configuration
    else {
        unreachable!()
    };
    let subject = CapabilitySubject::Protocol {
        capability: Capability::NativePlanMode,
    };
    requirements.stage.capabilities.push(subject.clone());
    let configured = requirements.clone();
    let workflow = store
        .save_workflow_revision(project.clone(), "profile-evidence".into(), graph)
        .await?;
    let checkout = PathBuf::from(&repository.checkout_root);
    let markdown = store
        .capture_repository_markdown(repository.checkout_id.clone(), checkout.join("brief.md"))
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow,
            request_id: "profile-evidence-launch".into(),
            work_id: None,
            source: WorkflowLaunchSource {
                checkout_id: repository.checkout_id.clone(),
                revision_id: markdown.revision.id,
            },
        })
        .await?;
    let context = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?["context"]
        .clone();
    let mut submission = submission(&repository, &run, "plan", None, SessionMode::Plan);
    submission.artifacts = NodeInputs::from([("context".into(), context)]);
    submission.scope.model = Some("model-reviewed".into());
    store.submit_workflow_agent(submission).await?;
    let plan = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("Missing stage attempt")?;
    let detected = ConfigurationFile {
        path: checkout.join("AGENTS.md"),
        revision: "initial-content".into(),
        environment_revision: None,
    };
    let effective = CapabilityScope::from_dispatch(
        &plan.scope,
        plan.run.checkout_id,
        Some("CLI-identity".into()),
        DirectWindowsRunner.execution_platform(),
        ConfigurationEvidence {
            detected: vec![detected],
            unresolved: vec![],
            provider_revision: None,
            effective_revision: None,
            scoped_launch_revision: Some("fixture-launch".into()),
            environment_revision: None,
            tool_revisions: Default::default(),
        },
        Some("model-reviewed".into()),
        None,
    );
    let descriptor = ReportedCapabilities::default()
        .descriptors(&effective)
        .into_iter()
        .find(|entry| entry.subject == subject)
        .unwrap();
    let verification = store
        .record_capability_evidence(
            effective.clone(),
            descriptor.clone(),
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Succeeded,
            plan.run.id.clone(),
        )
        .await?;
    let decisions = store
        .resolve_capability_assessment(effective.clone(), vec![descriptor.clone()])
        .await?;
    let decision = check_capability(&decisions, &subject);
    assert!(matches!(&decision, StageRequirementDecision::Capability {
        status: Some(CapabilityStatus::Verified), evidence_id: Some(evidence), blocker: None, ..
    } if evidence == &verification));
    let other = CapabilitySubject::Tool {
        id: "mcp/fixture".into(),
        version: "2".into(),
        operation: "read".into(),
    };
    assert!(check_capability(&decisions, &other).blocker().is_some());
    let pinned = store
        .stage_requirements_for_attempt(plan.run.id.clone())
        .await?
        .unwrap();
    store
        .save_stage_assessment(
            plan.run.id.clone(),
            WorkflowStageAssessment {
                node_id: pinned.node_id,
                instance_revision: pinned.instance_revision,
                role: pinned.role,
                role_template: pinned.role_template,
                scope: effective.clone(),
                decisions: vec![decision],
            },
        )
        .await?;
    drop(store);
    let store = Store::open(database)?;
    let selection = ModelSelection {
        provider: effective.provider,
        profile_id: effective.profile_id.clone(),
        model_id: "model-reviewed".into(),
    };
    let mut current = effective.clone();
    current.reported_model = None;
    current.configuration.scoped_launch_revision = None;
    let assess = |scope: CapabilityScope| {
        let (project, selection, configured) =
            (project.clone(), selection.clone(), configured.clone());
        store.execute(move |db| match_profile(db, &project, selection, &configured, &scope, &[]))
    };
    let exact = assess(current.clone()).await?;
    assert_eq!(exact.attempt_id.as_deref(), Some(plan.run.id.as_str()));
    assert!(exact.checked_at.is_some());
    assert!(exact.blockers.is_empty());
    let mut changed = current.clone();
    changed.configuration.detected[0].revision = "changed-file".into();
    let edited = assess(changed).await?;
    assert!(edited.attempt_id.is_none());
    assert!(edited.blockers[0].contains("configuration files"));
    store
        .record_capability_evidence(
            effective,
            descriptor,
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Failed,
            "failed-recheck".into(),
        )
        .await?;
    let obsolete = assess(current).await?;
    assert_eq!(obsolete.attempt_id.as_deref(), Some(plan.run.id.as_str()));
    assert!(obsolete.blockers[0].contains("superseded"));
    Ok(())
}
