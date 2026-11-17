use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    diagnostics, protection,
    repository::{
        verification::{
            SelectedCandidate, require_current,
            test_phase::{
                TestCheckResult, TestExecution, TestPhase, TestSpecification, TestVerdict,
                freeze_test_candidate, load_test_gate,
            },
        },
        verified_git::load_verified_delivery_commit,
    },
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, NodeConfiguration, NodeInputs, PinnedNode, WorkflowArtifact,
    WorkflowRun,
    approval::workflow_release_authorized,
    artifacts::{insert_scoped_artifact, load_artifact},
    control::{refresh, require_stage},
    digest,
    inspection::record_workflow_input_ids,
    runtime::{load_pinned_node, load_workflow_run},
    units::{load_delivery_unit, load_delivery_units},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPhaseEvidence {
    pub phase: TestPhase,
    pub base_commit: String,
    pub release_id: String,
    pub check: String,
    pub policy_revision: String,
    pub candidate_id: Option<String>,
    pub prior_artifact_id: Option<String>,
    pub specification: Option<TestSpecification>,
    pub result: TestCheckResult,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPhaseWait {
    node_id: String,
    unit_id: String,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<TestPhaseWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, unit_id, reason FROM workflow_test_phase_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND output_id IS NULL
         ORDER BY node_instance_id, unit_id",
    )?;
    let waits = query
        .query_map(params![run.id, run.plan_revision], |row| {
            Ok(TestPhaseWait {
                node_id: row.get(0)?,
                unit_id: row.get(1)?,
                reason: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(waits)
}

pub(super) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<&'static str>> {
    let unit = unit.context("Test phases need an approved logical unit")?;
    let plan = run
        .plan_revision
        .as_deref()
        .context("Test phases need an approved plan")?;
    let automatic: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [&run.id],
        |row| row.get(0),
    )?;
    if !automatic {
        return Ok(Some("blocked"));
    }
    let key = digest(&serde_json::to_vec(&(&node.revision, inputs))?);
    let recorded: Option<Option<String>> = db.query_row(
        "SELECT output_id FROM workflow_test_phase_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_id = ?3 AND plan_key = ?4 AND request_key = ?5",
        params![run.id, node.instance.id, unit, plan, key], |row| row.get(0),
    ).optional()?;
    match recorded {
        Some(Some(id)) => {
            let artifact = load_artifact(
                db,
                &run.project_id,
                &run.id,
                &id,
                ArtifactKind::TestPhaseEvidence,
            )?
            .context("The recorded test phase evidence is unavailable")?;
            let ArtifactValue::TestPhaseEvidence(evidence) = artifact.value else {
                unreachable!()
            };
            Ok(Some(evidence.result.verdict.port()))
        }
        Some(None) => Ok(None),
        None => {
            let id = inputs
                .get("worktree")
                .context("The test phase lost its selected worktree")?;
            let worktree = load_artifact(db, &run.project_id, &run.id, id, ArtifactKind::Worktree)?
                .context("The test worktree artifact is unavailable")?;
            let ArtifactValue::Worktree { task, .. } = worktree.value else {
                unreachable!()
            };
            let units = load_delivery_units(db, &run.project_id, &task.task_id, &run.id)?;
            let current = units
                .iter()
                .find(|item| item.unit.id == unit)
                .context("This test phase is outside the released delivery")?;
            if current.commit_evidence_id.is_some()
                || units.iter().any(|item| {
                    item.unit.ordinal < current.unit.ordinal && item.commit_evidence_id.is_none()
                })
            {
                return Ok(None);
            }
            db.execute(
                "INSERT INTO workflow_test_phase_actions
                 (pipeline_run_id, node_instance_id, unit_id, plan_key, request_key, inputs) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![run.id, node.instance.id, unit, plan, key, protection::protect(&serde_json::to_vec(inputs)?)?],
            )?;
            Ok(None)
        }
    }
}

#[derive(Clone)]
pub(crate) struct PendingTestPhase {
    project: String,
    run: String,
    node: String,
    unit: String,
    plan: String,
    key: String,
    inputs: NodeInputs,
}

impl Store {
    pub(crate) async fn next_workflow_test_phase(&self) -> Result<Option<PendingTestPhase>> {
        self.execute(|db| {
            let pending = db.query_row(
                "SELECT r.project_id, a.pipeline_run_id, a.node_instance_id, a.unit_id, a.plan_key, a.request_key, a.inputs
                 FROM workflow_test_phase_actions a JOIN job_pipeline_runs r ON r.id = a.pipeline_run_id AND r.plan_revision = a.plan_key
                 WHERE a.output_id IS NULL AND a.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id, a.unit_id LIMIT 1",
                [], |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, Vec<u8>>(6)?,
                )),
            ).optional()?;
            pending.map(|(project, run, node, unit, plan, key, bytes)| Ok(PendingTestPhase {
                project, run, node, unit, plan, key,
                inputs: serde_json::from_slice(&protection::unprotect(&bytes)?)?,
            })).transpose()
        }).await
    }
}

