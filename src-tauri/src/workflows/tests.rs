use crate::workflows::ArchivedNode;

use std::{path::Path, process::Command};

use anyhow::{Result, ensure};

use super::*;
use crate::{
    repository::{self, ProjectCheckout},
    store::Store,
};

pub(crate) fn simple_graph(name: &str) -> WorkflowGraph {
    WorkflowGraph {
        name: name.into(),
        nodes: vec![
            NodeInstance {
                id: "start".into(),
                kind: BuiltinNode::Start,
                configuration: NodeConfiguration::None,
            },
            NodeInstance {
                id: "end".into(),
                kind: BuiltinNode::End,
                configuration: NodeConfiguration::End,
            },
        ],
        wires: vec![Wire {
            from: Endpoint {
                node: "start".into(),
                port: "start".into(),
            },
            to: Endpoint {
                node: "end".into(),
                port: "ready".into(),
            },
        }],
    }
}

#[tokio::test]
async fn retired_node_is_readable_but_cannot_be_saved_or_launched() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("retired.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let registry = builtin_registry();
    assert_eq!(registry.len(), 10);
    assert!(
        registry
            .iter()
            .all(|definition| definition.kind.is_supported())
    );
    assert!(
        registry.iter().all(
            |definition| definition.kind != BuiltinNode::Archived(ArchivedNode::ValidateOutput)
        )
    );

    let mut graph = simple_graph("Archived workflow");
    graph.nodes[1].kind = BuiltinNode::Archived(ArchivedNode::ValidateOutput);
    assert!(
        store
            .save_workflow_revision(project.clone(), "retired".into(), graph.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("retired node type")
    );

    let mut saved = compile("retired".into(), simple_graph("Archived workflow"))?;
    saved.nodes[1].instance = graph.nodes.remove(1);
    saved.nodes[1].definition.kind = BuiltinNode::Archived(ArchivedNode::ValidateOutput);
    saved.nodes[1].definition.title = "Validate output".into();
    let version = saved.version.clone();
    let owner = project.clone();
    store
        .execute(move |db| {
            db.execute(
                "INSERT INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    owner,
                    saved.version.id,
                    saved.version.revision,
                    crate::protection::protect(&serde_json::to_vec(&saved)?)?
                ],
            )?;
            Ok(())
        })
        .await?;
    let archive = store
        .workflow_definition(project.clone(), version.clone())
        .await?
        .expect("Existing revisions stay readable");
    assert_eq!(
        archive.unsupported_node().unwrap().definition.title,
        "Validate output"
    );
    let error = store
        .start_workflow_run(project.clone(), version)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Validate output"));
    assert!(store.list_workflow_runs(project).await?.is_empty());
    Ok(())
}

#[test]
fn planner_units_require_a_closed_block_and_keep_purpose_order_and_revision() -> Result<()> {
    let body = r#"{"units":[{"purpose":"First","completionCriteria":["Build passes"]},{"purpose":"Second","completionCriteria":["Tests pass"]}]}"#;
    let unclosed = format!("Plan\n```omniagent-plan\n{body}");
    assert!(super::units::parse_plan_units("revision-one", &unclosed).is_err());
    let closed = format!("{unclosed}\n```");
    let units = super::units::parse_plan_units("revision-one", &closed)?;
    assert_eq!(units[0].purpose, "First");
    assert_eq!(units[0].completion_criteria, ["Build passes"]);
    assert_eq!(units[1].ordinal, 2);
    assert_eq!(units[1].purpose, "Second");
    assert_ne!(
        units[0].id,
        super::units::parse_plan_units("revision-two", &closed)?[0].id
    );
    Ok(())
}

