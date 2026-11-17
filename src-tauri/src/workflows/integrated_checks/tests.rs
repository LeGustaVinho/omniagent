use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

use crate::{
    repository::verification::{CandidateGate, CandidateGateAction, CandidateGateOutcome},
    workflows::{
        WorkflowRevision,
        control::{ControlStageReceipt, ControlStageState, recorded_control},
        tests::approved_delivery_repository,
    },
};

use super::*;

fn wire(source: (&str, &str), target: (&str, &str)) -> Value {
    json!({
        "from": { "node": source.0, "port": source.1 },
        "to": { "node": target.0, "port": target.1 },
    })
}

async fn check_workflow(store: &Store, project: &str) -> Result<WorkflowRevision> {
    let child = store
        .save_workflow_revision(
            project.into(),
            "integrated-test-child".into(),
            serde_json::from_value(json!({
                "name": "Backlog delivery fixture",
                "nodes": [
                    {"id": "start", "kind": "typed_start", "configuration": {
                        "kind": "typed_start", "fields": [
                            {"name": "backlog", "valueType": "object", "required": true}
                        ]
                    }},
                    {"id": "approve", "kind": "approve_implementation", "configuration": {"kind": "none"}}
                ],
                "wires": [
                    wire(("start", "start"), ("approve", "ready")),
                    wire(("start", "task"), ("approve", "task")),
                    wire(("start", "plan"), ("approve", "plan")),
                ]
            }))?,
        )
        .await?;
    let mut nodes = vec![
        json!({"id": "start", "kind": "typed_start", "configuration": {"kind": "typed_start", "fields": []}}),
        json!({"id": "criteria", "kind": "set_fields", "configuration": {
            "kind": "set_fields", "mode": "build", "operations": [
                {"op": "set", "path": "/fixture", "value": {"source": "literal", "value": true}}
            ],
        }}),
        json!({"id": "materialize", "kind": "materialize_backlog", "configuration": {"kind": "none"}}),
        json!({"id": "dispatch", "kind": "run_backlog", "configuration": {
            "kind": "run_backlog", "child": child, "maxParallel": 1,
            "failed": {"node": "approve", "port": "approved"}
        }}),
    ];
    let mut wires = vec![
        wire(("start", "start"), ("criteria", "ready")),
        wire(("criteria", "completed"), ("materialize", "ready")),
        wire(("criteria", "object"), ("materialize", "plan")),
        wire(("criteria", "object"), ("materialize", "sdd")),
        wire(("criteria", "object"), ("materialize", "bdd")),
        wire(("materialize", "completed"), ("dispatch", "ready")),
        wire(("materialize", "backlog"), ("dispatch", "backlog")),
    ];
    for (id, checks) in [
        (
            "1-regression",
            &["change scratch files", "read exact revision"][..],
        ),
        ("2-build", &["fail build"][..]),
        ("3-missing", &["unregistered"][..]),
        ("4-changed-policy", &["read exact revision"][..]),
    ] {
        nodes.push(json!({
            "id": id, "kind": "verify_integrated_checks",
            "configuration": { "kind": "integrated_checks", "checks": checks },
        }));
        wires.push(wire(("dispatch", "completed"), (id, "ready")));
        wires.push(wire(("dispatch", "integrated"), (id, "integrated")));
    }
    store
        .save_workflow_revision(
            project.into(),
            "integrated-test".into(),
            serde_json::from_value(
                json!({"name": "Integrated verification", "nodes": nodes, "wires": wires}),
            )?,
        )
        .await
}

fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git.exe")
        .current_dir(root)
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