struct PreparedPhase {
    evidence: TestPhaseEvidence,
    candidate: Option<SelectedCandidate>,
    execute: Option<TestExecution>,
}

impl PendingTestPhase {
    fn input(&self, db: &Connection, name: &str, kind: ArtifactKind) -> Result<WorkflowArtifact> {
        let id = self
            .inputs
            .get(name)
            .with_context(|| format!("Test phase requires input {name}"))?;
        load_artifact(db, &self.project, &self.run, id, kind)?
            .with_context(|| format!("The selected {name} artifact is unavailable"))
    }

    fn validate(&self, db: &Connection) -> Result<PinnedNode> {
        let run = load_workflow_run(db, &self.project, &self.run)?
            .context("The test workflow is unavailable")?;
        ensure!(
            run.plan_revision.as_ref() == Some(&self.plan),
            "The test phase plan was superseded"
        );
        require_stage(
            db,
            &self.project,
            &self.run,
            &self.node,
            Some(&self.unit),
            &self.inputs,
        )?;
        let node = load_pinned_node(db, &self.project, &self.run, &self.node)?
            .context("The test stage is unavailable")?;
        ensure!(
            digest(&serde_json::to_vec(&(&node.revision, &self.inputs))?) == self.key,
            "Stored test phase inputs differ from their selected pins"
        );
        Ok(node)
    }

