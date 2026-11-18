use std::process::Command;

use serde_json::{Value, json};

use crate::{
    jobs::{AdmissionEnvironment, JobPermission, JobRule, JobWorkspace},
    repository::{capture_worktree_start, save_worktree_candidate},
    workflows::{
        ArtifactValue, WorkflowLaunchRequest, WorkflowRevision,
        decision_nodes::{WorkflowDecisionRequest, WorkflowDecisionSubmission},
        generic_agent::tests::complete_response,
    },
};

use super::*;

fn bundled_package(delivery_only: bool) -> Result<WorkflowPackage> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("Find the workspace")?;
    let encoded = Command::new("node")
        .current_dir(root)
        .args([
            "--input-type=module",
            "-e",
            r#"
            import { registerHooks } from "node:module";
            registerHooks({ resolve(specifier, context, nextResolve) {
              const target = specifier.startsWith("@/")
                ? new URL("./src/" + specifier.slice(2) + ".ts", import.meta.url).href
                : specifier;
              return nextResolve(target, context);
            } });
            const { developmentPipelinePackage } = await import("./src/development-pipeline.ts");
            const template = developmentPipelinePackage({
              provider: "codex", profileId: "fixture", modelId: "reviewer",
            });
            if (process.argv[1] === "delivery") {
              // Keep the delivery nodes and their published contracts intact while shortening the fixture run.
              const member = (key) => template.members.find((item) => item.key === key);
              function retainNodes(item, keep) {
                const graph = item.body.graph;
                graph.nodes = graph.nodes.filter((node) => keep(node.id));
                graph.wires = graph.wires.filter((wire) => keep(wire.from.node) && keep(wire.to.node));
                for (const slot of Object.keys(item.references)) {
                  if (!keep(slot.split(":")[0])) delete item.references[slot];
                }
              }
              const root = member("pipeline");
              const snapshot = "prepare-approval-snapshot";
              const rounds = "delivery-and-validation-round";
              const allowed = new Set(["start", snapshot, "approve-work", rounds, "development-release-report", "end"]);
              retainNodes(root, (id) => allowed.has(id));
              root.body.graph.nodes.find(({ id }) => id === rounds).configuration.maxIterations = 7;
              root.references[snapshot + ":input_schema"] = root.references["start:schema"];
              for (const [out, input] of [["start", "ready"], ["inputs", "input"]]) {
                root.body.graph.wires.push({ from: { node: "start", port: out }, to: { node: snapshot, port: input } });
              }
              const delivery = member("function-" + rounds);
              const taskBranches = new Set(["select-second-task", "second-task"]);
              retainNodes(delivery, (id) => !taskBranches.has(id));
              for (const id of ["round-work-fork", "round-work-join"]) {
                delivery.body.graph.nodes.find((node) => node.id === id).configuration.branches = ["slot1", "ci", "audit", "context"];
              }
              const used = new Set();
              function retainMember(item) {
                if (used.has(item.key)) return;
                used.add(item.key);
                for (const reference of Object.values(item.references ?? {})) retainMember(member(reference.key));
              }
              retainMember(root);
              template.members = template.members.filter((item) => used.has(item.key));
            }
            process.stdout.write(JSON.stringify(template));
        "#,
            "--",
            if delivery_only { "delivery" } else { "full" },
        ])
        .output()?;
    ensure!(
        encoded.status.success(),
        "Serialize the built-in template: {}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    Ok(serde_json::from_slice(&encoded.stdout)?)
}

#[tokio::test]
async fn bundled_development_pipeline_publishes_through_the_native_compiler() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("development-template.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let draft = store
        .create_workflow_package(project.clone(), None, bundled_package(false)?)
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let version = &artifact(&published, "pipeline").version;
    let selected = WorkflowRevision {
        id: version.id.clone(),
        revision: version.revision.clone(),
    };
    store
        .execute(move |db| {
            let definition = crate::workflows::load_definition(db, &project, &selected)?
                .context("The bundled pipeline was not persisted")?;
            assert!(definition.unsupported_node().is_none());
            assert!(
                definition
                    .nodes
                    .iter()
                    .any(crate::workflows::write_scope::writer)
            );
            assert!(definition.nodes.len() <= 4096);
            Ok(())
        })
        .await
}

fn backlog_task(key: &str, kind: &str, depends_on: &[&str]) -> Value {
    json!({"key":key,"title":key,"description":"fixture task","kind":kind,"priority":1,
        "depends_on":depends_on,"requirement_ids":["R1"],"scenario_ids":["S1"],"acceptance_criteria":["Example works"]})
}

struct Scenario {
    merge_approved: bool,
}

