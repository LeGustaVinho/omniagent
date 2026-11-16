use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    jobs::{AdmissionEnvironment, Job, JobScheduler, RecoveryAction},
    process_io::CapturedProcess,
    repository::{self, ProjectCheckout},
    runner::{DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
    workflows::{
        ArtifactValue, BuiltinNode, Endpoint, NodeConfiguration, NodeInputs, NodeInstance, Wire,
        WorkflowGraph, WorkflowHostRequest, WorkflowHostSubmission,
        mcp_tools::{McpServer, McpServerRequest, McpToolConfiguration, WorkflowMcpToolSubmission},
    },
};
use serde_json::json;

#[derive(Default)]
struct FixtureRunner {
    changed_schema: AtomicBool,
    call_received: Arc<AtomicBool>,
}

impl Runner for FixtureRunner {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }
    fn launch(&self, _: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        Err(io::Error::other(
            "An MCP fixture must use the isolated runner",
        ))
    }
    fn launch_isolated_tool(
        &self,
        request: LaunchRequest<'_>,
        revision: &str,
        outbound: bool,
    ) -> io::Result<CapturedProcess> {
        let mut arguments = request.arguments.to_vec();
        if self.changed_schema.load(Ordering::Relaxed) {
            arguments.push(OsStr::new("--changed-schema"));
        }
        let CapturedProcess {
            control,
            input,
            output: native_output,
        } = DirectWindowsRunner.launch_isolated_tool(
            LaunchRequest {
                version: request.version,
                executable: request.executable,
                checkout: request.checkout,
                arguments: &arguments,
            },
            revision,
            outbound,
        )?;
        let (relay, output) = mpsc::sync_channel(32);
        let received = self.call_received.clone();
        std::thread::spawn(move || {
            for frame in native_output {
                if frame
                    .as_ref()
                    .is_ok_and(|frame| frame.data.contains("fixture-call-received"))
                {
                    received.store(true, Ordering::Release);
                }
                if relay.send(frame).is_err() {
                    break;
                }
            }
        });
        Ok(CapturedProcess {
            output,
            control,
            input,
        })
    }
}

async fn enqueue(
    store: &Store,
    repo: &ProjectCheckout,
    file: &Path,
    config: McpToolConfiguration,
) -> Result<Job> {
    let mapped = !config.argument_mappings.is_empty();
    let (run, source) = prepare_run(store, repo, file, config).await?;
    submit(store, repo, run, mapped.then_some(source)).await
}

async fn prepare_run(
    store: &Store,
    repo: &ProjectCheckout,
    file: &Path,
    config: McpToolConfiguration,
) -> Result<(String, String)> {
    let mut wires = vec![Wire {
        from: Endpoint {
            node: "start".into(),
            port: "start".into(),
        },
        to: Endpoint {
            node: "tool".into(),
            port: "ready".into(),
        },
    }];
    if !config.argument_mappings.is_empty() {
        wires.push(Wire {
            from: Endpoint {
                node: "start".into(),
                port: "document".into(),
            },
            to: Endpoint {
                node: "tool".into(),
                port: "arguments".into(),
            },
        });
    }
    let graph = WorkflowGraph {
        name: "Local MCP fixture".into(),
        nodes: vec![
            NodeInstance {
                id: "start".into(),
                kind: BuiltinNode::Start,
                configuration: NodeConfiguration::None,
            },
            NodeInstance {
                id: "tool".into(),
                kind: BuiltinNode::McpTool,
                configuration: NodeConfiguration::McpTool(config),
            },
        ],
        wires,
    };
    let version = store
        .save_workflow_revision(repo.project_id.clone(), "local-mcp-fixture".into(), graph)
        .await?;
    let run = store
        .start_workflow_run(repo.project_id.clone(), version)
        .await?;
    let document = store
        .capture_repository_markdown(repo.checkout_id.clone(), file.into())
        .await?;
    let source = store
        .execute_workflow_host(WorkflowHostSubmission {
            project_id: repo.project_id.clone(),
            run_id: run.id.clone(),
            node_id: "start".into(),
            inputs: NodeInputs::new(),
            request: WorkflowHostRequest::Start {
                document: document.revision,
            },
        })
        .await?;
    Ok((run.id, source.id))
}

