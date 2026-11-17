use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::workflows::{ArtifactValue, WorkflowArtifact, data_nodes::EvidenceOutcome};

use super::{FixtureScript, LocalGithub, ProjectCheckout, Store, WorkflowRun};

const DESIGN_FINDING: &str = "Pin release evidence to the integrated Git SHA";

pub(super) async fn plan_and_materialize(
    store: &Store,
    repo: &ProjectCheckout,
    workflow: &WorkflowRun,
    script: &FixtureScript,
    github: &LocalGithub,
) -> Result<()> {
    for _ in 0..72 {
        store.settle_ready_agile_stages().await?;
        let artifacts = store
            .workflow_run_artifacts(repo.project_id.clone(), workflow.id.clone())
            .await?;
        if let Some(backlog) = artifacts.iter().find(|artifact| {
            artifact.node_instance_id == "materialize-backlog" && artifact.port == "backlog"
        }) {
            assert_planning(&artifacts, backlog)?;
            return Ok(());
        }
        if let Some(action) = store.next_workflow_github_action().await? {
            action
                .execute_with_client(store, github)
                .await
                .context("Run public research through the GitHub node")?;
            continue;
        }
        let Some(pending) = store.next_workflow_agent().await? else {
            let trace = store
                .inspect_workflow_execution(repo.project_id.clone(), workflow.id.clone())
                .await?
                .context("The launched Audit workflow disappeared")?;
            let terminal: Vec<_> = trace
                .control
                .iter()
                .map(|receipt| (&receipt.node_id, &receipt.selected_ports))
                .collect();
            anyhow::bail!(
                "The published planning workflow stopped before materialization: {terminal:?}"
            );
        };
        let (run, node) = pending.fixture_identity();
        assert_eq!(run, workflow.id);
        let answer = response(node)
            .with_context(|| format!("Script the published planning agent {node}"))?;
        let attempt = script.claim(store, repo, &pending).await?;
        if second_design(node) {
            ensure!(
                attempt
                    .input
                    .iter()
                    .any(|message| message.text.contains(DESIGN_FINDING)),
                "The selective revision did not receive the recorded reviewer finding"
            );
        }
        script.complete_response(store, &attempt, &answer).await?;
    }
    anyhow::bail!("The published Audit planning exceeded its scripted stages")
}

fn response(node: &str) -> Result<Value> {
    let leaf = node.rsplit("::").next().unwrap_or(node);
    Ok(match leaf {
        "prepare-public-search-agent" => json!({"query":"export testing"}),
        "analyze-repository-agent" => {
            json!({"repositoryAnalysis":"The pinned repository contains brief.md and two tracked export fixture files. Git provides the integration history."})
        }
        "analyze-requirements-agent" => {
            json!({"requirements":"Deliver three verified export tasks while preserving validated output. The dependent final task requires its prerequisite merge.", "needsClarification":false, "questions":[]})
        }
        "analyze-research-agent" => {
            json!({"researchFindings":"The public fixture repository provides a reference revision and license. Adopt the checks-first pattern and use local Git evidence for implementation behavior."})
        }
        "write-master-plan-agent" => {
            json!({"masterPlan":"Specify independent export changes, pin regression and acceptance criteria, release prerequisite branches by verified merge, and run an integrated release audit."})
        }
        "write-sdd-agent" => sdd(),
        "write-bdd-agent" => bdd(),
        "write-design-agent" => design(second_design(node)),
        "write-test-strategy-agent" => {
            json!({"testStrategy":"Validate baseline, intentional RED, GREEN and regression at the integrated revision. Mutation, E2E and performance checks are inapplicable to the fixture." , "requiredChecks":[]})
        }
        "review-design-agent" if node.contains("iteration-1::") => {
            json!({"approved":false,"summary":"The release evidence needs an exact Git revision.","findings":[{"subject":"design","problem":"The design does not define the revision pinned to release evidence.","revision":DESIGN_FINDING}]})
        }
        "write-backlog-agent" => json!({
            "epicTitle":"Deliver audited exports",
            "tasks":[task("A", "Deliver primary export", 3, &[]), task("B", "Deliver fallback export", 4, &[]),
                task("C", "Deliver integrated export", 2, &["B"])],
        }),
        name if name.starts_with("review-") && name.ends_with("-agent") => approval(),
        _ => anyhow::bail!("Unrecognized planning agent {node}"),
    })
}

pub(super) fn approval() -> Value {
    json!({"approved":true,"summary":"The supplied evidence meets the fixture's review criteria.","findings":[]})
}

fn second_design(node: &str) -> bool {
    node.contains("iteration-2::") && node.ends_with("::write-design-agent")
}

fn design(revised: bool) -> Value {
    let description = if revised {
        "Keep each export change separate. Pin release evidence to the integrated Git SHA."
    } else {
        "Keep each export change separate."
    };
    json!({"technicalDesign":description})
}

