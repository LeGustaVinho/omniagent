use std::{sync::Arc, time::Duration};

use serde_json::json;

use super::*;
use crate::{
    jobs::{JobAttempt, JobKind, JobScheduler},
    repository::AttemptWorktreeEvidence,
    runs::RunStatus,
    workflows::{
        ArtifactKind, DeliveryRoles, Endpoint, ManualWorkflowLaunch, NodeInstance, Wire, WorkflowLaunchSource, builtin_delivery_graph,
        tests::{approve_delivery_fixture_in_run, approved_delivery_repository},
    },
};

async fn terminal(store: &Store, job: &Job) -> Result<JobAttempt> {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if let Some(attempt) = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("Command job disappeared")?
                .latest_attempt
                .filter(|attempt| attempt.status.is_terminal())
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
        ("changed", CommandOutputFormat::Json),
    ];
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    for (mode, output) in modes {
        let (binding, arguments) = if mode == "text" {
            (&direct, vec![
                "-NoProfile".into(), "-NonInteractive".into(), "-Command".into(),
                r#"[Console]::InputEncoding = [Text.UTF8Encoding]::new($false); [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); $data = ConvertFrom-Json -InputObject ([Console]::In.ReadToEnd()); [Console]::Write('text: ' + $data.message)"#.into(),
            ])
        } else {
            (&registered, vec![mode.into()])
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
            let (from_node, source) = if mode == "text" && target == "ready" {
                ("json", "completed")
            } else {
                ("prepare", source)
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

    let invalid = terminal(&store, &submit("invalid").await?).await?;
    assert_eq!(invalid.status, RunStatus::Failed);
    assert!(
        store
            .workflow_command_result(fixture.project.clone(), invalid.run_id.clone())
            .await?
            .is_none()
    );
    let invalid_logs = store
        .command_job_output(fixture.project.clone(), invalid.run_id)
        .await?
        .unwrap();
    assert_eq!(invalid_logs.stdout, "{ unfinished");
    assert_eq!(invalid_logs.process.unwrap().code, 0);

    std::fs::write(script, "throw 'This changed script must not start'")?;
    let changed = terminal(&store, &submit("changed").await?).await?;
    assert_eq!(changed.status, RunStatus::Failed);
    assert!(
        store
            .command_job_output(fixture.project.clone(), changed.run_id)
            .await?
            .is_none()
    );
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
