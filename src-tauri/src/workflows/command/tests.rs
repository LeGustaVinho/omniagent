use std::{sync::Arc, time::Duration};

use serde_json::json;

use super::*;
use crate::{
    jobs::{AdmissionEnvironment, DispatchRejection, JobAttempt, JobKind, JobScheduler, RecoveryAction},
    repository::AttemptWorktreeEvidence,
    runs::RunStatus,
    workflows::{
        ArtifactKind, DeliveryRoles, Endpoint, ManualWorkflowLaunch, NodeInstance, Wire, WorkflowLaunchSource, builtin_delivery_graph,
        tests::{approve_delivery_fixture_in_run, approved_delivery_repository},
    },
};

async fn terminal(store: &Store, job: &Job) -> Result<JobAttempt> {
    terminal_after(store, job, None).await
}

async fn terminal_after(store: &Store, job: &Job, previous: Option<&str>) -> Result<JobAttempt> {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if let Some(attempt) = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("Command job disappeared")?
                .latest_attempt
                .filter(|attempt| attempt.status.is_terminal() && Some(attempt.run_id.as_str()) != previous)
            {
                return Ok(attempt);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn registered_command_uses_approved_worktree_and_delivers_typed_outputs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path())
        .await
        .context("Create the command checkout")?;
    let second_project = dir.path().join("other");
    std::fs::create_dir(&second_project)?;
    let other = approved_delivery_repository(&store, &second_project)
        .await
        .context("Create the unrelated checkout")?;
    let script = dir.path().join("command.ps1");
    std::fs::write(
        &script,
        r#"
param([string]$Mode)
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$data = ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd())
[Console]::Error.Write("log: $Mode")
switch ($Mode) {
  'json' {
    [IO.File]::WriteAllText((Join-Path (Get-Location) 'first.txt'), $data.message)
    [Console]::Write((@{msg = $data.message; nested = $data.nested.key} | ConvertTo-Json -Compress))
  }
  'invalid' { [Console]::Write("{ unfinished") }
  'nonzero' { [Console]::Write('{"verified":true}'); exit 23 }
  'changed' { [Console]::Write('{"verified":true}') }
}
"#,
    )?;
    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let registered = store
        .register_workflow_command(
            repo.project_id.clone(),
            CommandRegistrationRequest {
                name: "Repository script".into(),
                target: CommandTarget::Script {
                    executable: executable.clone(),
                    path: script.clone(),
                    interpreter_arguments: vec![
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                    ],
                },
            },
        )
        .await?;
    assert_eq!(
        store
            .list_workflow_command_registrations(repo.project_id.clone())
            .await?,
        [registered.clone()]
    );
    assert!(
        store
            .list_workflow_command_registrations(other.project_id.clone())
            .await?
            .is_empty()
    );
    let direct = store
        .register_workflow_command(
            repo.project_id.clone(),
            CommandRegistrationRequest {
                name: "PowerShell executable".into(),
                target: CommandTarget::Executable { path: executable },
            },
        )
        .await?;

    let modes = [
        ("json", CommandOutputFormat::Json),
        ("text", CommandOutputFormat::Text),
        ("invalid", CommandOutputFormat::Json),
        ("nonzero", CommandOutputFormat::Json),
        ("changed", CommandOutputFormat::Json),
        ("changed-child", CommandOutputFormat::Json),
    ];
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    for (mode, output) in modes {
        let (binding, arguments) = if mode == "text" {
            (&direct, vec![
                "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                r#"[Console]::InputEncoding = [Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); $data = ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd()); [Console]::Write('text: ' + $data.message)"#.into(),
            ])
        } else {
            (&registered, vec![if mode == "changed-child" { "changed" } else { mode }.into()])
        };
        graph.nodes.push(NodeInstance {
            id: mode.into(),
            kind: BuiltinNode::Command,
            configuration: NodeConfiguration::Command(CommandConfiguration {
                registration_id: binding.id.clone(),
                registration_revision: binding.revision.clone(),
                arguments,
                input: serde_json::from_value(json!({"message":"ação 🚀","nested":{"key":7}}))?,
                output,
            }),
        });
        for (source, target) in [
            ("prepared", "ready"),
            ("release", "release"),
            ("worktree", "worktree"),
        ] {
            let (from_node, source) = match (mode, target) {
                ("text", "ready") => ("json", "completed"),
                ("changed-child", "ready") => ("changed", "completed"),
                _ => ("prepare", source),
            };
            graph.wires.push(Wire {
                from: Endpoint {
                    node: from_node.into(),
                    port: source.into(),
                },
                to: Endpoint {
                    node: mode.into(),
                    port: target.into(),
                },
            });
        }
    }
    for (id, from, port) in [
        ("valid-result", "invalid", "completed"),
        ("diagnose-result", "invalid", "failed"),
    ] {
        graph.nodes.push(NodeInstance {
            id: id.into(), kind: super::super::BuiltinNode::Parallel, configuration: NodeConfiguration::None,
        });
        graph.wires.push(Wire {
            from: Endpoint { node: from.into(), port: port.into() },
            to: Endpoint { node: id.into(), port: "ready".into() },
        });
    }
    let version = store
        .save_workflow_revision(repo.project_id.clone(), "command-test".into(), graph)
        .await?;
    let document = store.capture_repository_markdown(
        repo.checkout_id.clone(), Path::new(&repo.checkout_root).join("brief.md"),
    ).await?;
    let run = store.launch_workflow_manual(ManualWorkflowLaunch {
        project_id: repo.project_id.clone(), workflow: version,
        source: WorkflowLaunchSource { checkout_id: repo.checkout_id.clone(), revision_id: document.revision.id },
        request_id: Uuid::new_v4().to_string(), work_id: None,
    }).await?;
    assert!(
        store
            .submit_workflow_command(WorkflowCommandSubmission {
                project_id: repo.project_id.clone(),
                run_id: run.id.clone(),
                node_id: "json".into(),
                inputs: NodeInputs::new(),
            })
            .await
            .is_err()
    );
    let fixture = approve_delivery_fixture_in_run(&store, dir.path(), repo, None, run).await?;
    let submit = |node: &str| {
        store.submit_workflow_command(WorkflowCommandSubmission {
            project_id: fixture.project.clone(),
            run_id: fixture.run.id.clone(),
            node_id: node.into(),
            inputs: NodeInputs::from([
                ("release".into(), fixture.release.id.clone()),
                ("worktree".into(), fixture.worktree.id.clone()),
            ]),
        })
    };
    assert!(submit("text").await.is_err());
    let json_job = submit("json").await?;
    assert_eq!(json_job.kind, JobKind::Command);
    assert_eq!(submit("json").await?.id, json_job.id);
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let accepted = terminal(&store, &json_job).await?;
    if accepted.status != RunStatus::Succeeded {
        let logs = store
            .command_job_output(fixture.project.clone(), accepted.run_id.clone())
            .await?;
        let failure = store
            .job_failure(fixture.project.clone(), accepted.run_id.clone())
            .await?;
        anyhow::bail!("Registered command did not succeed: {failure:?} {logs:?}");
    }
    let evidence = store
        .workflow_command_result(fixture.project.clone(), accepted.run_id.clone())
        .await?
        .context("Command result was not published")?;
    assert_eq!(accepted.status, RunStatus::Succeeded);
    assert_eq!(
        evidence.value,
        ArtifactValue::CommandResult(WorkflowCommandResult {
            run_id: accepted.run_id.clone(),
            output: CommandData::Json(json!({"msg":"ação 🚀","nested":7})),
        })
    );
    assert_eq!(
        store
            .resolve_workflow_artifact(
                fixture.project.clone(),
                fixture.run.id.clone(),
                evidence.id.clone(),
                ArtifactKind::CommandResult
            )
            .await?,
        Some(evidence.clone())
    );
    assert!(
        store
            .workflow_command_result(other.project_id, accepted.run_id.clone())
            .await?
            .is_none()
    );
    let logs = store
        .command_job_output(fixture.project.clone(), accepted.run_id.clone())
        .await?
        .unwrap();
    assert_eq!(logs.stderr, "log: json");
    assert_eq!(logs.process.unwrap().code, 0);
    assert!(matches!(
        store
            .attempt_worktree_evidence(fixture.project.clone(), accepted.run_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { .. })
    ));
    let ArtifactValue::Worktree { checkout_id, .. } = &fixture.worktree.value else {
        unreachable!()
    };
    let root = crate::repository::checkout_root(&store, checkout_id.clone()).await?;
    assert_eq!(std::fs::read_to_string(root.join("first.txt"))?, "ação 🚀");
    assert_eq!(
        std::fs::read_to_string(Path::new(&fixture.checkout.repository_root).join("first.txt"))?,
        "original first\n"
    );

    let as_text = terminal(&store, &submit("text").await?).await?;
    assert_eq!(as_text.status, RunStatus::Succeeded);
    assert!(
        matches!(store.workflow_command_result(fixture.project.clone(), as_text.run_id).await?.unwrap().value,
        ArtifactValue::CommandResult(WorkflowCommandResult { output: CommandData::Text(text), .. }) if text == "text: ação 🚀")
    );

    let invalid_job = submit("invalid").await?;
    let invalid = terminal(&store, &invalid_job).await?;
    assert_eq!(invalid.status, RunStatus::Failed);
    assert!(
        store
            .workflow_command_result(fixture.project.clone(), invalid.run_id.clone())
            .await?
            .is_none()
    );
    let invalid_logs = store
        .command_job_output(fixture.project.clone(), invalid.run_id.clone())
        .await?
        .unwrap();
    assert_eq!(invalid_logs.stdout, "{ unfinished");
    assert_eq!(invalid_logs.process.unwrap().code, 0);
    let inspected = store
        .inspect_workflow_execution(fixture.project.clone(), fixture.run.id.clone())
        .await?
        .context("Missing workflow inspection after Command failure")?;
    let routed = inspected.control.iter().find(|record| record.node_id == "invalid")
        .context("The Command failure did not select its wired outcome")?;
    assert_eq!(routed.selected_ports, ["failed"]);
    assert_eq!(routed.source_run_id.as_deref(), Some(invalid.run_id.as_str()));
    assert!(inspected.control.iter().any(|record| record.node_id == "diagnose-result" && record.selected_ports == ["branches"]));
    assert!(inspected.control.iter().any(|record| record.node_id == "valid-result" && record.selected_ports.is_empty()));
    let invalid_node = inspected
        .jobs
        .iter()
        .find(|entry| entry.job.id == invalid_job.id)
        .context("Missing failed Command in workflow inspection")?;
    assert_eq!(invalid_node.node_id, "invalid");
    assert_eq!(invalid_node.attempts[0].run_id, invalid.run_id);
    assert_eq!(invalid_node.attempts[0].status, RunStatus::Failed);
    assert_eq!(
        invalid_node.inputs.as_ref().unwrap().get("worktree"),
        Some(&fixture.worktree.id)
    );
    assert!(inspected.outputs.iter().any(|entry| entry.artifact == evidence && entry.current));
    assert!(inspected.outputs.iter().all(|entry| entry.artifact.node_instance_id != "invalid"));
    let failure = store
        .job_failure(fixture.project.clone(), invalid.run_id.clone())
        .await?
        .context("Missing failure evidence for the inspected Command attempt")?;
    assert_eq!(failure.observation.cause, Some(crate::jobs::FailureCause::InvalidResult));
    assert!(failure.logs_available);
    let job_inspection = |job: &Job| {
        store.inspect_workflow_execution_job(
            fixture.project.clone(), fixture.run.id.clone(), job.id.clone(),
            AdmissionEnvironment { runner: DirectWindowsRunner.identity().into(), bridge_available: true },
        )
    };
    let invalid_recovery = job_inspection(&invalid_job).await?.context("Missing invalid Command inspection")?;
    assert!(invalid_recovery.recovery_actions.is_empty());
    assert!(store.queue_job_recovery(fixture.project.clone(), invalid_job.id, invalid.run_id, RecoveryAction::Retry).await.is_err());
    let failed_exit = terminal(&store, &submit("nonzero").await?).await?;
    assert_eq!(failed_exit.status, RunStatus::Failed);
    let failure = store
        .job_failure(fixture.project.clone(), failed_exit.run_id.clone())
        .await?
        .context("Missing evidence for the nonzero Command exit")?;
    assert_eq!(
        failure.observation.cause,
        Some(crate::jobs::FailureCause::ProcessExit)
    );
    assert_eq!(
        failure.observation.process.map(|process| process.code),
        Some(23)
    );
    assert!(
        store
            .workflow_command_result(fixture.project.clone(), failed_exit.run_id)
            .await?
            .is_none()
    );

    let original = std::fs::read(&script)?;
    std::fs::write(&script, "throw 'This changed script must not start'")?;
    let changed_job = submit("changed").await?;
    let changed = terminal(&store, &changed_job).await?;
    assert_eq!(changed.status, RunStatus::Failed);
    assert!(
        store
            .command_job_output(fixture.project.clone(), changed.run_id.clone())
            .await?
            .is_none()
    );
    let rejected = store.job_failure(fixture.project.clone(), changed.run_id.clone()).await?
        .context("Missing failure evidence for the changed Command script")?;
    assert_eq!(rejected.dispatch_rejection, Some(DispatchRejection::ProfileUnavailable));
    let inspected = job_inspection(&changed_job).await?.context("Missing changed Command inspection")?;
    assert_eq!(inspected.job.latest_attempt.context("Missing failed attempt")?.run_id, changed.run_id);
    assert_eq!(inspected.recovery_actions, [RecoveryAction::Retry]);
    assert!(submit("changed-child").await.unwrap_err().to_string().contains("control input ready is waiting"));
    std::fs::write(script, original)?;
    store.queue_job_recovery(fixture.project.clone(), changed_job.id.clone(), changed.run_id.clone(), RecoveryAction::Retry).await?;
    let retried = terminal_after(&store, &changed_job, Some(&changed.run_id)).await?;
    assert_eq!(retried.status, RunStatus::Succeeded);
    let snapshot = store.inspect_workflow_execution(fixture.project.clone(), fixture.run.id.clone()).await?
        .context("Missing workflow inspection after Command recovery")?;
    let recorded = snapshot.jobs.iter().find(|entry| entry.job.id == changed_job.id).context("Missing retried job")?;
    assert_eq!(recorded.attempts.len(), 2);
    assert_eq!(recorded.attempts[1].run_id, retried.run_id);
    assert_eq!(recorded.attempts[1].previous_run_id.as_deref(), Some(changed.run_id.as_str()));
    let child = terminal(&store, &submit("changed-child").await?).await?;
    assert_eq!(child.status, RunStatus::Succeeded);
    scheduler.shutdown().await?;
    drop(store);
    assert_eq!(
        Store::open(database)?
            .workflow_command_result(fixture.project, accepted.run_id)
            .await?,
        Some(evidence)
    );
    Ok(())
}