    fn prepare(&self, db: &Connection) -> Result<PreparedPhase> {
        let node = self.validate(db)?;
        let NodeConfiguration::VerifyTestPhase(config) = node.instance.configuration else {
            anyhow::bail!("The pinned node does not verify a test phase");
        };
        let ArtifactValue::Release(release) =
            self.input(db, "release", ArtifactKind::Release)?.value
        else {
            unreachable!()
        };
        let ArtifactValue::Worktree { checkout_id, task } =
            self.input(db, "worktree", ArtifactKind::Worktree)?.value
        else {
            unreachable!()
        };
        ensure!(
            task.project_id == self.project
                && release.workflow_run_id == self.run
                && release.release.plan_revision == self.plan
                && release.release.checkout_id == checkout_id
                && release.release.task_id == task.task_id
                && workflow_release_authorized(db, &self.project, &self.run, &release.release.id)?
                    == Some(true),
            "The test phase requires the active release for its managed task and checkout"
        );
        let unit = load_delivery_unit(db, &self.project, &task.task_id, &self.unit)?
            .context("The test phase is outside the active logical unit")?;
        ensure!(
            unit.workflow_run_id == self.run
                && unit.plan_revision == self.plan
                && unit.checkout_id == checkout_id
                && unit.commit_evidence_id.is_none(),
            "The test phase belongs to a different delivery unit or its commit is already verified"
        );
        let (mut base, common): (String, String) = db
            .query_row(
                "SELECT m.base_commit, p.git_common_dir FROM managed_task_worktrees m
             JOIN projects p ON p.id = m.project_id WHERE m.project_id = ?1 AND m.checkout_id = ?2",
                params![self.project, checkout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("The prepared worktree no longer exists")?;
        if unit.unit.ordinal == 1
            && let Some(plan) =
                load_artifact(db, &self.project, &self.run, &self.plan, ArtifactKind::Plan)?
            && let ArtifactValue::BacklogPlan(plan) = plan.value
            && let Some(repair) = plan.repair
        {
            base = repair.previous_source_sha;
        }
        if unit.unit.ordinal > 1 {
            let units = load_delivery_units(db, &self.project, &task.task_id, &self.run)?;
            let previous = units
                .iter()
                .find(|item| item.unit.ordinal + 1 == unit.unit.ordinal)
                .and_then(|item| item.commit_evidence_id.as_ref())
                .context("The preceding unit has no verified commit")?;
            base = load_verified_delivery_commit(db, &self.project, &task.task_id, previous)?
                .context("The preceding unit lost its verified commit")?
                .sha;
        }

        let gate = load_test_gate(db, &self.project, &config.check)?;
        let mut evidence = TestPhaseEvidence {
            phase: config.phase,
            base_commit: base.clone(),
            release_id: release.release.id,
            check: config.check.clone(),
            policy_revision: gate.policy_revision.clone(),
            candidate_id: None,
            prior_artifact_id: None,
            specification: None,
            result: TestCheckResult::rejected(TestVerdict::Blocked, "The test phase has not run"),
        };
        let expected_patch = if config.phase == TestPhase::Baseline {
            None
        } else {
            let prior = self.input(db, "prior", ArtifactKind::TestPhaseEvidence)?;
            let ArtifactValue::TestPhaseEvidence(previous) = prior.value else {
                unreachable!()
            };
            let needed = if config.phase == TestPhase::Red {
                TestPhase::Baseline
            } else {
                TestPhase::Red
            };
            evidence.prior_artifact_id = Some(prior.id);
            if prior.unit_id.as_deref() != Some(&self.unit)
                || previous.phase != needed
                || previous.result.verdict != TestVerdict::Verified
                || previous.base_commit != base
                || previous.release_id != evidence.release_id
                || previous.check != config.check
                || previous.policy_revision != gate.policy_revision
            {
                evidence.result = TestCheckResult::rejected(
                    TestVerdict::Blocked,
                    "The previous test phase must be verified on the same base, release and registered check",
                );
                return Ok(PreparedPhase {
                    evidence,
                    candidate: None,
                    execute: None,
                });
            }
            if config.phase == TestPhase::Green {
                evidence.specification = Some(
                    previous
                        .specification
                        .context("The red evidence lost its declared tests")?,
                );
                Some(
                    previous
                        .result
                        .test_patch_revision
                        .context("The red evidence lost its test patch revision")?,
                )
            } else {
                None
            }
        };
        if config.phase == TestPhase::Red {
            let ArtifactValue::JsonObject(specification) =
                self.input(db, "tests", ArtifactKind::JsonObject)?.value
            else {
                unreachable!()
            };
            let specification =
                serde_json::from_value::<TestSpecification>(Value::Object(specification))
                    .context(
                        "The red test specification needs paths and expected test IDs with reasons",
                    )
                    .and_then(|specification| {
                        specification.validate(&config)?;
                        Ok(specification)
                    });
            match specification {
                Ok(specification) => evidence.specification = Some(specification),
                Err(error) => {
                    evidence.result =
                        TestCheckResult::rejected(TestVerdict::Blocked, error.to_string());
                    return Ok(PreparedPhase {
                        evidence,
                        candidate: None,
                        execute: None,
                    });
                }
            }
        }
        let candidate = if config.phase == TestPhase::Baseline {
            None
        } else {
            let ArtifactValue::Candidate(candidate) =
                self.input(db, "candidate", ArtifactKind::Candidate)?.value
            else {
                unreachable!()
            };
            ensure!(
                candidate.target.project_id == self.project
                    && candidate.target.task_id == task.task_id
                    && candidate.target.unit_id == self.unit
                    && candidate.checkout_id == checkout_id,
                "The test snapshot belongs to a different task, unit or checkout"
            );
            evidence.candidate_id = Some(candidate.id.clone());
            if candidate.base_commit != base {
                evidence.result = TestCheckResult::rejected(
                    TestVerdict::Blocked,
                    "The candidate does not use the verified test base",
                );
                return Ok(PreparedPhase {
                    evidence,
                    candidate: None,
                    execute: None,
                });
            }
            Some(freeze_test_candidate(db, &candidate)?)
        };
        Ok(PreparedPhase {
            candidate: candidate
                .as_ref()
                .map(|candidate| candidate.selection.clone()),
            execute: Some(TestExecution {
                gate,
                phase: config.phase,
                common_dir: PathBuf::from(common),
                base,
                specification: evidence.specification.clone(),
                candidate,
                expected_patch,
            }),
            evidence,
        })
    }

    pub(crate) async fn execute(&self, store: &Store) -> Result<()> {
        let action = self.clone();
        let mut prepared = store.execute(move |db| action.prepare(db)).await?;
        if let Some(execute) = prepared.execute.take() {
            prepared.evidence.result = tokio::task::spawn_blocking(move || execute.run()).await?;
        }
        let action = self.clone();
        store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            action.validate(&tx)?;
            ensure!(
                workflow_release_authorized(&tx, &action.project, &action.run, &prepared.evidence.release_id)? == Some(true),
                "The implementation release changed while running tests"
            );
            if let Some(candidate) = &prepared.candidate {
                require_current(&tx, candidate)?;
            }
            let current = load_test_gate(&tx, &action.project, &prepared.evidence.check)?;
            ensure!(current.policy_revision == prepared.evidence.policy_revision, "The test check policy changed during execution");
            let output = insert_scoped_artifact(&tx, &action.project, action.run.clone(),
                action.node.clone(), "evidence".into(), Some(action.unit.clone()),
                ArtifactValue::TestPhaseEvidence(prepared.evidence))?;
            record_workflow_input_ids(&tx, &output, &action.plan, &action.inputs)?;
            let count = tx.execute(
                "UPDATE workflow_test_phase_actions SET output_id = ?6, reason = NULL WHERE pipeline_run_id = ?1
                   AND node_instance_id = ?2 AND unit_id = ?3 AND plan_key = ?4 AND request_key = ?5 AND output_id IS NULL",
                params![action.run, action.node, action.unit, action.plan, action.key, output.id],
            )?;
            ensure!(count == 1, "The test phase already has a recorded outcome");
            refresh(&tx, &action.project, &action.run)?;
            tx.commit()?;
            Ok(())
        }).await
    }

    pub(crate) async fn defer(&self, store: &Store, error: &anyhow::Error) -> Result<()> {
        let reason = diagnostics::preview(&error.to_string());
        let action = self.clone();
        store.execute(move |db| {
            db.execute(
                "UPDATE workflow_test_phase_actions SET reason = ?6, retry_after = unixepoch() + 30
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_id = ?3 AND plan_key = ?4
                   AND request_key = ?5 AND output_id IS NULL",
                params![action.run, action.node, action.unit, action.plan, action.key, reason],
            )?;
            Ok(())
        }).await
    }
}