impl Scenario {
    fn response(&self, node: &str, prompt: &str) -> Result<Value> {
        let mut parts = node.rsplit("::");
        ensure!(parts.next() == Some("execute"), "Unexpected agent {node}");
        let stage = parts.next().context("Expected a named agent stage")?;
        let iteration = node
            .strip_prefix("delivery-and-validation-round::iteration-")
            .and_then(|suffix| suffix.split("::").next())
            .and_then(|index| index.parse::<u32>().ok())
            .unwrap_or(0);
        let documentation = iteration >= 5;
        let key = if documentation {
            "documentation"
        } else {
            "TASK-1"
        };
        let head = match iteration {
            1 => "a",
            2..=4 => "b",
            _ => "c",
        }
        .repeat(40);
        let tasks = vec![
            backlog_task("TASK-1", "implementation", &[]),
            backlog_task("documentation", "documentation", &["TASK-1"]),
            backlog_task("release-checks", "audit", &[]),
        ];
        let backlog =
            json!({"approved":true,"context":"approved specification R1 S1","tasks":tasks});
        let idle = json!({"task_key":"","candidate":false,"poll":false,"context":""});
        let dispatch = json!({"context":"fixture round","slot2":idle,
            "slot1":{"task_key":key,"candidate":iteration == 5,"poll":false,"context":"fixture scope"},
            "ci":{"task_key":key,"candidate":false,"poll":matches!(iteration, 1..=3 | 6),"context":"fixture scope"},
            "audit":{"enabled":matches!(iteration, 4 | 7),"task_key":"release-checks","context":"integrated scope"}});
        let progress = |status| {
            json!({"task_key":key,"base_sha":"fixture-base","status":status,"context":"fixture scope","checks":[],
            "head_sha":head,"pr_url":"https://example.invalid/pull/1",
            "integration_sha": if status == "merged" { head.as_str() } else { "" }})
        };
        let gate = |status| {
            let mut result = progress(status);
            result["approved"] = json!(true);
            result
        };
        let ci = || {
            let mut result = progress(match iteration {
                1 => "pending",
                2 => "retry",
                _ => "passed",
            });
            result["can_merge"] = json!(matches!(iteration, 3 | 6));
            result["human_required"] = json!(!documentation);
            result["base_branch"] = json!("main");
            result
        };
        let round = |stop, status| json!({"tasks":[],"stop":stop,"status":status,"context":"fixture round"});

        Ok(match stage {
            "prepare-approval-snapshot" => backlog,
            "dispatch-backlog-work" => dispatch,
            "select-first-task" => dispatch["slot1"].clone(),
            "select-ci-task" => dispatch["ci"].clone(),
            "select-integrated-audit" => dispatch["audit"].clone(),
            "baseline-and-red" | "red-evidence-gate" => gate("red"),
            "implement-or-fix" | "independent-qa-and-automated-checks" => progress("green"),
            "quality-gate" => gate("green"),
            "build-commit-and-open-pull-request"
            | "record-candidate-outcome"
            | "record-implementation-attempt" => progress("pending"),
            "inspect-pull-request-checks"
            | "prepare-merge-authorization"
            | "show-exact-merge-revision" => ci(),
            "integrate-approved-candidate" => {
                ensure!(
                    iteration == 3 && self.merge_approved,
                    "High-risk merge reached the writer before exact-head approval"
                );
                ensure!(
                    prompt.contains(&head),
                    "The approved merge writer received the wrong head"
                );
                progress("merged")
            }
            "integrate-low-risk-candidate" => {
                ensure!(
                    iteration == 6 && prompt.contains(&head),
                    "The automatic merge received the wrong head"
                );
                progress("merged")
            }
            "record-merge-outcome" => {
                ensure!(prompt.contains("merged"), "The merge result was not joined");
                progress("merged")
            }
            "record-ci-outcome" => {
                let status = match iteration {
                    1 => "pending",
                    2 => "retry",
                    _ => "merged",
                };
                ensure!(
                    prompt.contains(&format!("\"status\":\"{status}\""))
                        || prompt.contains(&format!("\"status\": \"{status}\"")),
                    "The CI join lost its {status} evidence"
                );
                progress(status)
            }
            "integrated-regression-and-build" => {
                let mut result = progress("passed");
                result["task_key"] = json!("release-checks");
                result
            }
            "integrated-audit-outcome" => {
                if iteration == 7 {
                    round(true, "ready")
                } else {
                    round(false, "documentation-required")
                }
            }
            "settle-delivery-round" => {
                if iteration == 4 {
                    ensure!(
                        prompt.contains("documentation-required"),
                        "The preliminary audit did not request documentation"
                    );
                }
                if iteration == 6 {
                    ensure!(
                        prompt.contains(&head),
                        "The integrated documentation SHA was lost"
                    );
                }
                round(
                    iteration == 7,
                    if iteration == 7 { "ready" } else { "running" },
                )
            }
            "development-release-report" => {
                ensure!(
                    prompt.contains("ready"),
                    "The release report lost the final audit"
                );
                json!({"status":"ready","integrated_sha":head,"deliveries":[],"evidence":["Final audit passed"],"blockers":[]})
            }
            other if other.ends_with("-review") => {
                json!({"subject":stage,"dimension":"fixture","verdict":"approved","risk":"low","findings":[]})
            }
            _ => anyhow::bail!("No fixture response for {node}"),
        })
    }
}

