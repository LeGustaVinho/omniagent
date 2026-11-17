use super::*;

use crate::{
    jobs::{AdmissionEnvironment, JobRequest, JobRole, PipelineDispatch},
    model_catalog::ModelSelection,
    providers::{
        composer_customizations::{current_skill, probe_codex_skills},
        plugins::{PluginInventoryStatus, probe_plugins},
    },
    workflows::{
        BuiltinNode, Endpoint, ManualWorkflowLaunch, NodeConfiguration, NodeInputs, NodeInstance,
        Wire, WorkflowAgentSubmission, WorkflowGraph, WorkflowLaunchSource, WorkflowRevision,
        role_templates::{ConversationTemplate, SaveConversationTemplate, TemplateReviewPolicy},
        stage_requirements::{
            RequiredStageSource, StagePluginIdentity, StageRequirementConfiguration,
            StageRequirementDecision, StageRequirements, StageRoleRevision, StageSourceEvidence,
            StageSourcePath,
        },
    },
};
use std::{env, fs};

const PLUGIN: &str = "fixture-plugin@omniagent-fixture-market";
const FIXTURE_ENV: &str = "OMNIAGENT_DISPOSABLE_CODEX_ROLES";

#[test]
#[ignore = "requires a Codex CLI with native local marketplace support"]
fn stage_roles_preserve_pre_change_inventory_and_recheck_a_removed_native_plugin() -> Result<()> {
    if let Some(root) = env::var_os(FIXTURE_ENV) {
        let root = PathBuf::from(root);
        ensure!(
            root.join("fixture-marker").is_file()
                && env::var_os("CODEX_HOME").map(PathBuf::from) == Some(root.join("profile")),
            "The Codex acceptance requires its own disposable configuration"
        );
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(exercise(&root));
    }
    let temp = tempfile::Builder::new()
        .prefix("omniagent-codex-stage-roles-")
        .tempdir()?;
    let root = dunce::canonicalize(temp.path())?;
    fs::create_dir(root.join("profile"))?;
    fs::write(
        root.join("profile/config.toml"),
        "chatgpt_base_url = 'http://127.0.0.1:9/backend-api/'\nweb_search = 'disabled'\n",
    )?;
    fs::write(root.join("fixture-marker"), b"isolated")?;
    let name = format!(
        "{}::stage_roles_preserve_pre_change_inventory_and_recheck_a_removed_native_plugin",
        module_path!()
    );
    let name = name.strip_prefix("omniagent::").unwrap_or(&name);
    let result = Command::new(env::current_exe()?)
        .args(["--ignored", "--exact", name, "--nocapture"])
        .env(FIXTURE_ENV, &root)
        .env("CODEX_HOME", root.join("profile"))
        .env("OPENAI_BASE_URL", "http://127.0.0.1:9/v1")
        .env_remove("OPENAI_API_KEY")
        .output()?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    ensure!(
        result.status.success() && stdout.contains("1 passed"),
        "Disposable Codex role fixture failed: {stdout}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

fn codex(executable: &Path, cwd: &Path, args: &[&str]) -> Result<String> {
    let result = Command::new(executable)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()?;
    ensure!(
        result.status.success(),
        "The disposable Codex CLI command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?)
}

fn prepare_market(root: &Path) -> Result<PathBuf> {
    let market = root.join("marketplace");
    let index = market.join(".agents/plugins");
    let plugin = market.join("plugins/fixture-plugin");
    let export = plugin.join("skills/fixture-export");
    fs::create_dir_all(&index)?;
    fs::create_dir_all(plugin.join(".codex-plugin"))?;
    fs::create_dir_all(&export)?;
    fs::write(index.join("marketplace.json"), json!({
        "name":"omniagent-fixture-market",
        "plugins":[{"name":"fixture-plugin","source":{"source":"local","path":"./plugins/fixture-plugin"}}]
    }).to_string())?;
    fs::write(plugin.join(".codex-plugin/plugin.json"), json!({
        "name":"fixture-plugin","version":"1.0.0","description":"Disposable fixture","skills":"./skills/"
    }).to_string())?;
    fs::write(
        export.join("SKILL.md"),
        "---\nname: fixture-export\ndescription: Disposable export\n---\nInspect the fixture.\n",
    )?;
    Ok(market)
}

async fn save_role(
    store: &Store,
    project: &str,
    name: &str,
    model: ModelSelection,
    source: RequiredStageSource,
) -> Result<StageRoleRevision> {
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(
            project.into(),
            None,
            ConversationTemplate {
                name: name.into(),
                role: JobRole::Pipeline,
                model,
                effort: None,
                instructions: "Read the saved brief.".into(),
                tool_scope: SessionMode::ReadOnly,
                completion_contract: "Report the matching source.".into(),
                review_policy: TemplateReviewPolicy::Independent,
                requirements: StageRequirements {
                    capabilities: vec![],
                    sources: vec![source],
                },
            },
        )
        .await?
    else {
        anyhow::bail!("The role could not be saved");
    };
    Ok(StageRoleRevision {
        template_id: detail.revision.template_id,
        revision: detail.revision.version.revision,
    })
}

fn agent(id: &str, role: StageRoleRevision, model: ModelSelection) -> NodeInstance {
    NodeInstance {
        id: id.into(),
        kind: BuiltinNode::Plan,
        configuration: NodeConfiguration::AgentRole {
            role: id.into(),
            model: Some(model),
            requirements: StageRequirementConfiguration {
                role_template: Some(role),
                ..Default::default()
            },
        },
    }
}

fn wire(from: &str, output: &str, to: &str, input: &str) -> Wire {
    Wire {
        from: Endpoint {
            node: from.into(),
            port: output.into(),
        },
        to: Endpoint {
            node: to.into(),
            port: input.into(),
        },
    }
}

// Start a native thread and forward its configuration and skill inventory. The
// fixture settles the turn locally without forwarding a prompt to the CLI.
struct ScopedRoleWire {
    log: WireLog,
    thread: Arc<Mutex<Option<Value>>>,
}

impl Runner for ScopedRoleWire {
    fn identity(&self) -> &str {
        DirectWindowsRunner.identity()
    }

    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        DirectWindowsRunner.execution_platform()
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        DirectWindowsRunner.executable_revision(path)
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.arguments == ["plugin", "list", "--json"].map(std::ffi::OsStr::new) {
            return DirectWindowsRunner.launch(request);
        }
        if !request
            .arguments
            .ends_with(&["app-server", "--listen", "stdio://"].map(std::ffi::OsStr::new))
        {
            return Err(io::Error::other("Unexpected native fixture command"));
        }
        let native = DirectWindowsRunner.launch(request)?;
        let (write, output) = mpsc::channel();
        let forward = write.clone();
        let thread = self.thread.clone();
        std::thread::spawn(move || {
            for frame in native.output {
                if let Ok(frame) = &frame
                    && frame.stream == Stream::Stdout
                    && let Ok(value) = serde_json::from_str::<Value>(&frame.data)
                    && let Some(result) = value.get("result")
                    && result["thread"]["id"].is_string()
                    && result["sandbox"].is_object()
                {
                    *thread.lock().unwrap() = Some(result.clone());
                }
                if forward.send(frame).is_err() {
                    break;
                }
            }
        });
        Ok(CapturedProcess {
            input: ProcessInput::new(NativeRoleInput {
                native: native.input,
                output: write,
                log: self.log.clone(),
                thread: self.thread.clone(),
            }),
            output,
            control: native.control,
        })
    }
}

struct NativeRoleInput {
    native: ProcessInput,
    output: mpsc::Sender<io::Result<OutputFrame>>,
    log: WireLog,
    thread: Arc<Mutex<Option<Value>>>,
}

impl io::Write for NativeRoleInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let frame: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        io::Write::write_all(&mut self.log, bytes)?;
        match frame["method"].as_str() {
            Some("initialize" | "initialized" | "thread/start" | "config/read" | "skills/list") => {
                self.native.send(bytes)?;
            }
            Some("model/list") => self.reply([json!({"id":frame["id"],"result":{"data":[]}})])?,
            Some("turn/start") => {
                let thread = self.thread.lock().unwrap();
                let session = thread
                    .as_ref()
                    .and_then(|opened| opened["thread"]["id"].as_str())
                    .ok_or_else(|| io::Error::other("No native thread for the offline turn"))?;
                let turn = format!("{session}-offline-turn");
                self.reply([
                    json!({"id":frame["id"],"result":{"turn":{"id":turn}}}),
                    json!({"method":"item/completed","params":{"threadId":session,"turnId":turn,
                        "item":{"type":"agentMessage","id":"result","text":"Fixture stage completed."}}}),
                    json!({"method":"turn/completed","params":{"threadId":session,"turn":{"id":turn,"status":"completed"}}}),
                ])?;
            }
            _ => {
                return Err(io::Error::other(
                    "Unexpected request in offline role fixture",
                ));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl NativeRoleInput {
    fn reply(&self, frames: impl IntoIterator<Item = Value>) -> io::Result<()> {
        for frame in frames {
            self.output
                .send(Ok(OutputFrame {
                    stream: Stream::Stdout,
                    data: frame.to_string(),
                }))
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}

struct Attempt {
    run: String,
    conversation: String,
    status: RunStatus,
    wire: Vec<Value>,
    native_thread: Value,
}

struct WorkflowFixture<'a> {
    store: &'a Store,
    project: &'a str,
    checkout: &'a str,
    document: String,
    workflow: WorkflowRevision,
}

impl WorkflowFixture<'_> {
    async fn run(&self, request: &str, node: &str, scope: SessionScope) -> Result<Attempt> {
        let run = self
            .store
            .launch_workflow_manual(ManualWorkflowLaunch {
                project_id: self.project.into(),
                workflow: self.workflow.clone(),
                source: WorkflowLaunchSource {
                    checkout_id: self.checkout.into(),
                    revision_id: self.document.clone(),
                },
                request_id: request.into(),
                work_id: None,
            })
            .await?;
        let context = self
            .store
            .current_workflow_host_outputs(self.project.into(), run.id.clone())
            .await?["context"]
            .clone();
        let runner = scope.runner.clone();
        self.store
            .submit_workflow_agent(WorkflowAgentSubmission {
                project_id: self.project.into(),
                dispatch: PipelineDispatch {
                    pipeline_run_id: run.id,
                    node_instance_id: node.into(),
                    plan_revision: None,
                    release_id: None,
                },
                scope,
                input: Default::default(),
                request: JobRequest {
                    checkout_id: Some(self.checkout.into()),
                    task_id: None,
                    mode: SessionMode::Plan,
                    instructions: "Read the pinned brief".into(),
                    dependencies: vec![],
                    requires_bridge: false,
                },
                artifacts: NodeInputs::from([("context".into(), context)]),
                unit_id: None,
            })
            .await?;
        let attempt = self
            .store
            .claim_next_job(AdmissionEnvironment {
                runner,
                bridge_available: false,
            })
            .await?
            .context("No attempt reached scheduler admission")?;
        let run = attempt.run.id.clone();
        let conversation = attempt.conversation_id.clone();
        let log = WireLog {
            store: self.store.clone(),
            runtime: tokio::runtime::Handle::current(),
            conversation: conversation.clone(),
            frames: Arc::new(Mutex::new(Vec::new())),
        };
        let thread = Arc::new(Mutex::new(None));
        let provider = ScopedRoleWire {
            log: log.clone(),
            thread: thread.clone(),
        };
        let status = start_managed_turn(self.store.clone(), Arc::new(provider), attempt, None)
            .wait()
            .await?;
        let wire = log
            .frames
            .lock()
            .unwrap()
            .iter()
            .map(|bytes| serde_json::from_slice(bytes))
            .collect::<serde_json::Result<Vec<_>>>()?;
        let native_thread = thread
            .lock()
            .unwrap()
            .take()
            .context("The native CLI did not return thread scope")?;
        Ok(Attempt {
            run,
            conversation,
            status,
            wire,
            native_thread,
        })
    }
}

async fn exercise(root: &Path) -> Result<()> {
    let executable = dunce::canonicalize(Provider::Codex.adapter().discover(None)?)?;
    let version = codex(&executable, root, &["--version"])?;
    let version = version
        .trim()
        .strip_prefix("codex-cli ")
        .context("Unexpected Codex version")?
        .to_owned();
    let market = prepare_market(root)?;
    codex(
        &executable,
        root,
        &[
            "plugin",
            "marketplace",
            "add",
            market.to_str().context("Invalid marketplace path")?,
            "--json",
        ],
    )?;
    let installed: Value = serde_json::from_str(&codex(
        &executable,
        root,
        &["plugin", "add", PLUGIN, "--json"],
    )?)?;
    assert_eq!(installed["pluginId"], PLUGIN);
    let configured = crate::providers::profiles::ConfiguredExecutable {
        provider: Provider::Codex,
        executable: Some(executable.to_string_lossy().into_owned()),
    };
    let reported = probe_plugins(configured.clone())?;
    assert_eq!(reported.status, PluginInventoryStatus::Reported);
    let plugin = reported
        .plugins
        .iter()
        .find(|plugin| plugin.id == PLUGIN && plugin.enabled == Some(true))
        .context("The native installed plugin is missing")?;
    let identity = StagePluginIdentity::from(plugin);

    let (directory, store, _, mut scope) = setup().await?;
    let database = directory.path().join("store.sqlite");
    let repository = crate::repository::register(&store, scope.checkout.clone()).await?;
    let project = repository.project_id.clone();
    let checkout = repository.checkout_id.clone();
    let local_path = scope.checkout.join(".agents/skills/local-stage/SKILL.md");
    fs::create_dir_all(local_path.parent().unwrap())?;
    fs::write(
        &local_path,
        "---\nname: local-stage\ndescription: Local stage fixture\n---\nRead the brief.\n",
    )?;
    scope.executable = executable.clone();
    scope.cli_version = version;
    scope.runner = DirectWindowsRunner.identity().into();
    scope.profile_id = "desktop-direct:codex".into();
    scope.mode = SessionMode::Plan;
    scope.effort = None;
    let layout =
        crate::repository::verify_checkout(&store, checkout.clone(), scope.checkout.clone())
            .await?;
    let skills = probe_codex_skills(&scope, &layout)?
        .context("The native CLI did not report worktree skills")?;
    let local = skills
        .entries
        .iter()
        .find(|skill| skill.path == local_path && skill.enabled == Some(true))
        .context("The local worktree skill was not discovered")?;
    let exported = skills
        .entries
        .iter()
        .find(|skill| skill.plugin_id.as_deref() == Some(PLUGIN) && skill.enabled == Some(true))
        .context("The native CLI did not report the enabled plugin export")?
        .id
        .clone();
    let revision = current_skill(local)?
        .context("Missing local skill bytes")?
        .0
        .revision;
    let selected_model = |id: &str| ModelSelection {
        provider: Provider::Codex,
        profile_id: scope.profile_id.clone(),
        model_id: id.into(),
    };
    let local_model = selected_model("gpt-5.4-mini");
    let plugin_model = selected_model("gpt-5.4");
    let local_role = save_role(
        &store,
        &project,
        "Local skill planner",
        local_model.clone(),
        RequiredStageSource::Skill {
            provider: Provider::Codex,
            path: StageSourcePath::Checkout {
                path: local_path.strip_prefix(&scope.checkout)?.into(),
            },
            namespace: None,
            revision: revision.clone(),
            invoke: true,
        },
    )
    .await?;
    let plugin_role = save_role(
        &store,
        &project,
        "Plugin planner",
        plugin_model.clone(),
        RequiredStageSource::Plugin {
            provider: Provider::Codex,
            identity: identity.clone(),
        },
    )
    .await?;
    let graph = WorkflowGraph {
        name: "Scoped stage roles".into(),
        nodes: vec![
            NodeInstance {
                id: "start".into(),
                kind: BuiltinNode::Start,
                configuration: NodeConfiguration::None,
            },
            NodeInstance {
                id: "context".into(),
                kind: BuiltinNode::LoadContext,
                configuration: NodeConfiguration::None,
            },
            agent("local-role", local_role.clone(), local_model.clone()),
            agent("plugin-role", plugin_role.clone(), plugin_model.clone()),
        ],
        wires: vec![
            wire("start", "start", "context", "ready"),
            wire("start", "document", "context", "document"),
            wire("context", "loaded", "local-role", "ready"),
            wire("context", "context", "local-role", "context"),
            wire("context", "loaded", "plugin-role", "ready"),
            wire("context", "context", "plugin-role", "context"),
        ],
    };
    let workflow = store
        .save_workflow_revision(project.clone(), "stage-role-criteria".into(), graph)
        .await?;
    let brief = scope.checkout.join("brief.md");
    fs::write(&brief, "Check the bounds of this fixture.")?;
    let document = store
        .capture_repository_markdown(checkout.clone(), brief)
        .await?
        .revision
        .id;
    let fixture = WorkflowFixture {
        store: &store,
        project: &project,
        checkout: &checkout,
        document,
        workflow,
    };
    let for_model = |model: &ModelSelection| SessionScope {
        model: Some(model.model_id.clone()),
        ..scope.clone()
    };
    let local = fixture
        .run("local-role-launch", "local-role", for_model(&local_model))
        .await?;
    assert_eq!(local.status, RunStatus::Succeeded);
    assert_wire_scope(&local, &scope, &local_model)?;
    let sent = local
        .wire
        .iter()
        .find(|event| event["method"] == "turn/start")
        .context("Local role did not dispatch")?;
    assert!(
        sent["params"]["input"]
            .as_array()
            .context("Missing inputs")?
            .iter()
            .any(|input| input["type"] == "skill"
                && input["path"].as_str().map(PathBuf::from).as_ref() == Some(&local_path))
    );
    let local_evidence = store
        .workflow_stage_assessment(project.clone(), local.run)
        .await?
        .context("Missing local role evidence")?;
    assert_eq!(local_evidence.role_template, Some(local_role));
    assert_scope(&local_evidence.scope, &scope, &checkout, &local_model);
    assert!(
        matches!(&local_evidence.decisions[..], [StageRequirementDecision::Source {
        evidence: Some(StageSourceEvidence::Skill { skill }), blocker: None, .. }] if skill.revision == revision)
    );

    let before = fixture
        .run(
            "plugin-enabled-launch",
            "plugin-role",
            for_model(&plugin_model),
        )
        .await?;
    assert_eq!(before.status, RunStatus::Succeeded);
    assert_wire_scope(&before, &scope, &plugin_model)?;
    let before_evidence = store
        .workflow_stage_assessment(project.clone(), before.run.clone())
        .await?
        .context("Missing plugin role evidence")?;
    assert_eq!(before_evidence.role_template, Some(plugin_role.clone()));
    assert_scope(&before_evidence.scope, &scope, &checkout, &plugin_model);
    let before_snapshot = store
        .agent_dispatch(before.conversation.clone(), before.run.clone())
        .await?
        .context("Missing plugin dispatch")?;
    assert_eq!(before_snapshot.delivery, Delivery::Submitted);
    let inventory = before_snapshot
        .snapshot
        .plugin_inventory
        .context("Missing previous run inventory")?;
    assert!(
        matches!(&before_evidence.decisions[..], [StageRequirementDecision::Source {
        evidence: Some(StageSourceEvidence::Plugin { inventory_id, plugin, skill_ids }), blocker: None, .. }]
        if inventory_id == &inventory.id && StagePluginIdentity::from(plugin) == identity && skill_ids.contains(&exported))
    );

    let removed: Value = serde_json::from_str(&codex(
        &executable,
        root,
        &["plugin", "remove", PLUGIN, "--json"],
    )?)?;
    assert_eq!(removed["pluginId"], PLUGIN);
    let current = probe_codex_skills(&scope, &layout)?
        .context("Missing native skills after plugin change")?;
    assert!(
        !current
            .entries
            .iter()
            .any(|skill| skill.plugin_id.as_deref() == Some(PLUGIN))
    );
    let after = fixture
        .run(
            "plugin-removed-launch",
            "plugin-role",
            for_model(&plugin_model),
        )
        .await?;
    assert_eq!(after.status, RunStatus::Failed);
    assert_wire_scope(&after, &scope, &plugin_model)?;
    assert!(
        after
            .wire
            .iter()
            .all(|event| event["method"] != "turn/start")
    );
    let after_evidence = store
        .workflow_stage_assessment(project.clone(), after.run.clone())
        .await?
        .context("Missing rejected assessment")?;
    assert_eq!(after_evidence.role_template, Some(plugin_role));
    assert_scope(&after_evidence.scope, &scope, &checkout, &plugin_model);
    assert!(
        matches!(&after_evidence.decisions[..], [StageRequirementDecision::Source {
        evidence: None, blocker: Some(reason), .. }] if reason.contains("not reported as enabled"))
    );
    assert!(
        store
            .agent_dispatch(after.conversation, after.run)
            .await?
            .is_none()
    );
    drop(store);
    let reopened = Store::open(database)?;
    let latest = reopened
        .latest_plugin_inventory(Provider::Codex)
        .await?
        .context("Missing refreshed inventory")?;
    assert!(
        !latest
            .report
            .plugins
            .iter()
            .any(|plugin| plugin.id == PLUGIN)
    );
    let retained = reopened
        .agent_dispatch(before.conversation, before.run.clone())
        .await?
        .context("Missing historical dispatch after reopen")?;
    assert_eq!(retained.snapshot.plugin_inventory, Some(inventory));
    assert_eq!(
        reopened
            .workflow_stage_assessment(project, before.run)
            .await?,
        Some(before_evidence)
    );
    Ok(())
}

fn assert_scope(
    observed: &crate::providers::capabilities::CapabilityScope,
    planned: &SessionScope,
    checkout: &str,
    model: &ModelSelection,
) {
    assert_eq!(observed.provider, model.provider);
    assert_eq!(observed.profile_id, model.profile_id);
    assert_eq!(
        observed.requested_model.as_deref(),
        Some(model.model_id.as_str())
    );
    assert_eq!(
        observed.reported_model.as_deref(),
        Some(model.model_id.as_str())
    );
    assert_eq!(observed.checkout.id, checkout);
    assert_eq!(observed.checkout.path, planned.checkout);
    assert_eq!(observed.mode, SessionMode::Plan);
    assert_eq!(observed.cli_version, planned.cli_version);
}

fn assert_wire_scope(
    attempt: &Attempt,
    planned: &SessionScope,
    model: &ModelSelection,
) -> Result<()> {
    let config = attempt
        .wire
        .iter()
        .find(|frame| frame["method"] == "thread/start")
        .context("The attempt did not open its configured Codex thread")?;
    assert_eq!(config["params"]["model"], model.model_id);
    assert_eq!(
        config["params"]["cwd"].as_str().map(PathBuf::from).as_ref(),
        Some(&planned.checkout)
    );
    let native = &attempt.native_thread;
    assert_eq!(native["model"], model.model_id);
    assert_eq!(
        native["cwd"].as_str().map(PathBuf::from).as_ref(),
        Some(&planned.checkout)
    );
    assert_eq!(native["sandbox"]["type"], "readOnly");
    assert_eq!(native["approvalPolicy"], "never");
    Ok(())
}
