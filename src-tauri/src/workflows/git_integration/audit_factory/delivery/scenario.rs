use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    conversations::AttemptPlan,
    repository::{
        self,
        verification::{CandidateGate, CandidateGateAction},
    },
    runs::RunStatus,
    store::Store,
    workflows::{ArtifactKind, ArtifactValue},
};

use super::{
    super::super::{
        backlog_saga::{DESTINATION, github_fixture::FAILED_CONTENT},
        git,
    },
    Child,
};

const RED_REASON: &str = "the expected behavior marker is absent";
pub(super) const REPAIRED_CONTENT: &str = "B passed repaired CI";
const CHECKS: &[&str] = &[
    "Pipeline unit",
    "Pipeline integration",
    "Pipeline functional",
    "Pipeline static",
    "Pipeline dependencies",
    "Pipeline candidate regression",
];

pub(super) async fn register_checks(store: &Store, project: &str) -> Result<()> {
    let powershell =
        PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let test_command = r#"
        $tests = @(Get-ChildItem -ErrorAction SilentlyContinue -Path 'tests/*.case' | ForEach-Object {
            $id = $_.BaseName
            if (Test-Path -LiteralPath "behavior/$id.txt") { @{id=$id; status='passed'} }
            else { @{id=$id; status='failed'; reason='the expected behavior marker is absent'} }
        })
        @{status='completed'; tests=$tests} | ConvertTo-Json -Compress -Depth 4
        if (@($tests | Where-Object {$_.status -eq 'failed'}).Count -gt 0) { exit 1 }
        exit 0
    "#;
    let build_command = r#"
        if (Test-Path -LiteralPath 'fixture-build.broken') {
            [Console]::Error.WriteLine('The fixture build is missing its generated product')
            exit 17
        }
        if (!(Test-Path -LiteralPath 'brief.md')) { exit 18 }
        exit 0
    "#;
    let command = |name: &str, script: &str| CandidateGate {
        name: name.into(),
        required: false,
        action: CandidateGateAction::Command {
            executable: powershell.clone(),
            arguments: ["-NoProfile", "-NonInteractive", "-Command", script]
                .map(str::to_owned)
                .to_vec(),
            timeout_seconds: Some(20),
        },
    };
    let mut gates: Vec<_> = CHECKS
        .iter()
        .map(|name| {
            command(
                name,
                "if (!(Test-Path -LiteralPath 'brief.md')) { exit 18 }; exit 0",
            )
        })
        .collect();
    gates.push(command("Pipeline test", test_command));
    gates.push(command("Pipeline regression", test_command));
    gates.push(command("Pipeline build", build_command));
    store
        .set_candidate_gate_policy(project.into(), gates)
        .await?;
    Ok(())
}

pub(super) async fn repair_attempt(store: &Store, project: &str, run: &str) -> Result<u16> {
    let plan = store
        .workflow_run(project.into(), run.into())
        .await?
        .context("The child run is unavailable")?
        .plan_revision
        .context("The child has no approved plan")?;
    let artifact = store
        .resolve_workflow_artifact(project.into(), run.into(), plan, ArtifactKind::Plan)
        .await?
        .context("The child plan artifact is unavailable")?;
    let ArtifactValue::BacklogPlan(plan) = artifact.value else {
        anyhow::bail!("The factory lost its backlog plan")
    };
    Ok(plan.repair.map_or(0, |repair| repair.attempt))
}

fn test_id(child: &Child, repair: u16) -> String {
    format!("{}-{repair}", child.key.to_ascii_lowercase())
}

pub(super) async fn write_candidate(
    store: &Store,
    attempt: &AttemptPlan,
    child: &Child,
    node: &str,
    repair: u16,
) -> Result<()> {
    let run = &attempt.run.id;
    let checkout = &attempt.scope.checkout;
    store.mark_agent_running(run.clone()).await?;
    ensure!(
        repository::capture_worktree_start(store, run.clone()).await?,
        "The factory writer lost its worktree capture"
    );
    let id = test_id(child, repair);
    if node.ends_with("write-failing-tests") {
        fs::create_dir_all(checkout.join("tests"))?;
        fs::write(
            checkout.join("tests").join(format!("{id}.case")),
            "Registered acceptance scenario\n",
        )?;
    } else if node.ends_with("implement-green") {
        fs::create_dir_all(checkout.join("behavior"))?;
        fs::write(
            checkout.join("behavior").join(format!("{id}.txt")),
            "Behavior is implemented\n",
        )?;
        if child.key == "B" {
            fs::write(
                checkout.join("first.txt"),
                if repair == 0 {
                    FAILED_CONTENT
                } else {
                    REPAIRED_CONTENT
                },
            )?;
            let broken = checkout.join("fixture-build.broken");
            if repair == 0 && node.contains("iteration-1::") {
                fs::write(&broken, "Incomplete generated product")?;
            } else if broken.exists() {
                fs::remove_file(&broken)?;
            }
        }
        if child.key.starts_with("integration-") {
            fs::write(
                checkout.join("integration-guide.txt"),
                "Fixture integration instructions and verification\n",
            )?;
        }
    } else {
        ensure!(
            node.ends_with("refactor"),
            "Unexpected factory writer {node}"
        );
    }
    repository::save_worktree_candidate(store, run.clone()).await?;
    store
        .finish_agent_attempt(run.clone(), RunStatus::Succeeded, None)
        .await?;
    Ok(())
}