fn sdd() -> Value {
    json!({"overview":"Deliver stable, verified export behavior.", "requirements":[{
        "id":"REQ-1", "kind":"functional", "description":"Produce verified exports for matching entries. With no matches, return an empty result.",
        "acceptanceCriteria":["The integrated export behavior passes the registered checks."],
    }]})
}

fn bdd() -> Value {
    json!({"scenarios":[
        {"id":"BDD-1","type":"positive","requirements":["REQ-1"],"given":"Matching entries","when":"Export runs","then":"Matching entries are exported"},
        {"id":"BDD-2","type":"negative","requirements":["REQ-1"],"given":"No matching entries","when":"Export runs","then":"The result is empty"},
        {"id":"BDD-3","type":"boundary","requirements":["REQ-1"],"given":"Exactly one matching entry","when":"Export runs","then":"Exactly one entry is exported"},
    ]})
}

fn task(key: &str, title: &str, priority: u8, dependencies: &[&str]) -> Value {
    json!({"key":key,"title":title,"description":"Conform to the approved export behavior and verify the change on the delivery branch.",
      "priority":priority,"dependsOn":dependencies,"acceptanceCriteria":["The assigned export change passes its tests."],
      "sddRequirements":["REQ-1"],"bddScenarios":["BDD-1","BDD-2","BDD-3"]})
}

fn assert_planning(artifacts: &[WorkflowArtifact], backlog: &WorkflowArtifact) -> Result<()> {
    let ArtifactValue::GithubResearch(research) = &artifacts
        .iter()
        .find(|artifact| artifact.node_instance_id == "research" && artifact.port == "research")
        .context("The published plan lost the native public research receipt")?
        .value
    else {
        anyhow::bail!("The GitHub node did not return typed public research");
    };
    assert_eq!(research.query, "export testing");
    assert_eq!(research.repositories.len(), 1);
    assert_eq!(research.repositories[0].license.as_deref(), Some("MIT"));
    assert_eq!(research.repositories[0].revision.len(), 40);
    let ArtifactValue::Backlog(backlog) = &backlog.value else {
        anyhow::bail!("Backlog materialization did not issue a native receipt");
    };
    assert_eq!(backlog.tasks.len(), 3);
    let tasks: Vec<_> = backlog
        .tasks
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<_, _>>()?;
    let task = |key| {
        tasks
            .iter()
            .find(|task| task["specification"]["key"] == key)
            .expect("fixture task key")
    };
    assert_eq!(task("A")["specification"]["dependsOn"], json!([]));
    assert_eq!(task("B")["specification"]["dependsOn"], json!([]));
    assert_eq!(task("C")["specification"]["dependsOn"], json!(["B"]));
    let approved_design = &backlog.sources["design"];
    let evidence = |artifact: &WorkflowArtifact| -> Result<Value> {
        let ArtifactValue::JsonObject(output) = &artifact.value else {
            anyhow::bail!("The validated planning output is not structured");
        };
        Ok(Value::Object(output.clone()))
    };
    let selected_design = artifacts
        .iter()
        .find(|artifact| &artifact.id == approved_design)
        .context("The backlog lost its approved technical design")?;
    assert_eq!(evidence(selected_design)?, design(true));
    for (stage, before, after) in [
        ("write-sdd", sdd(), sdd()),
        ("write-bdd", bdd(), bdd()),
        ("write-design", design(false), design(true)),
    ] {
        let get = |round: u8| -> Result<&WorkflowArtifact> {
            let suffix = format!("iteration-{round}::{stage}");
            artifacts
                .iter()
                .find(|artifact| {
                    artifact.port == "object" && artifact.node_instance_id.ends_with(&suffix)
                })
                .with_context(|| format!("The published plan lost {suffix} evidence"))
        };
        let initial = get(1)?;
        let revised = get(2)?;
        assert_eq!(evidence(initial)?, before);
        assert_eq!(evidence(revised)?, after);
        if stage == "write-design" {
            assert_ne!(&initial.id, approved_design);
        }
    }
    for (reviewer, unchanged) in [
        ("review-sdd", true),
        ("review-bdd", true),
        ("review-test-strategy", true),
        ("review-design", false),
        ("review-global-consistency", false),
    ] {
        let subject = |round: u8| -> Result<Vec<Value>> {
            let suffix = format!("iteration-{round}::{reviewer}-prompt");
            let artifact = artifacts
                .iter()
                .find(|entry| entry.port == "evidence" && entry.node_instance_id.ends_with(&suffix))
                .with_context(|| format!("The review lost its pinned subject {suffix}"))?;
            let ArtifactValue::EvidenceSet(set) = &artifact.value else {
                anyhow::bail!("Expected a review subject evidence set");
            };
            set.iter()
                .map(|entry| match &entry.outcome {
                    EvidenceOutcome::Completed { value, .. } => Ok(value.clone()),
                    _ => anyhow::bail!("A reusable review subject was incomplete"),
                })
                .collect()
        };
        assert_eq!(
            subject(1)? == subject(2)?,
            unchanged,
            "Unexpected revision scope for {reviewer}"
        );
    }
    Ok(())
}
