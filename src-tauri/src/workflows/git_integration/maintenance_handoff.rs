use sha2::{Digest, Sha256};

use crate::workflows::{
    ApprovalDecisionSource, BuiltinNode, DeliveryRoles,
    builtin::{BUILTIN_DELIVERY_ID, MAINTENANCE_ACTIVATION_ID},
    maintenance::{
        MaintenanceActivationMode, MaintenanceActivationRequest, MaintenanceBuild,
        MaintenanceConfiguration, MaintenanceHandoffRequest,
    },
    tests::{approve_reviewed_plan_fixture, link_reviewed_plan_fixture},
};

use super::*;

const PLAN: &str = r#"Build a candidate of the maintenance application.

```omniagent-plan
{"units":[{"purpose":"Correct the build revision","completionCriteria":["The candidate emits the expected build revision"]}]}
```"#;

const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$version = (Get-Content -Raw 'release-version.txt').Trim()
if ($version -ne '0.2.0') { exit 39 }
if ($args[0] -eq 'test') { exit 0 }
$commit = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { exit 40 }
[IO.File]::WriteAllText((Join-Path $pwd 'candidate.version'), $version)
[IO.File]::WriteAllText((Join-Path $pwd 'candidate.zip'), "$version/${commit}")
"#;

#[tokio::test]
async fn maintenance_uses_the_verified_source_and_requires_a_user_selected_fixture_activation()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("maintenance.sqlite");
    let package_root = temp.path().join("packages");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let source = Path::new(&repo.checkout_root);
    fs::write(source.join("Build-Maintenance.ps1"), SCRIPT)?;
    user_commit(
        source,
        "Build-Maintenance.ps1",
        "Register fixture build script",
    )?;
    fs::write(source.join("release-version.txt"), "0.1.0\n")?;
    user_commit(
        source,
        "release-version.txt",
        "Record the running fixture version",
    )?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut build_arguments = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        ".\\Build-Maintenance.ps1",
    ]
    .map(String::from)
    .to_vec();
    build_arguments.push("test".into());
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![CandidateGate {
                name: "maintenance tests".into(),
                required: true,
                action: CandidateGateAction::Command {
                    executable: powershell.clone(),
                    arguments: build_arguments.clone(),
                    timeout_seconds: Some(30),
                },
            }],
        )
        .await?;
    build_arguments.pop();
    let mut configuration = MaintenanceConfiguration {
        source_checkout_id: repo.checkout_id.clone(),
        workflow_id: BUILTIN_DELIVERY_ID.into(),
        build: MaintenanceBuild {
            executable: powershell,
            arguments: build_arguments,
            timeout_seconds: Some(30),
            package_path: "candidate.zip".into(),
            version_path: "candidate.version".into(),
        },
        required_checks: vec!["maintenance tests".into()],
        activation: MaintenanceActivationMode::Disabled,
    };
    assert!(
        store
            .workflow_maintenance(project.clone())
            .await?
            .settings
            .is_none()
    );
    let disabled_settings = store
        .set_workflow_maintenance(project.clone(), None, configuration.clone())
        .await?;
    let run = store
        .start_builtin_delivery(project.clone(), DeliveryRoles::default())
        .await?;
    let definition = store
        .workflow_definition(project.clone(), run.version.clone())
        .await?
        .context("Missing pinned maintenance workflow")?;
    assert!(
        definition
            .nodes
            .iter()
            .any(|node| node.instance.id == MAINTENANCE_ACTIVATION_ID
                && node.instance.kind == BuiltinNode::ActivateMaintenance)
    );
    let linked = link_reviewed_plan_fixture(
        &store,
        &repo,
        None,
        &run,
        PLAN,
        "Correct OmniAgent candidate version",
    )
    .await?;
    let fixture =
        approve_reviewed_plan_fixture(&store, temp.path(), repo.clone(), run, linked).await?;
    let unit = fixture
        .units
        .first()
        .context("One maintenance task unit should have been released")?;
    let task = &unit.task_id;
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("The normal approval did not provision the managed worktree")?
        .worktree;
    assert_ne!(worktree.root, source);
    let mut request = MaintenanceHandoffRequest {
        project_id: project.clone(),
        workflow_run_id: fixture.run.id.clone(),
        node_id: MAINTENANCE_ACTIVATION_ID.into(),
        git_artifact_id: "not-yet-verified".into(),
        task_id: task.clone(),
        commit_evidence_id: "not-yet-verified".into(),
        expected_settings_revision: disabled_settings.revision.clone(),
    };
    assert!(
        store
            .create_maintenance_handoff(request.clone(), package_root.clone())
            .await
            .is_err()
    );
    assert!(!package_root.exists());

    let queued = queue_implementation(&store, &fixture, &worktree, unit).await?;
    let target =
        capture_implementation_changes(&store, &fixture, &worktree, unit, queued, |root| {
            fs::write(root.join("release-version.txt"), "0.2.0\n")?;
            Ok(())
        })
        .await?;
    let candidate = select_candidate(&store, &fixture, target, &["release-version.txt"]).await?;
    let review = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &candidate,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(&store, &fixture, unit, &candidate).await?;
    assert!(
        checked_evidence(&store, project, task, &checks)
            .await?
            .allows_acceptance()
    );
    let acceptance = accept_candidate(&store, &fixture, unit, &candidate, &review, &checks).await?;
    let outcome = commit(
        &store,
        &fixture,
        unit,
        &AcceptedUnit {
            candidate,
            acceptance: acceptance.clone(),
        },
        None,
    )
    .await?;
    let WorkflowUnitOutcome::Artifact { artifact: git } = &outcome else {
        anyhow::bail!("Expected the pinned Verify commit output");
    };
    request.git_artifact_id = git.id.clone();
    let verified = verified(&store, &fixture, unit, outcome).await?;
    let ArtifactValue::Release(release) = &fixture.release.value else {
        anyhow::bail!("Expected a verified release from the Approve implementation stage");
    };
    request.commit_evidence_id = verified.id.clone();
    let result = store
        .execute_workflow_unit(unit_request(
            &fixture,
            unit,
            "result",
            NodeInputs::from([
                ("git".into(), request.git_artifact_id.clone()),
                ("task".into(), fixture.task.id.clone()),
            ]),
            WorkflowUnitRequest::Result,
        ))
        .await?;
    let WorkflowUnitOutcome::Artifact { artifact: result } = result else {
        anyhow::bail!(
            "The normal delivery result must advance while fixture activation is disabled"
        );
    };
    let mut wrong_wire = request.clone();
    wrong_wire.git_artifact_id = result.id;
    assert!(
        store
            .create_maintenance_handoff(wrong_wire, package_root.clone())
            .await
            .is_err()
    );

    // Prove the builder reads the accepted commit while the live worktree drifts.
    fs::write(worktree.root.join("release-version.txt"), "9.9.9\n")?;
    let disabled = store
        .create_maintenance_handoff(request.clone(), package_root.clone())
        .await?;
    let mut activation = MaintenanceActivationRequest {
        project_id: project.clone(),
        handoff_id: disabled.id.clone(),
        expected_settings_revision: disabled_settings.revision.clone(),
        expected_sha256: disabled.sha256.clone(),
    };
    assert!(
        store
            .activate_maintenance_fixture(activation.clone(), package_root.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("disabled")
    );
    configuration.activation = MaintenanceActivationMode::ManualFixture;
    assert!(
        store
            .set_workflow_maintenance(project.clone(), None, configuration.clone())
            .await
            .is_err()
    );
    let enabled = store
        .set_workflow_maintenance(
            project.clone(),
            Some(disabled_settings.revision),
            configuration,
        )
        .await?;
    assert!(
        store
            .create_maintenance_handoff(request.clone(), package_root.clone())
            .await
            .is_err()
    );
    request.expected_settings_revision = enabled.revision.clone();
    let handoff = store
        .create_maintenance_handoff(request.clone(), package_root.clone())
        .await?;
    assert_eq!(handoff.workflow, fixture.run.version);
    assert_eq!(handoff.node_id, MAINTENANCE_ACTIVATION_ID);
    assert_eq!(handoff.git_artifact_id, request.git_artifact_id);
    assert_eq!(
        store
            .create_maintenance_handoff(request, package_root.clone())
            .await?,
        handoff
    );
    assert_eq!(handoff.version, "0.2.0");
    assert_eq!(handoff.source_revision, verified.sha);
    assert_eq!(handoff.source_tree, verified.tree);
    assert_eq!(handoff.verification_id, verified.verification_id);
    assert_eq!(handoff.approval_artifact_id, release.decision_artifact_id);
    assert_eq!(handoff.approval_source, ApprovalDecisionSource::Operator);
    assert_eq!(handoff.acceptance_artifact_id, acceptance.id);
    assert_eq!(handoff.review_artifact_id, review.id);
    assert_eq!(handoff.checks, ["maintenance tests"]);
    let expected = format!("0.2.0/{}", verified.sha).into_bytes();
    assert_eq!(handoff.sha256, format!("{:x}", Sha256::digest(&expected)));
    let package_path = package_root.join(format!("{}.package", handoff.id));
    assert_eq!(fs::read(&package_path)?, expected);
    assert_eq!(
        fs::read_to_string(worktree.root.join("release-version.txt"))?,
        "9.9.9\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("release-version.txt"))?.trim(),
        "0.1.0"
    );

    let live = serde_json::to_value(store.workflow_maintenance(project.clone()).await?)?;
    assert_eq!(live["runningBuild"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        live["handoffs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == handoff.id && entry["activation"].is_null())
    );
    activation.handoff_id = handoff.id.clone();
    activation.expected_settings_revision = enabled.revision;
    activation.expected_sha256 = "invalid".into();
    assert!(
        store
            .activate_maintenance_fixture(activation.clone(), package_root.clone())
            .await
            .is_err()
    );
    activation.expected_sha256 = handoff.sha256.clone();
    fs::write(&package_path, "changed bytes")?;
    assert!(
        store
            .activate_maintenance_fixture(activation.clone(), package_root.clone())
            .await
            .is_err()
    );
    fs::write(&package_path, &expected)?;
    let recorded = store
        .activate_maintenance_fixture(activation.clone(), package_root.clone())
        .await?;
    let activated = recorded
        .activation
        .as_ref()
        .context("The fixture decision did not activate its node")?;
    let execution = store
        .inspect_workflow_execution(project.clone(), fixture.run.id.clone())
        .await?
        .context("Inspect the pinned maintenance workflow")?;
    let output = execution
        .outputs
        .iter()
        .find(|output| output.artifact.id == activated.workflow_artifact_id)
        .context("The inspector lacks the node's recorded activation output")?;
    assert_eq!(output.artifact.node_instance_id, MAINTENANCE_ACTIVATION_ID);
    assert_eq!(output.artifact.port, "source");
    assert!(output.current);
    assert!(
        output
            .input_receipts
            .iter()
            .any(|receipt| receipt.inputs.get("git") == Some(&handoff.git_artifact_id))
    );
    drop(store);
    let store = Store::open(database)?;
    assert_eq!(
        store
            .activate_maintenance_fixture(activation.clone(), package_root.clone())
            .await?,
        recorded
    );
    store
        .revoke_job_pipeline_release(project.clone(), release.release.id.clone())
        .await?;
    assert!(
        store
            .activate_maintenance_fixture(activation, package_root)
            .await
            .is_err()
    );
    Ok(())
}