#[tokio::test]
async fn integrated_checks_keep_the_pinned_sha_and_report_all_outcomes_after_reopen() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let database = temp.path().join("integrated-checks.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let project = repo.project_id.clone();
    let root = Path::new(&repo.checkout_root);
    let sha = git(root, &["rev-parse", "HEAD"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let gate = |name: &str, script: &str| CandidateGate {
        name: name.into(),
        required: false,
        action: CandidateGateAction::Command {
            executable: powershell.clone(),
            arguments: ["-NoProfile", "-NonInteractive", "-Command", script]
                .map(str::to_owned)
                .to_vec(),
            timeout_seconds: Some(20),
        },
    };
    let policy_revision = store.set_candidate_gate_policy(project.clone(), vec![
        gate("change scratch files", "Set-Content -LiteralPath first.txt -Value 'scratch file changed'; exit 0"),
        gate("read exact revision", &format!(
            "if ((git.exe rev-parse HEAD) -ne '{sha}' -or (Get-Content -LiteralPath first.txt) -ne 'original first') {{ exit 19 }}; exit 0"
        )),
        gate("fail build", "[Console]::Error.WriteLine('compile failed'); exit 22"),
    ]).await?;
    let workflow = check_workflow(&store, &project).await?;
    let run = store.start_workflow_run(project.clone(), workflow).await?;
    let handle = run.clone();
    let branch = git(root, &["branch", "--show-current"])?;
    let revision = IntegratedRevision {
        destination_branch: branch,
        sha: sha.clone(),
        integration_shas: vec![sha.clone()],
    };
    let source = store
        .execute(move |db| {
            db.execute(
                "INSERT INTO workflow_automatic_runs VALUES (?1)",
                [&handle.id],
            )?;
            let artifact = insert_artifact(
                db,
                &handle.project_id,
                handle.id.clone(),
                "dispatch".into(),
                "integrated".into(),
                ArtifactValue::IntegratedRevision(revision),
            )?;
            let plan = handle.plan_revision.as_deref().unwrap_or_default();
            record_workflow_input_ids(db, &artifact, plan, &NodeInputs::new())?;
            let receipt = ControlStageReceipt {
                node_id: "dispatch".into(),
                unit_id: None,
                plan_revision: handle.plan_revision.clone(),
                state: ControlStageState::Completed,
                selected_ports: vec!["completed".into()],
                reason: None,
                source_run_id: None,
                dependencies: Vec::new(),
                forwarded_outputs: Default::default(),
            };
            db.execute(
                "INSERT INTO workflow_control_receipts VALUES (?1, 'dispatch', ?2, '', ?3)",
                params![
                    handle.id,
                    plan,
                    protection::protect(&serde_json::to_vec(&receipt)?)?
                ],
            )?;
            refresh(db, &handle.project_id, &handle.id)?;
            assert_eq!(waits(db, &handle)?.len(), 4);
            Ok(artifact.id)
        })
        .await?;
    store.set_job_queue_paused(true).await?;
    assert!(store.next_workflow_integrated_checks().await?.is_none());
    drop(store);

    fs::write(root.join("first.txt"), "live checkout advanced\n")?;
    git(root, &["add", "first.txt"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Workflow Test",
            "-c",
            "user.email=workflow@example.test",
            "commit",
            "-qm",
            "Concurrent checkout update",
        ],
    )?;
    let store = Store::open(database.clone())?;
    store.set_job_queue_paused(false).await?;
    for _ in 0..3 {
        store
            .next_workflow_integrated_checks()
            .await?
            .context("The enqueued integrated checks were lost")?
            .execute(&store)
            .await?;
    }
    store
        .set_candidate_gate_policy(
            project.clone(),
            vec![gate("read exact revision", "exit 85")],
        )
        .await?;
    store
        .next_workflow_integrated_checks()
        .await?
        .context("The old check policy was lost")?
        .execute(&store)
        .await?;
    assert_eq!(
        fs::read_to_string(root.join("first.txt"))?.trim(),
        "live checkout advanced"
    );
    drop(store);

    let reopened = Store::open(database)?;
    assert!(reopened.next_workflow_integrated_checks().await?.is_none());
    let artifacts = reopened
        .workflow_run_artifacts(project.clone(), run.id.clone())
        .await?;
    let receipts = reopened
        .execute(move |db| recorded_control(db, &run))
        .await?;
    for (node, verdict) in [
        ("1-regression", TestVerdict::Verified),
        ("2-build", TestVerdict::Failed),
        ("3-missing", TestVerdict::Blocked),
        ("4-changed-policy", TestVerdict::Blocked),
    ] {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.node_instance_id == node && artifact.port == "evidence")
            .with_context(|| format!("The integrated checks lost evidence for {node}"))?;
        let ArtifactValue::IntegratedChecksEvidence(evidence) = &artifact.value else {
            anyhow::bail!("Wrong integrated check artifact type");
        };
        assert_eq!(evidence.verdict, verdict);
        assert_eq!(evidence.integrated_artifact_id, source);
        assert_eq!(evidence.revision.sha, sha);
        assert_eq!(evidence.policy_revision, policy_revision);
        assert_eq!(
            receipts
                .iter()
                .find(|receipt| receipt.node_id == node)
                .unwrap()
                .selected_ports,
            [verdict.port()],
        );
        match node {
            "1-regression" => {
                assert_eq!(evidence.tree.as_ref(), Some(&tree));
                assert_eq!(evidence.checks.len(), 2);
                assert!(
                    evidence
                        .checks
                        .iter()
                        .all(|check| check.executable_revision.is_some()
                            && matches!(check.outcome, CandidateGateOutcome::Passed { .. }))
                );
            }
            "2-build" => assert!(matches!(&evidence.checks[0].outcome,
                CandidateGateOutcome::Failed { exit_code: 22, stderr, .. } if stderr.contains("compile failed"))),
            "3-missing" => assert!(matches!(&evidence.checks[0].outcome,
                CandidateGateOutcome::CouldNotRun { reason } if reason.contains("unregistered"))),
            _ => {
                assert!(evidence.tree.is_none());
                assert!(
                    evidence
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("policy changed"))
                );
            }
        }
    }
    Ok(())
}