async fn submit(
    store: &Store,
    repo: &ProjectCheckout,
    run: String,
    source: Option<String>,
) -> Result<Job> {
    store
        .submit_workflow_mcp_tool(WorkflowMcpToolSubmission {
            project_id: repo.project_id.clone(),
            run_id: run,
            node_id: "tool".into(),
            checkout_id: repo.checkout_id.clone(),
            inputs: source.map_or_else(NodeInputs::new, |id| {
                NodeInputs::from([("arguments".into(), id)])
            }),
        })
        .await
}

async fn settled(store: &Store, job: &Job) -> Result<omniagent::jobs::JobAttempt> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let current = store
                .job(job.project_id.clone(), job.id.clone())
                .await?
                .context("Missing MCP job")?;
            if let Some(attempt) = current
                .latest_attempt
                .filter(|attempt| attempt.status.is_terminal())
            {
                return Ok(attempt);
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    })
    .await
    .context("Local MCP tool did not settle")?
}

#[tokio::test]
async fn stdio_mcp_requires_scope_and_keeps_uncertain_effects_out_of_replay() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let repository = fixture.path().join("source");
    std::fs::create_dir(&repository)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet", "--template="])
            .current_dir(&repository)
            .status()?
            .success()
    );
    let protected = repository.join("brief.md");
    std::fs::write(&protected, "Private fixture repository\n")?;
    let executable = fixture.path().join("mcp-server.exe");
    let code =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_mcp_tool_server.rs");
    let compiled = Command::new("rustc")
        .args(["--edition=2024", "-C", "opt-level=1"])
        .arg(code)
        .arg("-o")
        .arg(&executable)
        .output()?;
    ensure!(
        compiled.status.success(),
        "Could not compile the local stdio fixture: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let database = fixture.path().join("mcp.sqlite");
    let store = Store::open(database.clone())?;
    let repo = repository::register(&store, repository).await?;
    let project = &repo.project_id;
    let request = McpServerRequest {
        name: "Isolated fixture".into(),
        executable,
        arguments: vec![
            protected
                .to_str()
                .context("Fixture path is not Unicode")?
                .into(),
        ],
        outbound_network: false,
    };
    let server = store
        .register_workflow_mcp_server(project.clone(), request.clone())
        .await?;
    let network_server = store
        .register_workflow_mcp_server(
            project.clone(),
            McpServerRequest {
                name: "Isolated network fixture".into(),
                outbound_network: true,
                ..request
            },
        )
        .await?;
    let binding = |server: &McpServer, destination: &str, action: &str| McpToolConfiguration {
        server_id: server.id.clone(),
        server_revision: server.revision.clone(),
        tool_name: server.tools[0].name.clone(),
        contract_revision: server.tools[0].revision.clone(),
        destination_pointer: Some("/destination".into()),
        argument_mappings: Default::default(),
        arguments: json!({"destination":destination,"action":action})
            .as_object()
            .unwrap()
            .clone(),
    };
    let config = |destination, action| binding(&server, destination, action);
    assert!(
        enqueue(
            &store,
            &repo,
            &protected,
            config("invalid inputs", "invalid action")
        )
        .await
        .is_err_and(|error| format!("{error:#}").contains("pinned MCP input schema"))
    );
    let runner = Arc::new(FixtureRunner::default());
    let scheduler = JobScheduler::start(store.clone(), runner.clone(), None);
    let validated = async {
        let denied = enqueue(&store, &repo, &protected, config("denied", "mutate")).await?;
        assert!(denied.latest_attempt.is_none());
        assert_eq!(
            store
                .workflow_mcp_job_review(project.clone(), denied.id.clone())
                .await?
                .decision,
            "pending"
        );
        store
            .decide_workflow_mcp_job(project.clone(), denied.id.clone(), false)
            .await?;
        assert!(
            store
                .job(project.clone(), denied.id)
                .await?
                .unwrap()
                .latest_attempt
                .is_none()
        );

        let granted_config = config("approved", "mutate");
        store
            .set_workflow_mcp_grant(project.clone(), granted_config.clone(), true)
            .await?;
        let wrong = enqueue(
            &store,
            &repo,
            &protected,
            config("different destination", "mutate"),
        )
        .await?;
        assert_eq!(
            store
                .workflow_mcp_job_review(project.clone(), wrong.id)
                .await?
                .decision,
            "pending"
        );
        let granted = enqueue(&store, &repo, &protected, granted_config.clone()).await?;
        assert_eq!(
            store
                .workflow_mcp_job_review(project.clone(), granted.id.clone())
                .await?
                .decision,
            "preauthorized"
        );
        let attempt = settled(&store, &granted).await?;
        assert_eq!(attempt.status, RunStatus::Succeeded);
        let evidence = store
            .workflow_mcp_attempt_evidence(project.clone(), attempt.run_id.clone())
            .await?
            .unwrap();
        assert_eq!(evidence.state, "confirmed");
        let artifact = evidence.artifact.unwrap();
        let ArtifactValue::McpToolResult(result) = artifact.value else {
            anyhow::bail!("Missing typed MCP output")
        };
        assert_eq!(
            result.output.structured,
            Some(json!({"repositoryDenied":true,"userProfileAbsent":true,"outboundCapability":false,"mutationCount":1}))
        );
        assert_eq!(
            std::fs::read_to_string(&protected)?,
            "Private fixture repository\n"
        );
        assert!(
            store
                .workflow_mcp_attempt_evidence("elsewhere".into(), attempt.run_id)
                .await?
                .is_none()
        );

        let network = enqueue(&store, &repo, &protected, binding(&network_server, "network destination", "mutate")).await?;
        store.decide_workflow_mcp_job(project.clone(), network.id.clone(), true).await?;
        let sent_over_network = settled(&store, &network).await?;
        assert_eq!(sent_over_network.status, RunStatus::Succeeded);
        let evidence = store.workflow_mcp_attempt_evidence(project.clone(), sent_over_network.run_id).await?.unwrap();
        let ArtifactValue::McpToolResult(result) = evidence.artifact.unwrap().value else {
            anyhow::bail!("Missing typed MCP output for the network-enabled server")
        };
        assert_eq!(result.output.structured, Some(json!({"repositoryDenied":true,"userProfileAbsent":true,"outboundCapability":true,"mutationCount":1})));

        runner.changed_schema.store(true, Ordering::Relaxed);
        let outdated = enqueue(&store, &repo, &protected, granted_config).await?;
        let rejected = settled(&store, &outdated).await?;
        assert_eq!(rejected.status, RunStatus::Failed);
        let drift = store
            .workflow_mcp_attempt_evidence(project.clone(), rejected.run_id)
            .await?
            .unwrap();
        assert_eq!(drift.state, "not_sent");
        assert!(drift.artifact.is_none());
        runner.changed_schema.store(false, Ordering::Relaxed);

        let mut mapped_config = config("replaced by the source", "verify_mapping");
        mapped_config.arguments.remove("destination");
        mapped_config.argument_mappings.insert("destination".into(), "/evidence/relativePath".into());
        let wrong_type = {
            let mut mapped = mapped_config.clone();
            mapped.argument_mappings.insert("destination".into(), "/evidence/baseCommit".into());
            mapped
        };
        assert!(enqueue(&store, &repo, &protected, wrong_type).await.is_err_and(|error| format!("{error:#}").contains("pinned MCP input schema")));
        let second = protected.with_file_name("second.md");
        std::fs::write(&second, "Different destination\n")?;
        let (original_run, original_source) = prepare_run(&store, &repo, &protected, mapped_config.clone()).await?;
        let (different_run, different_source) = prepare_run(&store, &repo, &second, mapped_config.clone()).await?;
        assert!(submit(&store, &repo, different_run.clone(), Some(original_source.clone())).await.is_err());
        let mapped = submit(&store, &repo, original_run, Some(original_source)).await?;
        let review = store.workflow_mcp_job_review(project.clone(), mapped.id.clone()).await?;
        assert_eq!(review.decision, "pending");
        assert_eq!(review.destination.as_deref(), Some("brief.md"));
        assert_eq!(json!(review.arguments), json!({"action":"verify_mapping", "destination":"brief.md"}));
        store.set_workflow_mcp_job_grant(project.clone(), mapped.id.clone(), true).await?;
        store.decide_workflow_mcp_job(project.clone(), mapped.id.clone(), true).await?;
        assert_eq!(settled(&store, &mapped).await?.status, RunStatus::Succeeded);
        let identical_destination = enqueue(&store, &repo, &protected, mapped_config).await?;
        assert_eq!(store.workflow_mcp_job_review(project.clone(), identical_destination.id.clone()).await?.decision, "preauthorized");
        assert_eq!(settled(&store, &identical_destination).await?.status, RunStatus::Succeeded);
        let different = submit(&store, &repo, different_run, Some(different_source)).await?;
        let different_review = store.workflow_mcp_job_review(project.clone(), different.id.clone()).await?;
        assert_eq!(different_review.destination.as_deref(), Some("second.md"));
        assert_eq!(different_review.decision, "pending");
        assert!(different.latest_attempt.is_none());

        let uncertain = enqueue(&store, &repo, &protected, config("interrupted", "hang")).await?;
        store
            .decide_workflow_mcp_job(project.clone(), uncertain.id.clone(), true)
            .await?;
        let run = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(attempt) = store
                    .job(project.clone(), uncertain.id.clone())
                    .await?
                    .unwrap()
                    .latest_attempt
                    && runner.call_received.load(Ordering::Acquire)
                    && store
                        .workflow_mcp_attempt_evidence(project.clone(), attempt.run_id.clone())
                        .await?
                        .is_some_and(|item| item.state == "sent")
                {
                    return Ok::<_, anyhow::Error>(attempt.run_id);
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        })
        .await??;
        Ok::<_, anyhow::Error>((uncertain.id, run))
    }
    .await;
    scheduler.shutdown().await?;
    let (job, run) = validated?;
    drop(store);
    let store = Store::open(database)?;
    store.recover_agent_attempts_after_restart().await?;
    let evidence = store
        .workflow_mcp_attempt_evidence(project.clone(), run.clone())
        .await?
        .unwrap();
    assert_eq!(evidence.state, "uncertain");
    assert!(evidence.artifact.is_none());
    assert_eq!(
        store
            .job(project.clone(), job.clone())
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .status,
        RunStatus::Interrupted
    );
    let environment = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: false,
    };
    let inspected = store
        .inspect_job(project.clone(), job.clone(), environment)
        .await?
        .unwrap();
    assert!(inspected.recovery_actions.is_empty());
    assert!(
        store
            .queue_job_recovery(
                project.clone(),
                job.clone(),
                run.clone(),
                RecoveryAction::Retry
            )
            .await
            .is_err()
    );
    store
        .reconcile_workflow_mcp_attempt(project.clone(), run.clone())
        .await?;
    assert_eq!(
        store
            .workflow_mcp_attempt_evidence(project.clone(), run)
            .await?
            .unwrap()
            .state,
        "reconciled"
    );
    assert_eq!(
        store
            .job(project.clone(), job)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .ordinal,
        1
    );
    Ok(())
}
