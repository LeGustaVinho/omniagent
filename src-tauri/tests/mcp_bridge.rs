use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    conversations::{AttemptPlan, SessionScope},
    dispatch::EventSource,
    jobs::{
        AdmissionBlocker, AdmissionEnvironment, DownstreamWaitState, JobContext, JobPermission,
        JobRequest, JobRole, JobRule, JobWorkspace, PipelineDispatch, PipelineNodeKind, QueueState,
        RecoveryAction, ReleaseBlocker,
    },
    mcp_bridge::{HELPER_FLAG, RunBridge},
    process_io::{CapturedProcess, Stream},
    providers::{
        Capability, Provider, ProviderEvent, ProviderEventData, SessionMode, WireMessage,
        capabilities::{
            Availability, CapabilityDescriptor, CapabilityScope, CapabilitySubject,
            CheckoutIdentity, DeclaredSource, EvidenceBoundary, ObservedResult, PolicyBlocker,
        },
    },
    repository,
    repository::verified_git::DeliveryGitPolicy,
    runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner},
    runs::RunStatus,
    store::Store,
    workflows::{
        DeliveryRoles, NodeInputs, WorkflowAgentSubmission, WorkflowHostRequest,
        WorkflowHostSubmission, builtin_delivery_graph,
    },
};
use serde_json::{Value, json};

async fn attempt(store: &Store, checkout: PathBuf) -> Result<AttemptPlan> {
    fs::create_dir(&checkout)?;
    let checkout = dunce::canonicalize(checkout)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()?
            .success()
    );
    new_attempt(store, checkout).await
}

async fn new_attempt(store: &Store, checkout: PathBuf) -> Result<AttemptPlan> {
    let project = repository::register(store, checkout.clone()).await?;
    let conversation = store
        .create_conversation(project.checkout_id, "Bridge fixture".into())
        .await?;
    let plan = store
        .prepare_agent_attempt(
            conversation.id,
            conversation.context_id,
            SessionScope {
                provider: Provider::Codex,
                profile_id: "bridge-fixture".into(),
                account_binding: None,
                executable: env!("CARGO_BIN_EXE_omniagent").into(),
                checkout,
                cli_version: "fixture".into(),
                runner: DirectWindowsRunner.identity().into(),
                permission_revision: "read-only".into(),
                mode: SessionMode::ReadOnly,
                model: None,
                effort: None,
            },
            "inspect scope".into(),
        )
        .await?;
    store.mark_agent_running(plan.run.id.clone()).await?;
    Ok(plan)
}

fn receive(process: &CapturedProcess, id: usize) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))??;
        ensure!(
            frame.stream == Stream::Stdout,
            "MCP helper stderr: {}",
            frame.data
        );
        let value: Value = serde_json::from_str(&frame.data)?;
        if value["id"] == id {
            return Ok(value);
        }
    }
}

fn send(process: &mut CapturedProcess, frame: Value) -> Result<()> {
    Ok(process.input.send(WireMessage::json(&frame)?.as_bytes())?)
}

async fn start_helper(ticket: PathBuf, checkout: PathBuf) -> Result<CapturedProcess> {
    tokio::task::spawn_blocking(move || {
        let mut process = DirectWindowsRunner.launch(LaunchRequest {
            version: CONTRACT_VERSION,
            executable: Path::new(env!("CARGO_BIN_EXE_omniagent")),
            checkout: &checkout,
            arguments: &[OsStr::new(HELPER_FLAG), ticket.as_os_str()],
        })?;
        send(&mut process, json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"omniagent-test","version":"1"}
        }}))?;
        ensure!(receive(&process, 1)?["result"]["capabilities"]["tools"].is_object());
        send(&mut process, json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))?;
        send(&mut process, json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))?;
        let listing = receive(&process, 2)?;
        ensure!(listing["result"]["tools"][0]["name"] == "inspect_run_scope");
        ensure!(listing["result"]["tools"].as_array().is_some_and(|tools| tools.iter()
            .any(|tool| tool["name"] == "inspect_omniagent" && tool["annotations"]["readOnlyHint"] == true)));
        Ok(process)
    }).await?
}

