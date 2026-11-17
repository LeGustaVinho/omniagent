use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use super::*;
use crate::{
    jobs::JobScheduler,
    runner::DirectWindowsRunner,
    runs::RunStatus,
    workflows::{
        ArtifactKind, ArtifactValue, BuiltinNode, CommandConfiguration, CommandData,
        CommandOutputFormat, CommandRegistrationRequest, CommandTarget, DeliveryRoles, Endpoint,
        ManualWorkflowLaunch, NodeConfiguration, NodeInputs, NodeInstance, Port, PortType, Wire,
        WorkflowCommandSubmission, WorkflowGraph, WorkflowLaunchSource, builtin_delivery_graph,
        tests::{approve_delivery_fixture_in_run, approved_delivery_repository},
    },
};

#[tokio::test]
async fn repeat_uses_the_previous_output_stops_at_its_bound_and_replays_after_restart() -> Result<()>
{
    use crate::workflows::{ConditionPredicate, RepeatFeedback, RepeatUntilConfiguration};
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("repeat.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id;
    let note = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let body = member(
        "body",
        PackageArtifactBody::Subflow {
            graph: WorkflowGraph {
                name: "Load iteration context".into(),
                nodes: vec![
                    node(
                        "start",
                        BuiltinNode::Start,
                        NodeConfiguration::SubflowEntry {
                            outputs: vec![
                                Port {
                                    name: "start".into(),
                                    value: PortType::Control,
                                },
                                Port {
                                    name: "always".into(),
                                    value: PortType::Control,
                                },
                                Port {
                                    name: "document".into(),
                                    value: PortType::Data(ArtifactKind::Markdown),
                                },
                            ],
                        },
                    ),
                    node("load", BuiltinNode::LoadContext, NodeConfiguration::None),
                ],
                wires: vec![
                    Wire {
                        from: port("start", "start"),
                        to: port("load", "ready"),
                    },
                    Wire {
                        from: port("start", "document"),
                        to: port("load", "document"),
                    },
                ],
            },
            inputs: [
                ("go".into(), port("start", "start")),
                ("always".into(), port("start", "always")),
                ("document".into(), port("start", "document")),
            ]
            .into(),
            outputs: [
                ("done".into(), port("load", "loaded")),
                ("context".into(), port("load", "context")),
            ]
            .into(),
        },
        &[],
    );
    let mut requests = Vec::new();
    let mut legacy = None;
    for expected in ["brief.md", "missing.md"] {
        let configuration = RepeatUntilConfiguration {
            max_iterations: 2,
            ready_input: "go".into(),
            completion_output: "done".into(),
            evidence_output: "context".into(),
            until: ConditionPredicate::Equals {
                pointer: "/evidence/relativePath".into(),
                value: serde_json::Value::String(expected.into()),
            },
            feedback: Some(RepeatFeedback {
                input: "document".into(),
                output: "context".into(),
            }),
            exports: Vec::new(),
        };
        let root = member(
            "root",
            PackageArtifactBody::Pipeline {
                graph: WorkflowGraph {
                    name: expected.into(),
                    nodes: vec![
                        node("start", BuiltinNode::Start, NodeConfiguration::None),
                        node(
                            "repeat",
                            BuiltinNode::RepeatUntil,
                            NodeConfiguration::RepeatUntil(configuration),
                        ),
                        node("success", BuiltinNode::Parallel, NodeConfiguration::None),
                        node("failure", BuiltinNode::Parallel, NodeConfiguration::None),
                    ],
                    wires: vec![
                        Wire {
                            from: port("start", "start"),
                            to: port("repeat", "go"),
                        },
                        Wire {
                            from: port("start", "start"),
                            to: port("repeat", "always"),
                        },
                        Wire {
                            from: port("start", "document"),
                            to: port("repeat", "document"),
                        },
                        Wire {
                            from: port("repeat", "completed"),
                            to: port("success", "ready"),
                        },
                        Wire {
                            from: port("repeat", "failed"),
                            to: port("failure", "ready"),
                        },
                    ],
                },
            },
            &[("repeat", "body")],
        );
        let draft = store
            .create_workflow_package(
                project.clone(),
                None,
                WorkflowPackage {
                    name: "Bounded iteration".into(),
                    members: vec![body.clone(), root],
                },
            )
            .await?;
        let WorkflowPackagePublishOutcome::Published { publication } = store
            .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
            .await?
        else {
            anyhow::bail!("Expected repeated workflow publication")
        };
        let version = &publication
            .members
            .iter()
            .find(|item| item.key == "root")
            .unwrap()
            .artifact
            .version;
        let launch = ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: WorkflowRevision {
                id: version.id.clone(),
                revision: version.revision.clone(),
            },
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: note.revision.id.clone(),
            },
            request_id: expected.into(),
            work_id: None,
        };
        let run = store.launch_workflow_manual(launch.clone()).await?;
        if legacy.is_none() {
            let body = publication
                .members
                .iter()
                .find(|item| item.key == "body")
                .context("Missing repeat body revision")?
                .artifact
                .clone();
            legacy = Some((launch.clone(), run.id.clone(), body));
        }
        requests.push((launch, run.id));
    }
    drop(store);
    let store = Store::open(database)?;
    for (index, (launch, id)) in requests.into_iter().enumerate() {
        assert_eq!(store.launch_workflow_manual(launch).await?.id, id);
        let graph = store
            .inspect_workflow_execution(project.clone(), id)
            .await?
            .context("Missing repeated workflow")?;
        let first = graph
            .outputs
            .iter()
            .find(|entry| entry.artifact.node_instance_id == "repeat::iteration-1::load")
            .context("Iteration one did not produce context")?;
        let second = graph
            .outputs
            .iter()
            .find(|entry| entry.artifact.node_instance_id == "repeat::iteration-2::load");
        let selected = if index == 0 { "success" } else { "failure" };
        assert!(
            graph.control.iter().any(
                |receipt| receipt.node_id == selected && receipt.selected_ports == ["branches"]
            )
        );
        if index == 0 {
            assert!(second.is_none());
        } else {
            let second = second.context("Iteration two was not reached")?;
            assert_eq!(
                second.input_receipts[0].inputs["document"],
                first.artifact.id
            );
            assert!(
                graph
                    .control
                    .iter()
                    .any(|receipt| receipt.node_id == "repeat::limit"
                        && receipt.reason.as_deref()
                            == Some("Reached the configured iteration limit"))
            );
        }
    }
    let (mut launch, previous_run, mut body) = legacy.context("Missing repeat fixture")?;
    let PackageArtifactBody::Subflow { graph, .. } = &mut body.body else {
        anyhow::bail!("Expected the published repeat body");
    };
    // Represent a saved body accepted by the old compiler. The always-ready input
    // could start this effect in any copy regardless of the stopping result.
    graph.nodes.push(node(
        "unguarded",
        BuiltinNode::McpTool,
        NodeConfiguration::McpTool(crate::workflows::mcp_tools::McpToolConfiguration {
            server_id: uuid::Uuid::new_v4().to_string(),
            server_revision: "1".repeat(64),
            tool_name: "send".into(),
            contract_revision: "2".repeat(64),
            arguments: Default::default(),
            argument_mappings: Default::default(),
            destination_pointer: None,
        }),
    ));
    graph.wires.push(Wire {
        from: port("start", "always"),
        to: port("unguarded", "ready"),
    });
    body.definitions.insert(
        "unguarded".into(),
        builtin_registry()
            .into_iter()
            .find(|definition| definition.kind == BuiltinNode::McpTool)
            .context("Missing MCP definition")?,
    );
    store
        .execute(move |db| {
            let changed = db.execute(
                "UPDATE workflow_package_artifact_revisions SET payload = ?1
                 WHERE artifact_id = ?2 AND revision = ?3",
                params![seal(&body)?, body.version.id, body.version.revision],
            )?;
            assert_eq!(changed, 1);
            Ok(())
        })
        .await?;
    launch.request_id = "legacy-body-new-launch".into();
    let error = store.launch_workflow_manual(launch).await.unwrap_err();
    assert!(format!("{error:#}").contains("Review and publish revised Repeat body repeat"));
    assert!(
        store
            .inspect_workflow_execution(project, previous_run)
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn repeat_exports_only_the_approved_round_in_simulation_and_execution() -> Result<()> {
    use crate::workflows::WorkflowLaunchRequest;
    use serde_json::json;

    let dir = tempfile::tempdir()?;
    let database = dir.path().join("repeat-output.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, dir.path()).await?;
    let project = &repo.project_id;
    let wire = |from, output, to, input| Wire {
        from: port(from, output),
        to: port(to, input),
    };
    let body = member(
        "body",
        serde_json::from_value(json!({
            "kind": "subflow",
            "graph": {
                "name": "Evaluate revised findings",
                "nodes": [
                    {"id":"start","kind":"start","configuration":{"kind":"subflow_entry","outputs":[
                        {"name":"start","value":{"kind":"control"}},
                        {"name":"state","value":{"kind":"data","artifact":"json_object"}}
                    ]}},
                    {"id":"revise","kind":"set_fields","configuration":{"kind":"set_fields","mode":"update","operations":[
                        {"op":"set","path":"/accepted","value":{"source":"input","pointer":"/current"}},
                        {"op":"set","path":"/current","value":{"source":"literal","value":true}}
                    ]}},
                    {"id":"render","kind":"build_text","configuration":{"kind":"build_text","template":"Approved: {{accepted}}"}}
                ],
                "wires": [
                    wire("start", "start", "revise", "ready"),
                    wire("start", "state", "revise", "source"),
                    wire("revise", "completed", "render", "ready"),
                    wire("revise", "object", "render", "values")
                ]
            },
            "inputs": {"go": port("start", "start"), "state": port("start", "state")},
            "outputs": {
                "done": port("revise", "completed"),
                "decision": port("revise", "object"),
                "summary": port("render", "text")
            }
        }))?,
        &[],
    );
    for max_iterations in [1, 2] {
        let root = member(
            "root",
            serde_json::from_value(json!({
                "kind":"pipeline",
                "graph":{
                    "name": format!("Review with {max_iterations} iterations"),
                    "nodes": [
                        {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
                        {"id":"seed","kind":"set_fields","configuration":{"kind":"set_fields","mode":"build","operations":[
                            {"op":"set","path":"/current","value":{"source":"literal","value":false}}
                        ]}},
                        {"id":"repeat","kind":"repeat_until","configuration":{
                            "kind":"repeat_until","maxIterations":max_iterations,"readyInput":"go",
                            "completionOutput":"done","evidenceOutput":"decision",
                            "until":{"op":"equals","pointer":"/evidence/accepted","value":true},
                            "feedback":{"input":"state","output":"decision"},
                            "exports":["decision","summary"]
                        }},
                        {"id":"consume","kind":"set_fields","configuration":{"kind":"set_fields","mode":"update","operations":[
                            {"op":"set","path":"/consumed","value":{"source":"literal","value":true}}
                        ]}},
                        {"id":"agent","kind":"agent","configuration":{"kind":"agent_role","role":"follow-up"}},
                        {"id":"escalate","kind":"parallel","configuration":{"kind":"none"}}
                    ],
                    "wires":[
                        wire("start", "start", "seed", "ready"),
                        wire("seed", "completed", "repeat", "go"),
                        wire("seed", "object", "repeat", "state"),
                        wire("repeat", "completed", "consume", "ready"),
                        wire("repeat", "decision", "consume", "source"),
                        wire("repeat", "completed", "agent", "ready"),
                        wire("repeat", "summary", "agent", "prompt"),
                        wire("repeat", "failed", "escalate", "ready")
                    ]
                }
            }))?,
            &[("repeat", "body")],
        );
        let draft = store
            .create_workflow_package(
                project.clone(),
                None,
                WorkflowPackage {
                    name: "Review iteration outputs".into(),
                    members: vec![body.clone(), root],
                },
            )
            .await?;
        let trace = serde_json::to_value(
            store
                .simulate_workflow_package_revision(
                    project.clone(),
                    draft.id.clone(),
                    draft.revision.clone(),
                    "root".into(),
                    Default::default(),
                )
                .await?,
        )?;
        let exports = &trace["subflows"]
            .as_array()
            .context("Missing simulated subflows")?
            .iter()
            .find(|entry| entry["path"] == "repeat")
            .context("Missing simulated repeat")?["outputs"];
        assert_eq!(
            exports["decision"]["state"],
            if max_iterations == 1 {
                "skipped"
            } else {
                "selected"
            }
        );
        assert_eq!(
            exports["failed"]["state"],
            if max_iterations == 1 {
                "selected"
            } else {
                "skipped"
            }
        );

        let WorkflowPackagePublishOutcome::Published { publication } = store
            .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
            .await?
        else {
            anyhow::bail!("The repeat package could not be published");
        };
        let version = &publication
            .members
            .iter()
            .find(|item| item.key == "root")
            .unwrap()
            .artifact
            .version;
        let request: WorkflowLaunchRequest = serde_json::from_value(json!({
            "projectId": project, "checkoutId": repo.checkout_id,
            "workflow": { "id": version.id, "revision": version.revision },
            "requestId": format!("repeat-outputs-{max_iterations}"), "inputs": {}
        }))?;
        let run = store.launch_workflow_request(request).await?;
        let reopened = Store::open(database.clone())?;
        let snapshot = reopened
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("Missing launched repeat")?;
        let stop = snapshot
            .control
            .iter()
            .find(|entry| entry.node_id == "repeat::stop")
            .unwrap();
        let first_prompt = snapshot
            .outputs
            .iter()
            .find(|output| output.artifact.node_instance_id == "repeat::iteration-1::render")
            .unwrap();
        if max_iterations == 1 {
            assert!(stop.forwarded_outputs.is_empty());
            assert!(
                snapshot
                    .control
                    .iter()
                    .any(|receipt| receipt.node_id == "repeat::limit"
                        && receipt.reason.as_deref()
                            == Some("Reached the configured iteration limit"))
            );
            assert!(
                snapshot
                    .control
                    .iter()
                    .any(|receipt| receipt.node_id == "escalate"
                        && receipt.selected_ports == ["branches"])
            );
            assert!(
                snapshot
                    .outputs
                    .iter()
                    .all(|output| output.artifact.node_instance_id != "consume")
            );
            continue;
        }
        let approved_prompt = snapshot
            .outputs
            .iter()
            .find(|output| output.artifact.node_instance_id == "repeat::iteration-2::render")
            .unwrap();
        let approved_decision = snapshot
            .outputs
            .iter()
            .find(|output| output.artifact.node_instance_id == "repeat::iteration-2::revise")
            .unwrap();
        assert_eq!(stop.selected_ports, ["completed"]);
        assert_eq!(
            stop.forwarded_outputs["summary"].artifact_id,
            approved_prompt.artifact.id
        );
        assert_eq!(
            stop.forwarded_outputs["decision"].artifact_id,
            approved_decision.artifact.id
        );
        assert_eq!(
            approved_prompt.artifact.value,
            ArtifactValue::RenderedText("Approved: true".into())
        );
        let consumer = snapshot
            .outputs
            .iter()
            .find(|output| output.artifact.node_instance_id == "consume")
            .unwrap();
        assert_eq!(
            consumer.input_receipts[0].inputs["source"],
            approved_decision.artifact.id
        );
        let agent_inputs = |id: &str| NodeInputs::from([("prompt".into(), id.into())]);
        assert!(
            reopened
                .workflow_operator_agent_target(
                    project,
                    &run.id,
                    "agent",
                    None,
                    &agent_inputs(&first_prompt.artifact.id)
                )
                .await
                .is_err()
        );
        reopened
            .workflow_operator_agent_target(
                project,
                &run.id,
                "agent",
                None,
                &agent_inputs(&approved_prompt.artifact.id),
            )
            .await?;
    }
    Ok(())
}

fn port(node: &str, name: &str) -> Endpoint {
    Endpoint {
        node: node.into(),
        port: name.into(),
    }
}

fn node(id: &str, kind: BuiltinNode, configuration: NodeConfiguration) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind,
        configuration,
    }
}

fn member(key: &str, body: PackageArtifactBody, refs: &[(&str, &str)]) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references: refs
            .iter()
            .map(|(slot, child)| {
                (
                    slot.to_string(),
                    PackageReference::Draft {
                        key: child.to_string(),
                    },
                )
            })
            .collect(),
    }
}