#[tokio::test]
async fn bundled_development_pipeline_runs_through_native_delivery_and_final_audit() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("development-template.sqlite");
    let mut store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    store
        .set_project_job_rules(
            project.clone(),
            Vec::new(),
            vec![JobRule {
                role: JobRole::Agent,
                operation: SessionMode::ApprovedWrite,
                workspace: JobWorkspace::Project,
                permission: JobPermission::Allow,
                requirements: Default::default(),
            }],
        )
        .await?;
    let draft = store
        .create_workflow_package(project.clone(), None, bundled_package(true)?)
        .await?;
    let published = commit(&store, &project, &draft).await?;
    let version = &artifact(&published, "pipeline").version;
    let selected = WorkflowRevision {
        id: version.id.clone(),
        revision: version.revision.clone(),
    };
    let request: WorkflowLaunchRequest = serde_json::from_value(json!({
        "projectId":project,"checkoutId":repo.checkout_id,"requestId":"template-run",
        "workflow":selected,"inputs":{"request":"Add an example","destination_branch":"main","remote":"origin"}
    }))?;
    let run = store.launch_workflow_request(request).await?;
    let mut scenario = Scenario {
        merge_approved: false,
    };
    let mut waits = 0;
    for _ in 0..100 {
        if let Some(stage) = store.next_workflow_agent().await? {
            let (_, node) = stage.fixture_identity();
            let submission = stage
                .fixture_submission(&store)
                .await
                .with_context(|| format!("Prepare {node}"))?;
            stage
                .enqueue(&store, submission)
                .await
                .with_context(|| format!("Enqueue {node}"))?;
            let attempt = store
                .claim_next_job(AdmissionEnvironment {
                    runner: "fixture-runner".into(),
                    bridge_available: false,
                })
                .await?
                .with_context(|| format!("Admit {node}"))?;
            let response = scenario.response(node, &attempt.input[0].text)?;
            if attempt.scope.mode == SessionMode::ApprovedWrite {
                ensure!(
                    capture_worktree_start(&store, attempt.run.id.clone()).await?,
                    "Capture the managed worktree for {node}"
                );
                save_worktree_candidate(&store, attempt.run.id.clone()).await?;
            }
            complete_response(&store, &attempt, &response.to_string(), None)
                .await
                .with_context(|| format!("Complete {node}"))?;
            continue;
        }
        let inspection = store
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("Workflow run unavailable")?;
        if let Some(decision) = inspection.pending_decisions.first() {
            let values = if decision.node_id == "approve-work" {
                json!({"authorizeWrites":true})
            } else {
                ensure!(
                    decision.node_id.ends_with("::approve-merge"),
                    "Unexpected decision {}",
                    decision.node_id
                );
                let WorkflowDecisionRequest::Human {
                    original: Some(input),
                    ..
                } = &decision.request
                else {
                    anyhow::bail!("The merge decision lacks exact-head evidence");
                };
                assert_eq!(input["head_sha"], "b".repeat(40));
                assert!(!scenario.merge_approved);
                scenario.merge_approved = true;
                json!({})
            };
            let submission: WorkflowDecisionSubmission = serde_json::from_value(json!({
                "projectId":project,"runId":run.id,"requestId":decision.request_id,
                "decision":{"kind":"human_approval","approved":true,"values":values}
            }))?;
            store.resolve_workflow_stage_decision(submission).await?;
        } else if !inspection.timed_waits.is_empty() {
            waits += 1;
            if waits == 1 {
                drop(store);
                store = Store::open(database.clone())?;
            }
            let run_id = run.id.clone();
            store.execute(move |db| {
                db.execute("UPDATE workflow_wait_deadlines SET deadline_at = 0 WHERE pipeline_run_id = ?1 AND state = 'waiting'", [run_id])?;
                Ok(())
            }).await?;
            assert_eq!(store.refresh_due_workflow_waits().await?, 1);
        } else {
            let output = inspection
                .outputs
                .iter()
                .find(|output| output.current && output.artifact.node_instance_id == "end")
                .with_context(|| {
                    format!(
                        "Workflow stopped before the release report after {} jobs",
                        inspection.jobs.len()
                    )
                })?;
            assert!(
                matches!(&output.artifact.value, ArtifactValue::JsonObject(result) if result["status"] == "ready")
            );
            assert!(scenario.merge_approved);
            assert_eq!(waits, 4);
            return Ok(());
        }
    }
    anyhow::bail!("The bundled pipeline did not finish")
}