#[tokio::test]
async fn runtime_inspection_is_bounded_redacted_and_audited_on_the_bound_attempt() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let fixture = attempt(&store, directory.path().join("source")).await?;
    let foreign = attempt(&store, directory.path().join("foreign")).await?;
    let project = fixture.run.project_id.clone();
    let task = store
        .create_project_task(
            project.clone(),
            "Inspection fixture api_key=task-canary".into(),
        )
        .await?;
    let pipeline = "inspection-pipeline".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "inspect".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    let request = JobRequest {
        checkout_id: Some(fixture.run.checkout_id.clone()),
        task_id: Some(task.id.clone()),
        mode: SessionMode::ReadOnly,
        instructions: "private-instructions-canary".into(),
        dependencies: vec![],
        requires_bridge: false,
    };
    let root = store
        .submit_pipeline_agent_job(
            PipelineDispatch {
                pipeline_run_id: pipeline,
                node_instance_id: "inspect".into(),
                plan_revision: None,
                release_id: None,
            },
            SessionScope {
                account_binding: Some("private-account-canary".into()),
                ..fixture.scope.clone()
            },
            request.clone(),
        )
        .await?;
    let env = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: true,
    };
    let active = store
        .claim_next_job(env.clone())
        .await?
        .context("The scoped fixture was not admitted")?;
    store.mark_agent_running(active.run.id.clone()).await?;
    let publish = |run: String, source, data| {
        store.record_agent_observation(
            run,
            source,
            ProviderEvent {
                location: Default::default(),
                data,
            },
        )
    };
    let scope = CapabilityScope {
        provider: active.scope.provider,
        transport: active.scope.provider.adapter().transport(),
        cli_version: active.scope.cli_version.clone(),
        executable_revision: None,
        profile_id: active.scope.profile_id.clone(),
        account_binding: active.scope.account_binding.clone(),
        requested_model: None,
        reported_model: None,
        requested_effort: None,
        reported_effort: None,
        runner: active.scope.runner.clone(),
        platform: DirectWindowsRunner.execution_platform(),
        mode: active.scope.mode,
        permission_revision: active.scope.permission_revision.clone(),
        checkout: CheckoutIdentity {
            id: active.run.checkout_id.clone(),
            path: active.scope.checkout.clone(),
        },
        configuration: serde_json::from_value(json!({
            "detected": [], "unresolved": [], "providerRevision": null, "effectiveRevision": null,
        }))?,
    };
    let assessment = store
        .resolve_capability_assessment(
            scope,
            vec![
                CapabilityDescriptor {
                    subject: CapabilitySubject::Protocol {
                        capability: Capability::RunScopedMcp,
                    },
                    source: DeclaredSource::Provider,
                    availability: Availability::Available,
                    observed: Some(ObservedResult::Succeeded),
                    blocker: None,
                    boundary: EvidenceBoundary::ExactCheckout,
                },
                CapabilityDescriptor {
                    subject: CapabilitySubject::Protocol {
                        capability: Capability::WorkspaceWrite,
                    },
                    source: DeclaredSource::Policy,
                    availability: Availability::Unavailable,
                    observed: None,
                    blocker: Some(PolicyBlocker::ReadOnlyProfile),
                    boundary: EvidenceBoundary::ExactCheckout,
                },
                CapabilityDescriptor {
                    subject: CapabilitySubject::Tool {
                        id: "private-tool-canary".into(),
                        version: "1".into(),
                        operation: "list".into(),
                    },
                    source: DeclaredSource::Provider,
                    availability: Availability::Available,
                    observed: None,
                    blocker: None,
                    boundary: EvidenceBoundary::ExactCheckout,
                },
            ],
        )
        .await?;
    publish(
        active.run.id.clone(),
        EventSource::Application,
        ProviderEventData::CapabilityAssessmentUpdated {
            assessment: Box::new(assessment),
        },
    )
    .await?;
    publish(
        active.run.id.clone(),
        EventSource::Provider,
        ProviderEventData::AssistantMessage {
            text: "private-transcript-canary".into(),
        },
    )
    .await?;
    publish(
        fixture.run.id.clone(),
        EventSource::Provider,
        ProviderEventData::Diagnostic {
            message: "sibling-diagnostic-canary".into(),
        },
    )
    .await?;
    for index in 0..6 {
        publish(
            active.run.id.clone(),
            EventSource::Provider,
            ProviderEventData::Diagnostic {
                message: format!(
                    "Fixture diagnostic {index} Authorization: Bearer credential-canary"
                ),
            },
        )
        .await?;
    }
    let sibling = store
        .submit_agent_job(fixture.run.id, "sibling".into(), request.clone())
        .await?;
    let mut children = Vec::new();
    for index in 0..8 {
        let child = store
            .submit_agent_job(
                active.run.id.clone(),
                format!("child-{index}"),
                request.clone(),
            )
            .await?;
        children.push(child.id);
    }
    let bridge = RunBridge::start(&store, active.run.id.clone()).await?;
    let helper = start_helper(bridge.ticket_path().into(), active.scope.checkout.clone()).await?;
    let (helper, spoofed) = invoke_tool(
        helper,
        "inspect_omniagent",
        json!({
            "runId": foreign.run.id, "projectId": foreign.run.project_id,
        }),
    )
    .await?;
    assert!(spoofed["error"].is_object());
    let (helper, protected_query) = invoke_tool(
        helper,
        "inspect_omniagent",
        json!({ "includeTranscript": true }),
    )
    .await?;
    assert!(protected_query["error"].is_object());
    let (helper, result) = invoke_tool(helper, "inspect_omniagent", json!({})).await?;
    let body = result["result"]["structuredContent"]
        .as_object()
        .with_context(|| format!("Inspection failed {result}"))?;
    assert_eq!(body["version"], 1);
    assert!(body.get("workflow").is_none());
    assert_eq!(body["build"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["build"]["target"], env!("OMNIAGENT_BUILD_TARGET"));
    assert_eq!(body["build"]["profile"], env!("OMNIAGENT_BUILD_PROFILE"));
    let build_commit = env!("OMNIAGENT_BUILD_COMMIT");
    assert_eq!(
        body["build"]["sourceCommit"].as_str(),
        (!build_commit.is_empty()).then_some(build_commit)
    );
    assert_eq!(body["run"]["id"], active.run.id);
    assert_eq!(body["run"]["projectId"], project);
    assert_eq!(body["profile"]["mode"], "read_only");
    assert_eq!(body["policy"]["role"], "pipeline");
    assert_eq!(body["policy"]["permission"], "allow");
    assert_eq!(body["policy"]["admittedRevision"], "default");
    assert_eq!(body["settingsRevision"].as_str().unwrap().len(), 64);
    assert_eq!(
        body["capabilities"]["protocol"],
        json!({ "run_scoped_mcp": "observed", "workspace_write": "blocked" })
    );
    assert_eq!(
        body["task"],
        json!({ "id": task.id, "title": "Inspection fixture [redacted]" })
    );
    let runtime = store.inspect_project_queue(project, env).await?;
    assert_eq!(
        body["queue"]["settings"],
        serde_json::to_value(runtime.settings)?
    );
    let current = runtime
        .jobs
        .iter()
        .find(|entry| entry.job.id == root.id)
        .context("The runtime did not return the current job")?;
    assert_eq!(body["queue"]["current"]["id"], current.job.id);
    assert_eq!(
        body["queue"]["current"]["state"],
        serde_json::to_value(current.job.queue_state)?
    );
    assert_eq!(
        body["queue"]["current"]["runStatus"],
        serde_json::to_value(
            current
                .job
                .latest_attempt
                .as_ref()
                .map(|attempt| attempt.status)
        )?
    );
    assert_eq!(body["queue"]["moreSubmitted"], true);
    let submitted = body["queue"]["submitted"].as_array().unwrap();
    assert_eq!(
        submitted
            .iter()
            .map(|job| job["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        children[2..]
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    for disclosed in submitted {
        let entry = runtime
            .jobs
            .iter()
            .find(|entry| disclosed["id"] == entry.job.id)
            .context("The bridge returned a job missing from the runtime queue")?;
        assert_eq!(
            disclosed["taskId"],
            serde_json::to_value(&entry.job.task_id)?
        );
        assert_eq!(
            disclosed["state"],
            serde_json::to_value(entry.job.queue_state)?
        );
        assert!(matches!(
            entry.blockers.as_slice(),
            [AdmissionBlocker::Approval { .. }]
        ));
        assert_eq!(disclosed["blockers"], json!(["approval"]));
    }
    let diagnostics = body["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 4);
    assert_eq!(body["moreDiagnostics"], true);
    assert_eq!(diagnostics[0]["message"], "Fixture diagnostic 5 [redacted]");
    let serialized = serde_json::to_string(body)?;
    for protected in [
        sibling.id.as_str(),
        foreign.run.id.as_str(),
        foreign.run.project_id.as_str(),
        "credential-canary",
        "task-canary",
        "private-instructions-canary",
        "private-transcript-canary",
        "private-account-canary",
        "private-tool-canary",
        "sibling-diagnostic-canary",
    ] {
        assert!(!serialized.contains(protected));
    }
    let audit = store
        .agent_observations(active.conversation_id, active.run.id.clone(), 0)
        .await?
        .into_iter()
        .filter(|event| matches!(event.event.data, ProviderEventData::RuntimeInspected { .. }))
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].source, EventSource::Application);
    assert!(matches!(
        audit[0].event.data,
        ProviderEventData::RuntimeInspected {
            jobs: 7,
            diagnostics: 4,
            task: true,
            capabilities: true
        }
    ));
    store
        .finish_agent_attempt(active.run.id, RunStatus::Cancelled, None)
        .await?;
    let (_helper, expired) = invoke_tool(helper, "inspect_omniagent", json!({})).await?;
    assert!(expired["error"].is_object());
    Ok(())
}

#[tokio::test]
async fn runtime_inspection_reports_the_admitted_workflow_stage_and_live_settings() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("workflow-inspection.sqlite"))?;
    let fixture = attempt(&store, directory.path().join("source")).await?;
    let project = fixture.run.project_id.clone();
    let stage = "pinned-planning-stage";
    let mut graph = builtin_delivery_graph(DeliveryRoles {
        planner: "private-role-definition-canary".into(),
        ..DeliveryRoles::default()
    });
    graph.name = "Pinned workflow api_key=workflow-secret-canary".into();
    graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap()
        .id = stage.into();
    for wire in &mut graph.wires {
        for endpoint in [&mut wire.from, &mut wire.to] {
            if endpoint.node == "plan" {
                endpoint.node = stage.into();
            }
        }
    }
    let workflow_id = "bridge-inspected-workflow";
    let revision = store
        .save_workflow_revision(project.clone(), workflow_id.into(), graph.clone())
        .await?;
    let run = store
        .start_workflow_run(project.clone(), revision.clone())
        .await?;
    let document_path = fixture.scope.checkout.join("workflow.md");
    fs::write(&document_path, "private-markdown-body-canary")?;
    let document = store
        .capture_repository_markdown(fixture.run.checkout_id.clone(), document_path)
        .await?;
    let host = |node: &str, inputs, request| WorkflowHostSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: node.into(),
        inputs,
        request,
    };
    let source = store
        .execute_workflow_host(host(
            "start",
            NodeInputs::new(),
            WorkflowHostRequest::Start {
                document: document.revision,
            },
        ))
        .await?;
    let context = store
        .execute_workflow_host(host(
            "context",
            NodeInputs::from([("document".into(), source.id)]),
            WorkflowHostRequest::LoadContext,
        ))
        .await?;
    let job = store
        .submit_workflow_agent(WorkflowAgentSubmission {
            project_id: project.clone(),
            dispatch: PipelineDispatch {
                pipeline_run_id: run.id.clone(),
                node_instance_id: stage.into(),
                plan_revision: None,
                release_id: None,
            },
            scope: SessionScope {
                mode: SessionMode::Plan,
                account_binding: Some("private-runtime-account-canary".into()),
                ..fixture.scope
            },
            request: JobRequest {
                checkout_id: Some(fixture.run.checkout_id),
                task_id: None,
                mode: SessionMode::Plan,
                instructions: "private-workflow-instructions-canary".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
            input: Default::default(),
            artifacts: NodeInputs::from([("context".into(), context.id)]),
            unit_id: None,
        })
        .await?;
    let active = store
        .claim_next_job(AdmissionEnvironment {
            runner: DirectWindowsRunner.identity().into(),
            bridge_available: true,
        })
        .await?
        .context("The authorized planner was not admitted")?;
    store.mark_agent_running(active.run.id.clone()).await?;
    let pin = store
        .workflow_attempt(project.clone(), active.run.id.clone())
        .await?
        .context("The admitted planner has no workflow pin")?
        .pin;

    graph.name = "unadmitted-workflow-revision-canary".into();
    let later = store
        .save_workflow_revision(project.clone(), workflow_id.into(), graph)
        .await?;
    assert_ne!(later.revision, revision.revision);
    let bridge = RunBridge::start(&store, active.run.id.clone()).await?;
    let helper = start_helper(bridge.ticket_path().into(), active.scope.checkout.clone()).await?;
    let (helper, result) = invoke_tool(helper, "inspect_omniagent", json!({})).await?;
    let body = &result["result"]["structuredContent"];
    assert!(body.is_object(), "Inspection failed {result}");
    assert_eq!(body["run"]["id"], active.run.id);
    assert_eq!(
        body["workflow"],
        json!({
            "runId": run.id,
            "id": workflow_id,
            "revision": revision.revision,
            "name": "Pinned workflow [redacted]",
            "node": {
                "id": stage,
                "instanceRevision": pin.instance_revision,
                "definitionRevision": pin.definition_revision,
                "title": "Plan",
                "kind": "plan",
                "executor": "plan_agent",
                "effect": "invoke_model",
            },
            "ordinal": 1,
            "status": "running",
            "jobId": job.id,
            "previousRunId": null,
            "planRevision": null,
            "planCurrent": null,
            "releaseId": null,
            "releaseRevoked": null,
            "unitId": null,
            "gitPolicy": "automatic_commit_user_merge",
            "completionMilestone": "verified_commit",
        })
    );
    let serialized = serde_json::to_string(body)?;
    for private in [
        "workflow-secret-canary",
        "private-role-definition-canary",
        "private-markdown-body-canary",
        "private-runtime-account-canary",
        "private-workflow-instructions-canary",
        "unadmitted-workflow-revision-canary",
        &later.revision,
    ] {
        assert!(!serialized.contains(private));
    }

    store
        .set_delivery_git_policy(project, DeliveryGitPolicy::UserCommitsAndMerges)
        .await?;
    let (_helper, result) = invoke_tool(helper, "inspect_omniagent", json!({})).await?;
    let refreshed = &result["result"]["structuredContent"];
    assert_eq!(refreshed["workflow"]["revision"], revision.revision);
    assert_eq!(
        refreshed["workflow"]["gitPolicy"],
        "user_commits_and_merges"
    );
    assert_ne!(refreshed["settingsRevision"], body["settingsRevision"]);
    Ok(())
}