pub(super) async fn answer_child(
    store: &Store,
    project: &str,
    attempt: &AttemptPlan,
    child: &Child,
    node: &str,
    repair: u16,
) -> Result<Value> {
    if node.ends_with("specify-red-tests-agent") {
        let id = test_id(child, repair);
        return Ok(
            json!({"paths": [format!("tests/{id}.case")], "tests": [{"id":id,"reason":RED_REASON}]}),
        );
    }
    if node.ends_with("classify-merge-risk-agent") {
        return Ok(json!({"highRisk": child.key == "B"}));
    }
    ensure!(
        node.rsplit("::")
            .next()
            .is_some_and(|name| name.starts_with("review-")),
        "Unexpected factory reviewer {node}"
    );
    let source = store
        .workflow_attempt(project.into(), attempt.run.id.clone())
        .await?
        .context("The reviewer lost its pinned workflow attempt")?;
    let id = source
        .pin
        .unit_id
        .context("The reviewer lost its logical unit")?;
    let unit = store
        .workflow_delivery_units(
            project.into(),
            child.task_id.clone(),
            child.workflow_run_id.clone(),
        )
        .await?
        .into_iter()
        .find(|unit| unit.unit.id == id)
        .context("The reviewed unit is unavailable")?;
    let criteria: Vec<_> = unit
        .unit
        .completion_criteria
        .iter()
        .enumerate()
        .map(|(index, _)| json!({"index":index + 1,"status":"met"}))
        .collect();
    Ok(json!({"verdict":"approved","criteria":criteria,"findings":[]}))
}

pub(super) fn answer_audit(root: &Path, attempt: &AttemptPlan, node: &str) -> Result<Value> {
    let sha = git(root, &["rev-parse", DESTINATION])?;
    ensure!(
        attempt
            .input
            .iter()
            .any(|message| message.text.contains(&sha)),
        "The auditor did not receive the exact integrated SHA on {node}"
    );
    let phase = node.rsplit("::").next().unwrap_or(node);
    let has_guide = git(
        root,
        &["cat-file", "-e", &format!("{sha}:integration-guide.txt")],
    )
    .is_ok();
    Ok(match phase {
        "audit-requirements-agent" if !has_guide => json!({
            "approved":false,"summary":"The exact integration has no guide", "findings":[{
                "subject":"REQ-1", "problem":"The integrated revision has no integration-guide.txt",
                "revision":"Add the integration guide and a test for its presence."
            }]
        }),
        "audit-requirements-agent"
        | "audit-bdd-agent"
        | "audit-architecture-agent"
        | "audit-code-quality-agent"
        | "review-remediation-backlog-agent" => {
            json!({"approved":true,"summary":"Reviewed this exact integration and its pinned evidence","findings":[]})
        }
        "write-remediation-backlog-agent" => {
            ensure!(
                !has_guide,
                "Remediation was launched after the guide was already merged"
            );
            json!({"epicTitle":"Integration guide remediation","tasks":[{
                "key":format!("integration-{}-guide", &sha[..12]), "title":"Add the integration guide",
                "description":format!("Fix the missing integration guide on {sha}."), "priority":4, "dependsOn":[],
                "acceptanceCriteria":["The exact integration contains the guide"],
                "sddRequirements":["REQ-1"], "bddScenarios":["BDD-1"],
            }]})
        }
        "review-documentation-cleanup-agent" => {
            ensure!(
                has_guide,
                "Documentation review ran before integrating remediation"
            );
            json!({"documentationAndCleanup":"The integrated guide and product files are present.","needsChanges":false,"findings":[]})
        }
        _ => anyhow::bail!("Unexpected factory audit job {node}"),
    })
}