fn fragment(name: &str, stage: NodeInstance) -> PackageArtifactBody {
    let inputs = [
        ("ready", PortType::Control),
        ("release", PortType::Data(ArtifactKind::Release)),
        ("worktree", PortType::Data(ArtifactKind::Worktree)),
    ];
    let start = node(
        "start",
        BuiltinNode::Start,
        NodeConfiguration::SubflowEntry {
            outputs: inputs
                .iter()
                .map(|(name, value)| Port {
                    name: name.to_string(),
                    value: *value,
                })
                .collect(),
        },
    );
    let stage_id = stage.id.clone();
    PackageArtifactBody::Subflow {
        graph: WorkflowGraph {
            name: name.into(),
            nodes: vec![start, stage],
            wires: inputs
                .iter()
                .map(|(name, _)| Wire {
                    from: port("start", name),
                    to: port(&stage_id, name),
                })
                .collect(),
        },
        inputs: inputs
            .iter()
            .map(|(name, _)| (name.to_string(), port("start", name)))
            .collect(),
        outputs: [
            ("completed".into(), port(&stage_id, "completed")),
            ("result".into(), port(&stage_id, "result")),
        ]
        .into(),
    }
}

#[tokio::test]
async fn nested_reusable_command_uses_the_parent_release_and_preserves_exact_output_pins()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id.clone();
    let script = directory.path().join("nested.ps1");
    std::fs::write(
        &script,
        r#"
$null = [Console]::In.ReadToEnd()
[IO.File]::WriteAllText((Join-Path (Get-Location) 'nested-proof.txt'), 'inside managed scope')
[Console]::Write('{"verified":true}')
"#,
    )?;
    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let registration = store
        .register_workflow_command(
            project.clone(),
            CommandRegistrationRequest {
                name: "Nested stage fixture".into(),
                target: CommandTarget::Script {
                    executable,
                    path: script,
                    interpreter_arguments: [
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                    ]
                    .map(String::from)
                    .to_vec(),
                },
            },
        )
        .await?;
    let inner = member(
        "check",
        fragment(
            "Check source",
            node(
                "command",
                BuiltinNode::Command,
                NodeConfiguration::Command(CommandConfiguration {
                    registration_id: registration.id,
                    registration_revision: registration.revision,
                    arguments: vec![],
                    input: Default::default(),
                    output: CommandOutputFormat::Json,
                }),
            ),
        ),
        &[],
    );
    let outer = member(
        "review",
        fragment(
            "Review",
            node("check", BuiltinNode::Subflow, NodeConfiguration::None),
        ),
        &[("check", "check")],
    );
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.nodes.push(node(
        "review",
        BuiltinNode::Subflow,
        NodeConfiguration::None,
    ));
    graph.wires.extend(
        [
            ("prepared", "ready"),
            ("release", "release"),
            ("worktree", "worktree"),
        ]
        .map(|(source, target)| Wire {
            from: port("prepare", source),
            to: port("review", target),
        }),
    );
    let root = member(
        "delivery",
        PackageArtifactBody::Pipeline { graph },
        &[("review", "review")],
    );
    let mut invalid = root.clone();
    let PackageArtifactBody::Pipeline { graph } = &mut invalid.body else {
        unreachable!()
    };
    graph
        .wires
        .iter_mut()
        .find(|wire| wire.to == port("review", "release"))
        .unwrap()
        .from = port("prepare", "worktree");
    let bad = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Invalid port".into(),
                members: vec![inner.clone(), outer.clone(), invalid],
            },
        )
        .await?;
    let error = store
        .publish_reviewed_workflow_package(project.clone(), bad.id, bad.revision)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("incompatible port type"),
        "{error:#}"
    );

    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Verified nested workflow".into(),
                members: vec![inner, outer, root],
            },
        )
        .await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Expected the reviewed package")
    };
    let versions = publication
        .members
        .iter()
        .map(|member| (member.key.as_str(), &member.artifact.version))
        .collect::<BTreeMap<_, _>>();
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
                id: versions["delivery"].id.clone(),
                revision: versions["delivery"].revision.clone(),
            },
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: note.revision.id,
            },
            request_id: "nested-launch".into(),
            work_id: None,
        })
        .await?;
    let nested = "review::check::command";
    let request = |inputs| WorkflowCommandSubmission {
        project_id: project.clone(),
        run_id: run.id.clone(),
        node_id: nested.into(),
        inputs,
    };
    assert!(
        store
            .submit_workflow_command(request(NodeInputs::new()))
            .await
            .is_err()
    );
    let fixture =
        approve_delivery_fixture_in_run(&store, directory.path(), repo, None, run.clone()).await?;
    let job = store
        .submit_workflow_command(request(NodeInputs::from([
            ("release".into(), fixture.release.id.clone()),
            ("worktree".into(), fixture.worktree.id.clone()),
        ])))
        .await?;
    let scheduler = JobScheduler::start(store.clone(), Arc::new(DirectWindowsRunner), None);
    let attempt = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let job = store
                .job(project.clone(), job.id.clone())
                .await?
                .context("Missing nested job")?;
            if let Some(attempt) = job
                .latest_attempt
                .filter(|attempt| attempt.status.is_terminal())
            {
                return Ok::<_, anyhow::Error>(attempt);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    assert_eq!(attempt.status, RunStatus::Succeeded);
    let result = store
        .workflow_command_result(project.clone(), attempt.run_id.clone())
        .await?
        .context("Missing nested result")?;
    assert!(
        matches!(result.value, ArtifactValue::CommandResult(answer) if answer.output == CommandData::Json(serde_json::json!({"verified":true})))
    );
    drop(scheduler);
    drop(store);
    let reopened = Store::open(database)?;
    let snapshot = reopened
        .inspect_workflow_execution(project, run.id)
        .await?
        .context("Missing pinned execution")?;
    assert_eq!(snapshot.definition.subflows.len(), 2);
    assert_eq!(&snapshot.definition.subflows[0].version, versions["review"]);
    assert_eq!(&snapshot.definition.subflows[1].version, versions["check"]);
    assert_eq!(
        snapshot.definition.subflows[0].outputs["result"],
        port(nested, "result")
    );
    assert!(
        snapshot
            .jobs
            .iter()
            .any(|job| job.node_id == nested && job.attempts[0].run_id == attempt.run_id)
    );
    Ok(())
}