#[tokio::test]
async fn saved_cross_review_preserves_legacy_verdict_branches() -> Result<()> {
    use crate::jobs::AdmissionEnvironment;
    use crate::workflows::authoring::package::WorkflowPackagePublishOutcome;
    use crate::workflows::control::ControlStageState;
    use crate::workflows::generic_agent::tests::complete_response;
    use serde_json::json;

    let directory = tempfile::tempdir()?;
    let store = Store::open(directory.path().join("cross-review.sqlite"))?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = repo.project_id;
    let role = |key: &str, policy: &str| {
        json!({
            "key":key, "name":key, "body":{"kind":"role", "template":{
                "name":key, "role":"agent", "model":{"provider":"codex", "profileId":"fixture", "modelId":"fixture-model"},
                "instructions":"Review the fixture.", "toolScope":"read_only",
                "completionContract":"Return the schema result.", "reviewPolicy":policy,
            }}
        })
    };
    let endpoint = |node: &str, port: &str| Endpoint {
        node: node.into(),
        port: port.into(),
    };
    let wire = |node: &str, port: &str, target: &str, input: &str| Wire {
        from: endpoint(node, port),
        to: endpoint(target, input),
    };
    let draft = store.create_workflow_package(project.clone(), None, serde_json::from_value(json!({
        "name":"Cross-review fixture", "members":[
            role("author", "cross_review"), role("reviewer", "independent"),
            {"key":"text", "name":"Text", "body":{"kind":"schema", "fields":[
                {"name":"reply", "type":"string", "required":true}
            ]}},
            {"key":"review", "name":"Review", "body":{"kind":"prompt", "prompt":{
                "text":"Review {{input.reply}}.", "schemaContract":true, "executor":"agent"
            }}, "references":{
                "role":{"source":"draft", "key":"author"},
                "reviewer":{"source":"draft", "key":"reviewer"},
                "input_schema":{"source":"draft", "key":"text"},
                "output_schema":{"source":"draft", "key":"text"}
            }},
            {"key":"pipeline", "name":"Review pipeline", "body":{"kind":"pipeline", "graph":{
                "name":"Review pipeline", "nodes":[
                    {"id":"start","kind":"typed_start","configuration":{"kind":"typed_start","fields":[]}},
                    {"id":"review","kind":"subflow","configuration":{"kind":"none"}},
                    {"id":"end","kind":"end","configuration":{"kind":"end"}}
                ], "wires":[
                    wire("start", "start", "review", "ready"), wire("start", "inputs", "review", "input"),
                    wire("review", "completed", "end", "ready"), wire("review", "result", "end", "result"),
                ]
            }}, "references":{
                "start:schema":{"source":"draft", "key":"text"}, "review":{"source":"draft", "key":"review"}
            }}
        ]
    }))?).await?;
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.clone(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("The cross-review fixture was not published");
    };
    let artifact = &publication
        .members
        .iter()
        .find(|member| member.key == "pipeline")
        .context("Missing cross-review pipeline")?
        .artifact;
    let version = WorkflowRevision {
        id: artifact.version.id.clone(),
        revision: artifact.version.revision.clone(),
    };
    let mut original = store
        .workflow_definition(project.clone(), version.clone())
        .await?
        .context("Missing workflow definition")?;
    let reviewer = original
        .nodes
        .iter_mut()
        .find(|node| node.instance.id == "review::reviewer")
        .context("Missing reviewer")?;
    assert!(reviewer.instance.validate_authored().is_err());
    let NodeConfiguration::AgentRole { requirements, .. } = &mut reviewer.instance.configuration
    else {
        anyhow::bail!("Expected reviewer Agent settings");
    };
    requirements
        .structured
        .as_mut()
        .context("Missing reviewer schema")?
        .review_verdict = false;
    let failure = original
        .nodes
        .iter_mut()
        .find(|node| node.instance.id == "review::failure")
        .context("Missing cross-review failure join")?;
    failure.instance.kind = BuiltinNode::Archived(ArchivedNode::Join);
    failure.instance.configuration = NodeConfiguration::None;
    failure.definition.kind = failure.instance.kind;
    failure.definition.configuration = ConfigurationKind::None;

    let mut verdict = builtin_registry()
        .into_iter()
        .find(|node| node.kind == BuiltinNode::JoinResults)
        .context("Missing Join definition")?;
    registry::configure_legacy_review_verdict(&mut verdict);
    let verdict_id = "review::verdict";
    assert!(
        NodeInstance {
            id: "authored-verdict".into(),
            kind: BuiltinNode::JoinResults,
            configuration: NodeConfiguration::LegacyReviewVerdict,
        }
        .validate_authored()
        .is_err()
    );
    verdict.kind = BuiltinNode::Archived(ArchivedNode::Condition);
    verdict.title = "Condition".into();
    verdict.configuration = ConfigurationKind::Condition;
    original.nodes.push(PinnedNode {
        instance: NodeInstance {
            id: verdict_id.into(),
            kind: verdict.kind,
            configuration: NodeConfiguration::Condition {
                predicate: ConditionPredicate::Equals {
                    pointer: "/evidence/approved".into(),
                    value: json!(true),
                },
            },
        },
        revision: "historical-verdict-pin".into(),
        definition: verdict,
    });
    original
        .subflows
        .iter_mut()
        .find(|scope| scope.path == "review")
        .context("Missing AI Node scope")?
        .nodes
        .push(verdict_id.into());
    original
        .wires
        .iter_mut()
        .find(|wire| wire.to == endpoint("review::return", "ready"))
        .context("Missing reviewer completion")?
        .from = endpoint(verdict_id, "matched");
    original.wires.extend([
        wire("review::reviewer", "completed", verdict_id, "ready"),
        wire("review::reviewer", "result", verdict_id, "value"),
        wire(verdict_id, "unmatched", "review::failure", "branches"),
    ]);
    let mut hand_authored = original.clone();
    hand_authored.subflows.clear();
    hand_authored.normalize_legacy_prompt_nodes();
    assert!(hand_authored.ensure_supported_nodes().is_err());
    assert!(
        hand_authored
            .nodes
            .iter()
            .find(|node| node.instance.id == verdict_id)
            .is_some_and(
                |node| node.instance.kind == BuiltinNode::Archived(ArchivedNode::Condition)
            )
    );

    let owner = project.clone();
    store
        .execute(move |db| {
            db.execute(
                "UPDATE workflow_revisions SET definition = ?4
             WHERE project_id = ?1 AND workflow_id = ?2 AND revision = ?3",
                rusqlite::params![
                    owner,
                    original.version.id,
                    original.version.revision,
                    crate::protection::protect(&serde_json::to_vec(&original)?)?
                ],
            )?;
            Ok(())
        })
        .await?;
    let respond = |store: Store, stage_id: &'static str, text: String| async move {
        let stage = store
            .next_workflow_agent()
            .await?
            .context("Missing ready cross-review Agent")?;
        assert_eq!(stage.fixture_identity().1, stage_id);
        stage
            .enqueue(&store, stage.fixture_submission(&store).await?)
            .await?;
        let attempt = store
            .claim_next_job(AdmissionEnvironment {
                runner: "fixture-runner".into(),
                bridge_available: false,
            })
            .await?
            .context("Cross-review Agent did not pass admission")?;
        complete_response(&store, &attempt, &text, None).await?;
        anyhow::Ok(())
    };
    for approved in [true, false] {
        let run = store.launch_workflow_request(serde_json::from_value(json!({
            "projectId":project, "checkoutId":repo.checkout_id, "requestId":format!("legacy-cross-review-{approved}"),
            "workflow":version, "inputs":{"reply":"Hello"}
        }))?).await?;
        respond(
            store.clone(),
            "review::execute",
            String::from(r#"{"reply":"candidate"}"#),
        )
        .await?;
        respond(
            store.clone(),
            "review::reviewer",
            json!({"approved":approved}).to_string(),
        )
        .await?;
        let snapshot = store
            .inspect_workflow_execution(project.clone(), run.id)
            .await?
            .context("Missing archived-format run")?;
        assert_eq!(snapshot.definition.version, version);
        let node = snapshot
            .definition
            .nodes
            .iter()
            .find(|node| node.instance.id == verdict_id)
            .context("Missing legacy verdict")?;
        assert_eq!(
            node.instance.configuration,
            NodeConfiguration::LegacyReviewVerdict
        );
        assert_eq!(node.revision, "historical-verdict-pin");
        let verdict = snapshot
            .control
            .iter()
            .find(|receipt| receipt.node_id == verdict_id)
            .context("Missing legacy verdict receipt")?;
        assert_eq!(
            verdict.selected_ports,
            [if approved { "matched" } else { "unmatched" }]
        );
        assert!(
            snapshot
                .control
                .iter()
                .any(|receipt| receipt.node_id == "end"
                    && receipt.state
                        == if approved {
                            ControlStageState::Completed
                        } else {
                            ControlStageState::Skipped
                        })
        );
        assert!(
            snapshot
                .control
                .iter()
                .any(|receipt| receipt.node_id == "review::failure"
                    && receipt.state
                        == if approved {
                            ControlStageState::Skipped
                        } else {
                            ControlStageState::Completed
                        })
        );
    }
    Ok(())
}

async fn checkout(store: &Store, path: &Path) -> Result<ProjectCheckout> {
    std::fs::create_dir(path)?;
    ensure!(
        Command::new("git.exe")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()?
            .success()
    );
    std::fs::write(path.join("codex.exe"), b"workflow fixture executable")?;
    repository::register(store, dunce::canonicalize(path)?).await
}

pub(super) async fn approved_delivery_repository(
    store: &Store,
    temp_path: &Path,
) -> Result<ProjectCheckout> {
    let repo = checkout(store, &temp_path.join("builtin-source")).await?;
    let root = Path::new(&repo.checkout_root);
    std::fs::write(root.join("brief.md"), "Two independent changes")?;
    std::fs::write(root.join("first.txt"), "original first\n")?;
    std::fs::write(root.join("second.txt"), "original second\n")?;
    for args in [
        vec!["add", "brief.md", "first.txt", "second.txt"],
        vec![
            "-c",
            "user.name=Workflow Test",
            "-c",
            "user.email=workflow@example.test",
            "commit",
            "-qm",
            "Base",
        ],
    ] {
        ensure!(
            Command::new("git.exe")
                .args(args)
                .current_dir(root)
                .status()?
                .success()
        );
    }
    Ok(repo)
}
