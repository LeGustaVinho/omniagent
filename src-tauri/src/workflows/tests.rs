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
