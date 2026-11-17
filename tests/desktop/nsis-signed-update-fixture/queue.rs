use crate::{
    jobs::QueueState,
    repository::ProjectCheckout,
    workflows::{
        BuiltinNode, CommandConfiguration, CommandOutputFormat, CommandRegistrationRequest,
        CommandTarget, Endpoint, ManualWorkflowLaunch, NodeConfiguration, NodeInputs, NodeInstance,
        Wire, WorkflowCommandSubmission, WorkflowLaunchSource, builtin_delivery_graph,
    },
};

use super::*;

pub(super) async fn seed(store: &Store, case: &Path, repo: &ProjectCheckout) -> Result<()> {
    let project = repo.project_id.clone();
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("Windows root is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let registration = store
        .register_workflow_command(
            project.clone(),
            CommandRegistrationRequest {
                name: "Observe the updater drain and startup".into(),
                target: CommandTarget::Script {
                    executable: powershell,
                    path: case.join("queue.ps1"),
                    interpreter_arguments: [
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                },
            },
        )
        .await?;
    let tag = case
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("omniagent-signed-update-"))
        .context("Invalid disposable case directory")?;
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("Missing LOCALAPPDATA")?);
    let executable = local
        .join(format!("OmniAgent Signed Update Test {tag}"))
        .join("omniagent.exe");
    let journal = local
        .join(env!("OMNIAGENT_SIGNED_FIXTURE_ID"))
        .join("fixture-queue-journal.json");
    let mut commands = Vec::new();
    for mode in ["hold", "probe"] {
        let mut graph = builtin_delivery_graph(DeliveryRoles::default());
        graph.nodes.push(NodeInstance {
            id: mode.into(),
            kind: BuiltinNode::Command,
            configuration: NodeConfiguration::Command(CommandConfiguration {
                registration_id: registration.id.clone(),
                registration_revision: registration.revision.clone(),
                arguments: [
                    mode.into(),
                    case.to_str().context("Invalid fixture directory")?.into(),
                    executable
                        .to_str()
                        .context("Invalid fixture executable")?
                        .into(),
                    journal.to_str().context("Invalid fixture journal")?.into(),
                ]
                .to_vec(),
                input: serde_json::from_value(json!({}))?,
                output: CommandOutputFormat::Json,
            }),
        });
        for (from, to) in [
            ("prepared", "ready"),
            ("release", "release"),
            ("worktree", "worktree"),
        ] {
            graph.wires.push(Wire {
                from: Endpoint {
                    node: "prepare".into(),
                    port: from.into(),
                },
                to: Endpoint {
                    node: mode.into(),
                    port: to.into(),
                },
            });
        }
        let version = store
            .save_workflow_revision(project.clone(), format!("fixture-{mode}"), graph)
            .await?;
        let run = store
            .launch_workflow_manual(ManualWorkflowLaunch {
                project_id: project.clone(),
                workflow: version,
                source: WorkflowLaunchSource {
                    checkout_id: repo.checkout_id.clone(),
                    revision_id: source.revision.id.clone(),
                },
                request_id: uuid::Uuid::new_v4().to_string(),
                work_id: None,
            })
            .await?;
        let linked = link_reviewed_plan_fixture(store, repo, None, &run, PLAN, mode).await?;
        let fixture = approve_reviewed_plan_fixture(store, case, repo.clone(), run, linked).await?;
        commands.push((mode, fixture));
    }
    store.set_job_queue_capacity(1).await?;
    store.set_job_queue_paused(true).await?;
    let mut ids = Vec::new();
    for (mode, fixture) in commands {
        let job = store
            .submit_workflow_command(WorkflowCommandSubmission {
                project_id: project.clone(),
                run_id: fixture.run.id,
                node_id: mode.into(),
                inputs: NodeInputs::from([
                    ("release".into(), fixture.release.id),
                    ("worktree".into(), fixture.worktree.id),
                ]),
            })
            .await?;
        if mode == "probe" {
            store
                .set_pending_job_state(project.clone(), job.id.clone(), QueueState::Paused)
                .await?;
        }
        ids.push(job.id);
    }
    fs::write(
        case.join("queue-plan.json"),
        serde_json::to_vec_pretty(&json!({"projectId":project,"holdId":ids[0],"probeId":ids[1]}))?,
    )?;
    Ok(())
}