async fn invoke(process: CapturedProcess, arguments: Value) -> Result<(CapturedProcess, Value)> {
    invoke_tool(process, "inspect_run_scope", arguments).await
}

async fn invoke_tool(
    mut process: CapturedProcess,
    name: &'static str,
    arguments: Value,
) -> Result<(CapturedProcess, Value)> {
    tokio::task::spawn_blocking(move || {
        send(
            &mut process,
            json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{
                "name":name, "arguments":arguments
            }}),
        )?;
        let response = receive(&process, 3).context("scope tool response")?;
        Ok((process, response))
    })
    .await?
}

fn expect_scope(response: &Value, attempt: &AttemptPlan) {
    assert_eq!(
        response["result"]["structuredContent"],
        json!({
            "runId":attempt.run.id, "projectId":attempt.run.project_id,
            "checkoutId":attempt.run.checkout_id, "turnId":attempt.run.turn_id,
        })
    );
}

#[tokio::test]
async fn wait_for_jobs_authenticates_children_and_leaves_an_unconfirmed_restart_for_review()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("wait.sqlite"))?;
    let earlier = attempt(&store, directory.path().join("project")).await?;
    let project = earlier.run.project_id.clone();
    let conversation = store
        .create_conversation(earlier.run.checkout_id, "Wait fixture".into())
        .await?;
    let text = "Wait for a delegated result".to_owned();
    store
        .save_conversation_draft(conversation.id.clone(), text.clone())
        .await?;
    let parent = store
        .enqueue_conversation_turn(
            JobContext {
                conversation_id: conversation.id,
                context_id: conversation.context_id,
            },
            earlier.scope,
            text,
        )
        .await?;
    let env = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: true,
    };
    let active = store
        .claim_next_job(env.clone())
        .await?
        .context("The wait fixture was not admitted")?;
    store.mark_agent_running(active.run.id.clone()).await?;
    let request = JobRequest {
        checkout_id: None,
        task_id: None,
        mode: SessionMode::ReadOnly,
        instructions: "Return a read-only report".into(),
        dependencies: vec![],
        requires_bridge: false,
    };
    let own = store
        .submit_agent_job(active.run.id.clone(), "own".into(), request.clone())
        .await?;
    let cycle = store
        .submit_agent_job(
            active.run.id.clone(),
            "cycle".into(),
            JobRequest {
                dependencies: vec![parent.id.clone()],
                ..request.clone()
            },
        )
        .await?;
    let other = store
        .submit_agent_job(earlier.run.id, "other".into(), request)
        .await?;
    let bridge = RunBridge::start(&store, active.run.id.clone()).await?;
    let helper = start_helper(bridge.ticket_path().into(), active.scope.checkout.clone()).await?;
    let (helper, unauthorized) = invoke_tool(
        helper,
        "wait_for_jobs",
        json!({
            "requestId": "bad-scope", "childJobIds": [other.id],
        }),
    )
    .await?;
    assert!(unauthorized["error"].is_object());
    let (helper, cycle) = invoke_tool(
        helper,
        "wait_for_jobs",
        json!({
            "requestId": "bad-dependency", "childJobIds": [cycle.id],
        }),
    )
    .await?;
    assert!(cycle["error"].is_object());
    let request = json!({ "requestId": "wait-once", "childJobIds": [own.id] });
    let (helper, accepted) = invoke_tool(helper, "wait_for_jobs", request.clone()).await?;
    let wait = store
        .downstream_wait(project.clone(), active.run.id.clone())
        .await?
        .context("The wait was not persisted")?;
    assert_eq!(accepted["result"]["structuredContent"]["waitId"], wait.id);
    assert_eq!(accepted["result"]["structuredContent"]["state"], "yielding");
    let (_helper, repeated) = invoke_tool(helper, "wait_for_jobs", request).await?;
    assert_eq!(repeated["result"]["structuredContent"]["waitId"], wait.id);
    store.recover_agent_attempts_after_restart().await?;
    let recovered = store
        .downstream_wait(project.clone(), active.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(recovered.state, DownstreamWaitState::NeedsReview);
    assert!(recovered.continuation_run_id.is_none());
    let listed = store
        .inspect_job(project.clone(), parent.id.clone(), env.clone())
        .await?
        .unwrap();
    assert!(listed.blockers.iter().any(|blocker| matches!(
        blocker,
        AdmissionBlocker::Downstream {
            needs_review: true,
            ..
        }
    )));
    assert!(listed.recovery_actions.is_empty());
    assert!(store.claim_next_job(env).await?.is_none());
    store
        .set_pending_job_state(
            project.clone(),
            parent.id,
            omniagent::jobs::QueueState::Cancelled,
        )
        .await?;
    assert_eq!(
        store
            .downstream_wait(project, active.run.id)
            .await?
            .unwrap()
            .state,
        DownstreamWaitState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn stdio_helpers_report_only_the_identity_of_their_active_attempt() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let first = attempt(&store, directory.path().join("alpha")).await?;
    let second = attempt(&store, directory.path().join("beta")).await?;
    assert_ne!(first.run.project_id, second.run.project_id);
    let first_bridge = RunBridge::start(&store, first.run.id.clone()).await?;
    let second_bridge = RunBridge::start(&store, second.run.id.clone()).await?;
    let first_helper = start_helper(
        first_bridge.ticket_path().into(),
        first.scope.checkout.clone(),
    )
    .await?;
    let second_helper = start_helper(
        second_bridge.ticket_path().into(),
        second.scope.checkout.clone(),
    )
    .await?;
    let (first_helper, response) = invoke(first_helper, json!({})).await?;
    expect_scope(&response, &first);
    let (first_helper, response) = invoke(first_helper, json!({"runId":second.run.id})).await?;
    assert!(response["error"].is_object());
    let (second_helper, response) = invoke(second_helper, json!({})).await?;
    expect_scope(&response, &second);

    store
        .finish_agent_attempt(first.run.id.clone(), RunStatus::Cancelled, None)
        .await?;
    let (_first_helper, response) = invoke(first_helper, json!({})).await?;
    assert!(response["error"].is_object());
    let (_second_helper, response) = invoke(second_helper, json!({})).await?;
    expect_scope(&response, &second);

    let first_ticket = first_bridge.ticket_path().to_path_buf();
    drop(first_bridge);
    assert!(!first_ticket.exists());
    assert!(
        RunBridge::start(&store, first.run.id.clone())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn run_bound_mcp_submits_jobs_idempotently_and_only_discloses_its_own_children() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let parent = attempt(&store, directory.path().join("parent")).await?;
    let stranger = new_attempt(&store, parent.scope.checkout.clone()).await?;
    let foreign = attempt(&store, directory.path().join("foreign")).await?;
    let task = store
        .create_project_task(parent.run.project_id.clone(), "Parent project task".into())
        .await?;
    let foreign_task = store
        .create_project_task(foreign.run.project_id, "Other project task".into())
        .await?;
    let bridge = RunBridge::start(&store, parent.run.id.clone()).await?;
    let stranger_bridge = RunBridge::start(&store, stranger.run.id.clone()).await?;
    let stranger_helper = start_helper(
        stranger_bridge.ticket_path().into(),
        stranger.scope.checkout,
    )
    .await?;
    let helper = start_helper(bridge.ticket_path().into(), parent.scope.checkout).await?;
    let request = json!({"requestId":"child-once", "request":{
        "mode":"plan", "instructions":"Plan a bounded test change", "taskId":task.id,
    }});
    let (helper, submitted) = invoke_tool(helper, "submit_job", request.clone()).await?;
    let id = submitted["result"]["structuredContent"]["jobId"]
        .as_str()
        .with_context(|| format!("submit response: {submitted}"))?;
    let (helper, replay) = invoke_tool(helper, "submit_job", request.clone()).await?;
    assert_eq!(replay["result"]["structuredContent"]["jobId"], id);

    let mut changed = request.clone();
    changed["request"]["instructions"] = json!("Reused ID with different input");
    let (helper, conflict) = invoke_tool(helper, "submit_job", changed).await?;
    assert!(
        conflict["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("already used")
    );
    let (helper, status) = invoke_tool(helper, "get_job_status", json!({"jobId":id})).await?;
    let current = &status["result"]["structuredContent"];
    assert_eq!(current["id"], id);
    assert_eq!(current["projectId"], parent.run.project_id);
    assert_eq!(current["taskId"], task.id);
    assert_eq!(current["origin"]["runId"], parent.run.id);
    assert_eq!(current["context"]["contextId"], parent.context_id);
    assert_eq!(current["queueState"], "queued");
    assert!(current.get("instructions").is_none());

    let (_stranger_helper, denied) =
        invoke_tool(stranger_helper, "get_job_status", json!({"jobId":id})).await?;
    assert!(denied["error"].is_object());
    let (helper, spoofed) = invoke_tool(
        helper,
        "get_job_status",
        json!({"jobId":id,"runId":stranger.run.id}),
    )
    .await?;
    assert!(spoofed["error"].is_object());
    let (helper, foreign_task) = invoke_tool(
        helper,
        "submit_job",
        json!({"requestId":"foreign-task", "request":{
            "mode":"plan", "instructions":"Try another project", "taskId":foreign_task.id,
        }}),
    )
    .await?;
    assert!(foreign_task["error"].is_object());
    let (helper, foreign_checkout) = invoke_tool(helper, "submit_job", json!({"requestId":"foreign-checkout", "request":{
        "mode":"plan", "instructions":"Try another checkout", "checkoutId":foreign.run.checkout_id,
    }})).await?;
    assert!(foreign_checkout["error"].is_object());

    store
        .finish_agent_attempt(parent.run.id, RunStatus::Cancelled, None)
        .await?;
    let (_helper, expired) = invoke_tool(helper, "get_job_status", json!({"jobId":id})).await?;
    assert!(expired["error"].is_object());
    Ok(())
}

#[tokio::test]
async fn mcp_children_inherit_the_parent_task_and_follow_operator_queue_controls() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("bridge.sqlite"))?;
    let fixture = attempt(&store, directory.path().join("source")).await?;
    store
        .finish_agent_attempt(fixture.run.id, RunStatus::Cancelled, None)
        .await?;
    let project = fixture.run.project_id;
    let checkout = fixture.run.checkout_id;
    let task = store
        .create_project_task(project.clone(), "Parent task".into())
        .await?;
    let pipeline = "delegation-fixture".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "parent".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    store
        .set_project_job_rules(
            project.clone(),
            vec![],
            vec![JobRule {
                role: JobRole::Agent,
                operation: SessionMode::Plan,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    store
        .submit_pipeline_agent_job(
            PipelineDispatch {
                pipeline_run_id: pipeline,
                node_instance_id: "parent".into(),
                plan_revision: None,
                release_id: None,
            },
            fixture.scope,
            JobRequest {
                checkout_id: Some(checkout),
                task_id: Some(task.id.clone()),
                mode: SessionMode::ReadOnly,
                instructions: "Delegate the selected task".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
        )
        .await?;
    let environment = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: true,
    };
    let parent = store
        .claim_next_job(environment.clone())
        .await?
        .context("the parent did not start")?;
    store.mark_agent_running(parent.run.id.clone()).await?;
    let bridge = RunBridge::start(&store, parent.run.id.clone()).await?;
    let helper = start_helper(bridge.ticket_path().into(), parent.scope.checkout).await?;
    let request = json!({
        "requestId":"allowed-child",
        "request":{"mode":"plan","instructions":"Plan the inherited task"}
    });
    let (helper, submitted) = invoke_tool(helper, "submit_job", request.clone()).await?;
    let child = submitted
        .pointer("/result/structuredContent/jobId")
        .and_then(Value::as_str)
        .with_context(|| format!("child submission failed: {submitted}"))?
        .to_owned();
    let (helper, replay) = invoke_tool(helper, "submit_job", request).await?;
    assert_eq!(replay["result"]["structuredContent"]["jobId"], child);
    let (helper, status) = invoke_tool(helper, "get_job_status", json!({"jobId":child})).await?;
    let status = &status["result"]["structuredContent"];
    assert_eq!(status["taskId"], task.id);
    assert_eq!(status["origin"]["runId"], parent.run.id);

    let inspect = |id: String| store.inspect_job(project.clone(), id, environment.clone());
    assert!(inspect(child.clone()).await?.unwrap().blockers.is_empty());
    store.set_job_queue_paused(true).await?;
    assert_eq!(
        inspect(child.clone()).await?.unwrap().blockers,
        [AdmissionBlocker::QueuePaused]
    );
    assert!(store.claim_next_job(environment.clone()).await?.is_none());
    store
        .set_pending_job_state(project.clone(), child.clone(), QueueState::Paused)
        .await?;
    store.set_job_queue_paused(false).await?;
    assert_eq!(
        inspect(child.clone()).await?.unwrap().blockers,
        [AdmissionBlocker::JobPaused]
    );
    assert!(store.claim_next_job(environment.clone()).await?.is_none());
    store
        .set_pending_job_state(project.clone(), child.clone(), QueueState::Queued)
        .await?;
    let first = store
        .claim_next_job(environment.clone())
        .await?
        .context("the allowed child did not start")?;
    store.mark_agent_running(first.run.id.clone()).await?;
    store
        .request_job_interrupt(first.conversation_id, first.run.id.clone())
        .await?;
    let (mut helper, stopping) =
        invoke_tool(helper, "get_job_status", json!({"jobId":child})).await?;
    let live = &stopping["result"]["structuredContent"]["latestAttempt"];
    assert_eq!(live["runId"], first.run.id);
    assert_eq!(live["status"], "running");
    assert_eq!(live["cancelRequested"], true);
    store
        .finish_agent_attempt(
            first.run.id.clone(),
            RunStatus::Cancelled,
            Some("Partial child result".into()),
        )
        .await?;
    assert_eq!(
        inspect(child.clone()).await?.unwrap().recovery_actions,
        [RecoveryAction::Retry, RecoveryAction::Resume]
    );
    store
        .queue_job_recovery(
            project.clone(),
            child.clone(),
            first.run.id.clone(),
            RecoveryAction::Resume,
        )
        .await?;
    let resumed = store
        .claim_next_job(environment.clone())
        .await?
        .context("the child did not resume")?;
    assert_eq!(resumed.previous_run_id, Some(first.run.id));
    assert!(
        resumed
            .input
            .iter()
            .any(|message| message.text == "Partial child result")
    );
    store
        .finish_agent_attempt(resumed.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            project.clone(),
            child.clone(),
            resumed.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let retried = store
        .claim_next_job(environment.clone())
        .await?
        .context("the child did not retry")?;
    assert_eq!(retried.previous_run_id, Some(resumed.run.id));
    assert_eq!(retried.input, resumed.input);
    store.mark_agent_running(retried.run.id.clone()).await?;
    store
        .finish_agent_attempt(retried.run.id, RunStatus::Succeeded, None)
        .await?;

    for (request_id, approved) in [("approved-child", true), ("declined-child", false)] {
        let request = json!({"requestId":request_id, "request":{
            "mode":"read_only", "instructions":"Read the inherited task"
        }});
        let (next_helper, submitted) = invoke_tool(helper, "submit_job", request).await?;
        helper = next_helper;
        let child = submitted
            .pointer("/result/structuredContent/jobId")
            .and_then(Value::as_str)
            .with_context(|| format!("review submission failed: {submitted}"))?
            .to_owned();
        let pending = inspect(child.clone()).await?.unwrap();
        assert_eq!(pending.job.task_id.as_deref(), Some(task.id.as_str()));
        let [AdmissionBlocker::Approval { approval_id }] = pending.blockers.as_slice() else {
            anyhow::bail!("the review child did not wait for approval");
        };
        assert!(store.claim_next_job(environment.clone()).await?.is_none());
        store
            .decide_job_approval(project.clone(), approval_id.clone(), approved)
            .await?;
        if approved {
            let reviewed = store
                .claim_next_job(environment.clone())
                .await?
                .context("the approved child did not start")?;
            let current = inspect(child).await?.unwrap().job.latest_attempt.unwrap();
            assert_eq!(current.run_id, reviewed.run.id);
            store.mark_agent_running(reviewed.run.id.clone()).await?;
            store
                .finish_agent_attempt(reviewed.run.id, RunStatus::Succeeded, None)
                .await?;
        } else {
            assert_eq!(
                inspect(child.clone()).await?.unwrap().blockers,
                [AdmissionBlocker::Policy {
                    reason: "An operator declined this job",
                }]
            );
            assert!(store.claim_next_job(environment.clone()).await?.is_none());
            store
                .set_pending_job_state(project.clone(), child.clone(), QueueState::Cancelled)
                .await?;
            let (next_helper, status) =
                invoke_tool(helper, "get_job_status", json!({"jobId":child})).await?;
            helper = next_helper;
            assert_eq!(
                status["result"]["structuredContent"]["queueState"],
                "cancelled"
            );
        }
    }
    store
        .finish_agent_attempt(parent.run.id, RunStatus::Succeeded, None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn pipeline_release_scope_is_inherited_by_mcp_descendants_and_rechecked_on_retry()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let db = directory.path().join("bridge.sqlite");
    let store = Store::open(db.clone())?;
    let fixture = attempt(&store, directory.path().join("source")).await?;
    ensure!(
        Command::new("git.exe")
            .current_dir(&fixture.scope.checkout)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=NUL",
                "commit",
                "--allow-empty",
                "-qm",
                "Base"
            ])
            .status()?
            .success()
    );
    let project = fixture.run.project_id.clone();
    let task = store
        .create_project_task(project.clone(), "Approved task".into())
        .await?;
    let worktree = repository::provision_task_worktree(
        &store,
        directory.path().join("worktrees"),
        project.clone(),
        task.id.clone(),
        fixture.run.checkout_id,
    )
    .await?;
    let allow = [JobRole::Pipeline, JobRole::Agent]
        .into_iter()
        .flat_map(|role| {
            [SessionMode::ReadOnly, SessionMode::ApprovedWrite]
                .into_iter()
                .map(move |operation| JobRule {
                    role,
                    operation,
                    workspace: JobWorkspace::Project,
                    permission: JobPermission::Allow,
                    requirements: Default::default(),
                })
        })
        .collect();
    store
        .set_project_job_rules(project.clone(), vec![], allow)
        .await?;
    let env = AdmissionEnvironment {
        runner: DirectWindowsRunner.identity().into(),
        bridge_available: true,
    };
    let pipeline = "pipeline-run".to_owned();
    store
        .register_job_pipeline(project.clone(), pipeline.clone())
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "implement".into(),
            PipelineNodeKind::Execution,
        )
        .await?;
    store
        .register_job_pipeline_node(
            project.clone(),
            pipeline.clone(),
            "approve".into(),
            PipelineNodeKind::ImplementationApproval,
        )
        .await?;
    let submit = |plan: &str, release_id: Option<String>, mode| {
        store.submit_pipeline_agent_job(
            PipelineDispatch {
                pipeline_run_id: pipeline.clone(),
                node_instance_id: "implement".into(),
                plan_revision: Some(plan.into()),
                release_id,
            },
            SessionScope {
                mode,
                checkout: worktree.root.clone(),
                ..fixture.scope.clone()
            },
            JobRequest {
                checkout_id: Some(worktree.checkout_id.clone()),
                task_id: Some(task.id.clone()),
                mode,
                instructions: "Use the approved plan".into(),
                dependencies: vec![],
                requires_bridge: false,
            },
        )
    };
    store
        .set_job_pipeline_plan(project.clone(), pipeline.clone(), None, "plan-1".into())
        .await?;
    assert!(
        store
            .record_job_pipeline_release(
                project.clone(),
                pipeline.clone(),
                "implement".into(),
                "plan-1".into(),
                worktree.checkout_id.clone(),
                task.id.clone()
            )
            .await
            .is_err()
    );
    let stale = store
        .record_job_pipeline_release(
            project.clone(),
            pipeline.clone(),
            "approve".into(),
            "plan-1".into(),
            worktree.checkout_id.clone(),
            task.id.clone(),
        )
        .await?;
    let missing = submit("plan-1", None, SessionMode::ApprovedWrite).await?;
    let old = submit("plan-1", Some(stale.id), SessionMode::ApprovedWrite).await?;
    store
        .set_job_pipeline_plan(
            project.clone(),
            pipeline.clone(),
            Some("plan-1".into()),
            "plan-2".into(),
        )
        .await?;
    for (job_id, expected) in [
        (missing.id, ReleaseBlocker::Missing),
        (old.id, ReleaseBlocker::StalePlan),
    ] {
        assert_eq!(
            store
                .inspect_job(project.clone(), job_id, env.clone())
                .await?
                .unwrap()
                .blockers,
            [AdmissionBlocker::PipelineRelease { reason: expected }]
        );
    }
    assert!(store.claim_next_job(env.clone()).await?.is_none());
    let release = store
        .record_job_pipeline_release(
            project.clone(),
            pipeline.clone(),
            "approve".into(),
            "plan-2".into(),
            worktree.checkout_id.clone(),
            task.id.clone(),
        )
        .await?;
    let parent = submit(
        "plan-2",
        Some(release.id.clone()),
        SessionMode::ApprovedWrite,
    )
    .await?;
    let attempt = store
        .claim_next_job(env.clone())
        .await?
        .context("The released writer was not admitted")?;
    assert_eq!(
        store
            .job(project.clone(), parent.id)
            .await?
            .unwrap()
            .latest_attempt
            .unwrap()
            .run_id,
        attempt.run.id
    );
    let bridge = RunBridge::start(&store, attempt.run.id.clone()).await?;
    let helper = start_helper(bridge.ticket_path().into(), worktree.root.clone()).await?;
    let (helper, spoofed) = invoke_tool(helper, "submit_job", json!({"requestId":"spoof", "request":{
        "mode":"approved_write", "instructions":"Try to forge release identity", "releaseId":"untrusted"
    }})).await?;
    assert!(spoofed["error"].is_object());
    let (helper, submitted) = invoke_tool(
        helper,
        "submit_job",
        json!({"requestId":"child", "request":{
            "mode":"approved_write", "instructions":"Implement the delegated scope"
        }}),
    )
    .await?;
    let child = submitted["result"]["structuredContent"]["jobId"]
        .as_str()
        .context("Child response omitted the job ID")?;
    let (_helper, status) = invoke_tool(helper, "get_job_status", json!({"jobId":child})).await?;
    assert_eq!(
        status["result"]["structuredContent"]["pipeline"],
        json!({
            "pipelineRunId": pipeline, "nodeInstanceId": "implement", "planRevision": "plan-2", "release": release,
        })
    );
    assert!(
        store.claim_next_job(env.clone()).await?.is_none(),
        "The parent still owns the writer"
    );
    store
        .finish_agent_attempt(attempt.run.id, RunStatus::Failed, None)
        .await?;
    let first_child = store
        .claim_next_job(env.clone())
        .await?
        .context("Released child did not run")?;
    let lineage = store
        .attempt_pipeline_origin(project.clone(), first_child.run.id.clone())
        .await?
        .unwrap();
    assert_eq!(lineage.release.as_ref(), Some(&release));
    let grandchild = store
        .submit_agent_job(
            first_child.run.id.clone(),
            "nested".into(),
            JobRequest {
                checkout_id: None,
                task_id: None,
                mode: SessionMode::ApprovedWrite,
                instructions: "Nested writer".into(),
                dependencies: vec![child.into()],
                requires_bridge: false,
            },
        )
        .await?;
    assert_eq!(grandchild.pipeline.as_ref(), Some(&lineage));
    store
        .finish_agent_attempt(first_child.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            project.clone(),
            child.into(),
            first_child.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    let retry = store
        .claim_next_job(env.clone())
        .await?
        .context("Eligible child retry did not run")?;
    assert_eq!(retry.previous_run_id.as_ref(), Some(&first_child.run.id));
    assert_eq!(
        Store::open(db)?
            .attempt_pipeline_origin(project.clone(), retry.run.id.clone())
            .await?,
        Some(lineage)
    );
    store
        .finish_agent_attempt(retry.run.id.clone(), RunStatus::Failed, None)
        .await?;
    store
        .queue_job_recovery(
            project.clone(),
            child.into(),
            retry.run.id.clone(),
            RecoveryAction::Retry,
        )
        .await?;
    store
        .revoke_job_pipeline_release(project.clone(), release.id)
        .await?;
    let recheck = store
        .inspect_job(project.clone(), child.into(), env.clone())
        .await?
        .unwrap();
    assert_eq!(
        recheck.blockers,
        [AdmissionBlocker::PipelineRelease {
            reason: ReleaseBlocker::Revoked
        }]
    );
    assert!(store.claim_next_job(env).await?.is_none());
    assert!(
        store
            .queue_job_recovery(project, child.into(), retry.run.id, RecoveryAction::Retry)
            .await
            .is_err()
    );
    Ok(())
}
