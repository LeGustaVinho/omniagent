use super::*;
use crate::repository::verification::test_phase::{TestPhase, TestVerdict};
use crate::workflows::{
    BuiltinNode, DeliveryRoles, Endpoint, NodeInstance, Wire, builtin_delivery_graph,
    test_phase::TestPhaseEvidence, tests::approve_delivery_fixture_in_run,
};

fn wire(from: (&str, &str), to: (&str, &str)) -> Wire {
    Wire {
        from: Endpoint {
            node: from.0.into(),
            port: from.1.into(),
        },
        to: Endpoint {
            node: to.0.into(),
            port: to.1.into(),
        },
    }
}

#[tokio::test]
async fn registered_test_check_verifies_baseline_red_and_green_on_exact_replayed_trees()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("test-phases.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = &repo.project_id;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    store.set_candidate_gate_policy(project.clone(), vec![CandidateGate {
        name: "BDD assertions".into(), required: true,
        action: CandidateGateAction::Command {
            executable: powershell,
            arguments: vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), r#"
                $report = @{ status = 'completed'; tests = @() }
                $exitCode = 0
                if (Test-Path -LiteralPath 'tests/behavior.case') {
                    if ((Get-Content -Raw -LiteralPath 'first.txt').Trim() -eq 'implemented') {
                        $report.tests = @(@{ id = 'BDD-1'; status = 'passed' })
                    } else {
                        $report.tests = @(@{ id = 'BDD-1'; status = 'failed'; reason = 'feature missing' })
                        $exitCode = 1
                    }
                }
                $report | ConvertTo-Json -Compress -Depth 5
                exit $exitCode
            "#.into()],
            timeout_seconds: Some(30),
        },
    }]).await?;
    let task = store
        .create_project_task(project.clone(), "Exercise the test phases".into())
        .await?;
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph
        .wires
        .retain(|wire| !(wire.to.node == "implement" && wire.to.port == "ready"));
    let mut fix = graph
        .nodes
        .iter()
        .find(|node| node.id == "implement")
        .unwrap()
        .clone();
    fix.id = "fix".into();
    graph.nodes.push(fix);
    for (id, config) in [
        (
            "baseline",
            json!({"kind":"verify_test_phase", "phase":"baseline", "check":"BDD assertions"}),
        ),
        (
            "red",
            json!({"kind":"verify_test_phase", "phase":"red", "check":"BDD assertions", "allowedTestPaths":["tests/"]}),
        ),
        (
            "green",
            json!({"kind":"verify_test_phase", "phase":"green", "check":"BDD assertions"}),
        ),
    ] {
        graph.nodes.push(NodeInstance {
            id: id.into(),
            kind: BuiltinNode::VerifyTestPhase,
            configuration: serde_json::from_value(config)?,
        });
        graph.wires.extend([
            wire(("prepare", "worktree"), (id, "worktree")),
            wire(("prepare", "release"), (id, "release")),
        ]);
    }
    graph.nodes.push(NodeInstance {
        id: "tests".into(), kind: BuiltinNode::SetFields, configuration: serde_json::from_value(json!({
            "kind":"set_fields", "mode":"build", "operations":[
                {"op":"set","path":"/paths","value":{"source":"literal","value":["tests/behavior.case"]}},
                {"op":"set","path":"/tests","value":{"source":"literal","value":[{"id":"BDD-1","reason":"feature missing"}]}}
            ]
        }))?,
    });
    graph.wires.extend([
        wire(("prepare", "prepared"), ("baseline", "ready")),
        wire(("baseline", "verified"), ("implement", "ready")),
        wire(("prepare", "prepared"), ("tests", "ready")),
        wire(("implement", "implemented"), ("red", "ready")),
        wire(("implement", "candidate"), ("red", "candidate")),
        wire(("baseline", "evidence"), ("red", "prior")),
        wire(("tests", "object"), ("red", "tests")),
        wire(("red", "verified"), ("fix", "ready")),
        wire(("plan", "plan"), ("fix", "plan")),
        wire(("prepare", "release"), ("fix", "release")),
        wire(("prepare", "worktree"), ("fix", "worktree")),
        wire(("fix", "implemented"), ("green", "ready")),
        wire(("fix", "candidate"), ("green", "candidate")),
        wire(("red", "evidence"), ("green", "prior")),
    ]);
    let version = store
        .save_workflow_revision(project.clone(), "test-phases".into(), graph)
        .await?;
    let run = store.start_workflow_run(project.clone(), version).await?;
    let automatic = run.id.clone();
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO workflow_automatic_runs VALUES (?1)",
                [automatic],
            )?;
            Ok(())
        })
        .await?;
    let fixture = approve_delivery_fixture_in_run(
        &store,
        temp.path(),
        repo.clone(),
        Some(task.id.clone()),
        run,
    )
    .await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("Missing single test unit")
    };
    let tree = store
        .live_managed_worktree(project.clone(), task.id.clone())
        .await?
        .unwrap()
        .worktree;

    let next = store
        .next_workflow_test_phase()
        .await?
        .context("The baseline was not queued")?;
    next.execute(&store).await?;
    let (baseline, base_proof) = proof(&store, &fixture, "baseline").await?;
    assert_eq!(base_proof.phase, TestPhase::Baseline);
    assert_eq!(base_proof.result.verdict, TestVerdict::Verified);
    assert_eq!(base_proof.base_commit, tree.base_commit);
    assert_eq!(
        base_proof.result.tree.as_deref(),
        Some(git(&tree.root, &["rev-parse", "HEAD^{tree}"])?.as_str())
    );

    let writer = queue_implementation(&store, &fixture, &tree, unit).await?;
    let capture = capture_implementation_changes(&store, &fixture, &tree, unit, writer, |root| {
        fs::create_dir(root.join("tests"))?;
        fs::write(root.join("tests/behavior.case"), "BDD-1\n")?;
        Ok(())
    })
    .await?;
    let red_candidate = publish(&store, &fixture, capture, "implement", &[]).await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The red writer result was not queued")?
        .publish(&store)
        .await?;
    store.set_job_queue_paused(true).await?;
    assert!(store.next_workflow_test_phase().await?.is_none());
    drop(store);

    let store = Store::open(database)?;
    store.set_job_queue_paused(false).await?;
    store
        .next_workflow_test_phase()
        .await?
        .context("The pending red phase was lost")?
        .execute(&store)
        .await?;
    let (red, red_proof) = proof(&store, &fixture, "red").await?;
    assert_eq!(red_proof.phase, TestPhase::Red);
    assert_eq!(red_proof.result.verdict, TestVerdict::Verified);
    assert_eq!(red_proof.result.exit_code, Some(1));
    assert_eq!(red_proof.prior_artifact_id, Some(baseline.id));
    let ArtifactValue::Candidate(frozen_red) = red_candidate.value else {
        unreachable!()
    };
    assert_eq!(red_proof.result.tree.as_ref(), Some(&frozen_red.tree));

    let mut write = implementation_submission(&fixture, &tree, unit)?;
    write.dispatch.node_instance_id = "fix".into();
    let writer = store.submit_workflow_agent(write).await?;
    let capture = capture_implementation_changes(&store, &fixture, &tree, unit, writer, |root| {
        fs::write(root.join("first.txt"), "implemented\n")?;
        Ok(())
    })
    .await?;
    let green_candidate = publish(&store, &fixture, capture, "fix", &["first.txt"]).await?;
    store
        .next_workflow_delivery_result()
        .await?
        .context("The green writer result was not queued")?
        .publish(&store)
        .await?;
    store
        .next_workflow_test_phase()
        .await?
        .context("The green phase was not queued")?
        .execute(&store)
        .await?;
    let (_, green_proof) = proof(&store, &fixture, "green").await?;
    let ArtifactValue::Candidate(frozen_green) = green_candidate.value else {
        unreachable!()
    };
    assert_eq!(green_proof.result.tree.as_ref(), Some(&frozen_green.tree));
    assert_eq!(green_proof.phase, TestPhase::Green);
    assert_eq!(green_proof.result.verdict, TestVerdict::Verified);
    assert_eq!(green_proof.prior_artifact_id, Some(red.id));
    assert_eq!(
        green_proof.result.test_patch_revision,
        red_proof.result.test_patch_revision
    );
    assert!(green_proof.result.test_patch_revision.is_some());
    assert!(store.next_workflow_test_phase().await?.is_none());
    Ok(())
}

