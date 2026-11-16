use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    ApprovalDecision, ApprovalDecisionSource, active_approval, record_decision,
    require_current_plan, validate_linked_task,
};
use crate::workflows::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs, WorkflowArtifact,
    artifacts::load_artifact,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ApprovalRuleSettings {
    #[default]
    Human,
    Automatic {
        task_id: String,
        source_checkout_id: String,
        maximum_units: u32,
        if_unmet: UnmetApproval,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmetApproval {
    Wait,
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalFacts {
    pub plan_revision: String,
    pub task_artifact_id: String,
    pub task_id: String,
    pub source_checkout_id: String,
    pub unit_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRuleEvidence {
    pub revision: String,
    pub facts: ApprovalFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRuleOutcome {
    HumanRequired,
    ScopeDoesNotMatch,
    UnitLimitExceeded,
    AnotherDeliveryOwnsTask,
    ConditionsMet,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowApprovalPreview {
    pub revision: Option<String>,
    pub settings: ApprovalRuleSettings,
    pub facts: ApprovalFacts,
    pub outcome: ApprovalRuleOutcome,
    pub allows_human: bool,
    pub decision_artifact_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowApprovalSubmission {
    pub project_id: String,
    pub run_id: String,
    pub node_id: String,
    pub inputs: NodeInputs,
}

struct ApprovalContext {
    decision: ApprovalDecision,
    workflow_id: String,
    facts: ApprovalFacts,
    inputs: NodeInputs,
}

impl Store {
    async fn approval_context(
        &self,
        submission: WorkflowApprovalSubmission,
    ) -> Result<ApprovalContext> {
        let WorkflowApprovalSubmission {
            project_id,
            run_id,
            node_id,
            inputs,
        } = submission;
        let (node, resolved) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, None, &inputs)
            .await?;
        ensure!(
            node.definition.kind == BuiltinNode::ApproveImplementation,
            "the stage does not approve implementation"
        );
        let ArtifactValue::Task(task) = &resolved["task"].value else {
            anyhow::bail!("approval requires linked work");
        };
        let plan = &resolved["plan"];
        let ArtifactValue::Plan {
            run_id: attempt, ..
        } = &plan.value
        else {
            anyhow::bail!("approval requires a plan");
        };
        let unit_count = self
            .workflow_plan_units(project_id.clone(), run_id.clone(), plan.id.clone())
            .await?
            .len()
            .try_into()?;
        let source_checkout_id = self.workflow_plan_checkout(&project_id, attempt).await?;
        let workflow_id = self
            .workflow_run(project_id.clone(), run_id.clone())
            .await?
            .context("the approval workflow is unavailable")?
            .version
            .id;
        let facts = ApprovalFacts {
            plan_revision: plan.id.clone(),
            task_artifact_id: resolved["task"].id.clone(),
            task_id: task.task_id.clone(),
            source_checkout_id: source_checkout_id.clone(),
            unit_count,
        };
        Ok(ApprovalContext {
            facts,
            workflow_id,
            inputs,
            decision: ApprovalDecision {
                workflow_run_id: run_id,
                node_instance_id: node_id,
                plan_revision: plan.id.clone(),
                task: task.clone(),
                task_artifact_id: resolved["task"].id.clone(),
                source_checkout_id,
                source: ApprovalDecisionSource::Operator,
                rule_evidence: None,
            },
        })
    }

    pub async fn workflow_approval_preview(
        &self,
        submission: WorkflowApprovalSubmission,
    ) -> Result<WorkflowApprovalPreview> {
        let context = self.approval_context(submission).await?;
        self.execute(move |db| inspect(db, &context)).await
    }

    pub async fn set_workflow_approval_rule(
        &self,
        submission: WorkflowApprovalSubmission,
        expected_revision: Option<String>,
        settings: ApprovalRuleSettings,
    ) -> Result<WorkflowApprovalPreview> {
        let context = self.approval_context(submission).await?;
        if let ApprovalRuleSettings::Automatic {
            task_id,
            source_checkout_id,
            maximum_units,
            ..
        } = &settings
        {
            ensure!(
                *maximum_units > 0,
                "an automatic release needs a positive unit limit"
            );
            ensure!(
                task_id == &context.facts.task_id
                    && source_checkout_id == &context.facts.source_checkout_id,
                "the rule scope must match the linked task and the planning checkout"
            );
        }
        let preview = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let old = inspect(&tx, &context)?;
            ensure!(old.revision == expected_revision, "the automatic approval rule changed; refresh its preview");
            if settings != old.settings {
                let project = &context.decision.task.project_id;
                let workflow = &context.workflow_id;
                let node = &context.decision.node_instance_id;
                let revision = Uuid::new_v4().to_string();
                let payload = protection::protect(&serde_json::to_vec(&settings)?)?;
                tx.execute(
                    "INSERT INTO workflow_approval_rule_revisions (revision, project_id, workflow_id, node_instance_id, settings)
                     VALUES (?1, ?2, ?3, ?4, ?5)", params![revision, project, workflow, node, payload],
                )?;
                tx.execute(
                    "INSERT INTO workflow_approval_rule_heads VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(project_id, workflow_id, node_instance_id) DO UPDATE SET revision = excluded.revision",
                    params![project, workflow, node, revision],
                )?;
                if let Some(stale) = old.revision {
                    tx.execute(
                        "UPDATE job_pipeline_releases SET revoked = 1 WHERE project_id = ?1 AND id IN (
                            SELECT b.release_id FROM workflow_release_bindings b
                            JOIN workflow_approval_decisions d ON d.decision_artifact_id = b.decision_artifact_id
                            WHERE d.rule_revision = ?2
                         )", params![project, stale],
                    )?;
                }
            }
            let preview = inspect(&tx, &context)?;
            let preview = apply_if_automatic(&tx, &context, preview)?;
            tx.commit()?;
            Ok(preview)
        }).await?;
        self.job_signal.send_replace(());
        Ok(preview)
    }

    pub async fn evaluate_workflow_approval(
        &self,
        submission: WorkflowApprovalSubmission,
        expected_revision: String,
    ) -> Result<WorkflowApprovalPreview> {
        self.evaluate_current_approval(submission, Some(expected_revision))
            .await
    }

    async fn evaluate_current_approval(
        &self,
        submission: WorkflowApprovalSubmission,
        expected_revision: Option<String>,
    ) -> Result<WorkflowApprovalPreview> {
        let context = self.approval_context(submission).await?;
        let preview = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let preview = inspect(&tx, &context)?;
                if let Some(expected_revision) = expected_revision {
                    ensure!(
                        preview.revision.as_ref() == Some(&expected_revision),
                        "the automatic approval rule changed; refresh its preview"
                    );
                }
                let preview = apply_if_automatic(&tx, &context, preview)?;
                tx.commit()?;
                Ok(preview)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(preview)
    }

    pub(in crate::workflows) async fn approve_workflow_implementation(
        &self,
        submission: WorkflowApprovalSubmission,
    ) -> Result<WorkflowArtifact> {
        let context = self.approval_context(submission).await?;
        let completed = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let preview = inspect(&tx, &context)?;
                if let Some(id) = preview.decision_artifact_id {
                    return load_artifact(
                        &tx,
                        &context.decision.task.project_id,
                        &context.decision.workflow_run_id,
                        &id,
                        ArtifactKind::Approval,
                    )?
                    .context("the active approval is unavailable");
                }
                ensure!(
                    preview.allows_human,
                    "this stage is waiting for automatic approval conditions"
                );
                let rule_evidence = preview.revision.map(|revision| ApprovalRuleEvidence {
                    revision,
                    facts: preview.facts,
                });
                let artifact = persist(
                    &tx,
                    &context,
                    ApprovalDecisionSource::Operator,
                    rule_evidence,
                )?;
                tx.commit()?;
                Ok(artifact)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(completed)
    }

    pub(in crate::workflows) async fn approve_after_link(
        &self,
        project: &str,
        task: &WorkflowArtifact,
        plan_id: &str,
    ) -> Result<()> {
        let run = &task.workflow_run_id;
        let workflow = self
            .workflow_run(project.into(), run.clone())
            .await?
            .context("the approval workflow is unavailable")?;
        if workflow.plan_revision.as_deref() != Some(plan_id) {
            return Ok(());
        }
        let definition = self
            .workflow_definition(project.into(), workflow.version)
            .await?
            .context("the approval definition is unavailable")?;
        for node in definition
            .nodes
            .iter()
            .filter(|node| node.instance.kind == BuiltinNode::ApproveImplementation)
        {
            let node_id = &node.instance.id;
            if !definition.wires.iter().any(|wire| {
                wire.from.node == task.node_instance_id
                    && wire.from.port == task.port
                    && &wire.to.node == node_id
                    && wire.to.port == "task"
            }) {
                continue;
            }
            let saved = self
                .execute({
                    let (project, workflow, node) = (
                        project.to_owned(),
                        definition.version.id.clone(),
                        node_id.clone(),
                    );
                    move |db| load_rule(db, &project, &workflow, &node)
                })
                .await?;
            if let Some((_, ApprovalRuleSettings::Automatic { .. })) = saved {
                self.evaluate_current_approval(
                    WorkflowApprovalSubmission {
                        project_id: project.into(),
                        run_id: run.clone(),
                        node_id: node_id.clone(),
                        inputs: NodeInputs::from([
                            ("plan".into(), plan_id.into()),
                            ("task".into(), task.id.clone()),
                        ]),
                    },
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn inspect(db: &Connection, context: &ApprovalContext) -> Result<WorkflowApprovalPreview> {
    let decision = &context.decision;
    require_current_plan(
        db,
        &decision.task.project_id,
        &decision.workflow_run_id,
        &decision.plan_revision,
    )?;
    validate_linked_task(db, decision)?;
    let rule = load_rule(
        db,
        &decision.task.project_id,
        &context.workflow_id,
        &decision.node_instance_id,
    )?;
    let (revision, settings) = rule.map_or_else(
        || (None, ApprovalRuleSettings::default()),
        |(revision, settings)| (Some(revision), settings),
    );
    let active = active_approval(db, decision)?;
    let mut outcome = outcome(&settings, &context.facts);
    if outcome == ApprovalRuleOutcome::ConditionsMet && active.is_none() {
        let other_active: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_delivery_heads h
             JOIN job_pipeline_runs r ON r.id = h.pipeline_run_id AND r.plan_revision = h.plan_revision
             JOIN workflow_approval_decisions d ON d.decision_artifact_id = h.decision_artifact_id
             WHERE h.project_id = ?1 AND h.task_id = ?2
               AND (h.pipeline_run_id != ?3 OR d.node_instance_id != ?4))",
            params![decision.task.project_id, decision.task.task_id, decision.workflow_run_id, decision.node_instance_id],
            |row| row.get(0),
        )?;
        if other_active {
            outcome = ApprovalRuleOutcome::AnotherDeliveryOwnsTask;
        }
    }
    let allows_human = outcome == ApprovalRuleOutcome::HumanRequired
        || (outcome != ApprovalRuleOutcome::ConditionsMet
            && matches!(
                settings,
                ApprovalRuleSettings::Automatic {
                    if_unmet: UnmetApproval::Human,
                    ..
                }
            ));
    Ok(WorkflowApprovalPreview {
        revision,
        settings,
        outcome,
        allows_human,
        facts: context.facts.clone(),
        decision_artifact_id: active,
    })
}

fn outcome(settings: &ApprovalRuleSettings, facts: &ApprovalFacts) -> ApprovalRuleOutcome {
    let ApprovalRuleSettings::Automatic {
        task_id,
        source_checkout_id,
        maximum_units,
        ..
    } = settings
    else {
        return ApprovalRuleOutcome::HumanRequired;
    };
    if task_id != &facts.task_id || source_checkout_id != &facts.source_checkout_id {
        return ApprovalRuleOutcome::ScopeDoesNotMatch;
    }
    if facts.unit_count == 0 || facts.unit_count > *maximum_units {
        return ApprovalRuleOutcome::UnitLimitExceeded;
    }
    ApprovalRuleOutcome::ConditionsMet
}

fn apply_if_automatic(
    db: &Connection,
    context: &ApprovalContext,
    mut preview: WorkflowApprovalPreview,
) -> Result<WorkflowApprovalPreview> {
    if preview.decision_artifact_id.is_none()
        && preview.outcome == ApprovalRuleOutcome::ConditionsMet
    {
        let evidence = ApprovalRuleEvidence {
            revision: preview
                .revision
                .clone()
                .context("automatic approval has no rule revision")?,
            facts: context.facts.clone(),
        };
        preview.decision_artifact_id = Some(
            persist(
                db,
                context,
                ApprovalDecisionSource::AutomaticRule,
                Some(evidence),
            )?
            .id,
        );
    }
    Ok(preview)
}

fn persist(
    db: &Connection,
    context: &ApprovalContext,
    source: ApprovalDecisionSource,
    rule_evidence: Option<ApprovalRuleEvidence>,
) -> Result<WorkflowArtifact> {
    let mut decision = context.decision.clone();
    decision.source = source;
    decision.rule_evidence = rule_evidence;
    let artifact = record_decision(db, decision.clone())?;
    let request_revision =
        crate::workflows::digest(&serde_json::to_vec(&(&context.inputs, &decision))?);
    db.execute(
        "INSERT INTO workflow_host_completions VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(pipeline_run_id, node_instance_id, execution_key) DO UPDATE SET
            request_revision = excluded.request_revision, artifact_id = excluded.artifact_id",
        params![
            decision.workflow_run_id,
            decision.node_instance_id,
            decision.plan_revision,
            request_revision,
            artifact.id
        ],
    )?;
    super::super::inspection::record_workflow_input_ids(
        db,
        &artifact,
        &decision.plan_revision,
        &context.inputs,
    )?;
    Ok(artifact)
}

fn load_rule(
    db: &Connection,
    project: &str,
    workflow: &str,
    node: &str,
) -> Result<Option<(String, ApprovalRuleSettings)>> {
    let saved: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT r.revision, r.settings FROM workflow_approval_rule_heads h
         JOIN workflow_approval_rule_revisions r ON r.revision = h.revision
         WHERE h.project_id = ?1 AND h.workflow_id = ?2 AND h.node_instance_id = ?3",
            params![project, workflow, node],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    saved
        .map(|(revision, settings)| {
            Ok((
                revision,
                serde_json::from_slice(&protection::unprotect(&settings)?)?,
            ))
        })
        .transpose()
}

pub(super) fn rule_authorizes(db: &Connection, decision: &ApprovalDecision) -> Result<bool> {
    if decision.source == ApprovalDecisionSource::Operator {
        return Ok(true);
    }
    let Some(evidence) = decision.rule_evidence.as_ref() else {
        return Ok(false);
    };
    let workflow: String = db.query_row(
        "SELECT workflow_id FROM workflow_run_pins WHERE pipeline_run_id = ?1 AND project_id = ?2",
        params![decision.workflow_run_id, decision.task.project_id],
        |row| row.get(0),
    )?;
    let Some((revision, settings)) = load_rule(
        db,
        &decision.task.project_id,
        &workflow,
        &decision.node_instance_id,
    )?
    else {
        return Ok(false);
    };
    Ok(revision == evidence.revision
        && decision.plan_revision == evidence.facts.plan_revision
        && decision.task_artifact_id == evidence.facts.task_artifact_id
        && decision.task.task_id == evidence.facts.task_id
        && decision.source_checkout_id == evidence.facts.source_checkout_id
        && outcome(&settings, &evidence.facts) == ApprovalRuleOutcome::ConditionsMet)
}