async fn publish(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    target: CandidateTarget,
    node: &str,
    changed: &[&str],
) -> Result<WorkflowArtifact> {
    let task = target.task_id.clone();
    let picked = store
        .select_worktree_candidate(
            target,
            CandidateSelection {
                tracked_paths: Some(changed.iter().map(|path| (*path).into()).collect()),
                untracked_paths: vec!["tests/behavior.case".into()],
            },
        )
        .await?;
    store
        .publish_workflow_candidate(
            fixture.project.clone(),
            fixture.run.id.clone(),
            node.into(),
            task,
            picked.id,
        )
        .await
}

async fn proof(
    store: &Store,
    fixture: &ApprovedDeliveryFixture,
    node: &str,
) -> Result<(WorkflowArtifact, TestPhaseEvidence)> {
    let artifact = store
        .workflow_run_artifacts(fixture.project.clone(), fixture.run.id.clone())
        .await?
        .into_iter()
        .find(|artifact| artifact.node_instance_id == node && artifact.port == "evidence")
        .with_context(|| format!("Missing proof for {node}"))?;
    let ArtifactValue::TestPhaseEvidence(result) = &artifact.value else {
        anyhow::bail!("Wrong artifact kind for {node}");
    };
    Ok((artifact.clone(), result.clone()))
}
