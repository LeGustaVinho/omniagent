use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{protection, store::Store};

use super::{
    ApprovalDecision, ApprovalDecisionSource, active_approval, load_plan_checkout, record_decision,
    require_current_plan, validate_linked_task,
};
use crate::workflows::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeInputs, PinnedNode, WorkflowArtifact,
    WorkflowRun,
    artifacts::{load_artifact, load_plan_text},
    runtime::load_workflow_run,
    units::parse_plan_units,
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

#[derive(Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkflowApprovalStage {
    Waiting {
        reason: String,
    },
    Skipped {
        reason: String,
    },
    Ready {
        plan_id: String,
        task_artifact_id: String,
        source: Option<ApprovalDecisionSource>,
        preview: Box<WorkflowApprovalPreview>,
    },
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

impl ApprovalContext {
    fn from_inputs(
        db: &Connection,
        run: &WorkflowRun,
        node_id: &str,
        plan: &WorkflowArtifact,
        task: &WorkflowArtifact,
    ) -> Result<Self> {
        let ArtifactValue::Task(task_ref) = &task.value else {
            anyhow::bail!("approval requires linked work");
        };
        let ArtifactValue::Plan {
            run_id: attempt,
            event_sequence,
        } = &plan.value
        else {
            anyhow::bail!("approval requires a plan");
        };
        ensure!(
            task_ref.project_id == run.project_id
                && task.workflow_run_id == run.id
                && plan.workflow_run_id == run.id,
            "approval evidence is outside this workflow"
        );
        let text = load_plan_text(db, attempt, *event_sequence)?;
        let unit_count = parse_plan_units(&plan.id, &text)?.len().try_into()?;
        let source_checkout_id = load_plan_checkout(db, &run.project_id, attempt)?;
        let facts = ApprovalFacts {
            plan_revision: plan.id.clone(),
            task_artifact_id: task.id.clone(),
            task_id: task_ref.task_id.clone(),
            source_checkout_id: source_checkout_id.clone(),
            unit_count,
        };
        Ok(Self {
            facts,
            workflow_id: run.version.id.clone(),
            inputs: NodeInputs::from([
                ("plan".into(), plan.id.clone()),
                ("task".into(), task.id.clone()),
            ]),
            decision: ApprovalDecision {
                workflow_run_id: run.id.clone(),
                node_instance_id: node_id.into(),
                plan_revision: plan.id.clone(),
                task: task_ref.clone(),
                task_artifact_id: task.id.clone(),
                source_checkout_id,
                source: ApprovalDecisionSource::Operator,
                rule_evidence: None,
            },
        })
    }
}

impl Store {
    pub async fn workflow_approval_stage(
        &self,
        project: String,
        run_id: String,
        node_id: String,
    ) -> Result<WorkflowApprovalStage> {
        use super::super::control::{ApprovalSelection, approval_selection, refresh};
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let run = load_workflow_run(&tx, &project, &run_id)?
                .context("the approval workflow is unavailable")?;
            refresh(&tx, &project, &run_id)?;
            let stage = match approval_selection(&tx, &run, &node_id)? {
                ApprovalSelection::Waiting { reason } => WorkflowApprovalStage::Waiting { reason },
                ApprovalSelection::Skipped { reason } => WorkflowApprovalStage::Skipped { reason },
                ApprovalSelection::Ready { plan, task } => {
                    approval_stage_ready(&tx, &run, &node_id, &plan, &task)?
                }
            };
            tx.commit()?;
            Ok(stage)
        })
        .await
    }

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
        self.execute(move |db| {
            let run = load_workflow_run(db, &project_id, &run_id)?
                .context("the approval workflow is unavailable")?;
            ApprovalContext::from_inputs(db, &run, &node_id, &resolved["plan"], &resolved["task"])
        })
        .await
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
            let preview = apply_and_refresh(&tx, &context, preview)?;
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
        let context = self.approval_context(submission).await?;
        let preview = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let preview = inspect(&tx, &context)?;
                ensure!(
                    preview.revision.as_ref() == Some(&expected_revision),
                    "the automatic approval rule changed; refresh its preview"
                );
                let preview = apply_and_refresh(&tx, &context, preview)?;
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
                super::super::control::refresh(
                    &tx,
                    &context.decision.task.project_id,
                    &context.decision.workflow_run_id,
                )?;
                tx.commit()?;
                Ok(artifact)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(completed)
    }
}

fn approval_stage_ready(
    db: &Connection,
    run: &WorkflowRun,
    node_id: &str,
    plan: &WorkflowArtifact,
    task: &WorkflowArtifact,
) -> Result<WorkflowApprovalStage> {
    let preview = ApprovalContext::from_inputs(db, run, node_id, plan, task)
        .and_then(|context| inspect(db, &context));
    let preview = match preview {
        Ok(preview) => preview,
        Err(error) => {
            return Ok(WorkflowApprovalStage::Waiting {
                reason: crate::diagnostics::preview(&error.to_string()),
            });
        }
    };
    let source = if let Some(id) = &preview.decision_artifact_id {
        let artifact = load_artifact(db, &run.project_id, &run.id, id, ArtifactKind::Approval)?
            .context("the current approval decision is unavailable")?;
        let ArtifactValue::Approval(decision) = artifact.value else {
            anyhow::bail!("the current approval decision has the wrong type");
        };
        Some(decision.source)
    } else {
        None
    };
    Ok(WorkflowApprovalStage::Ready {
        plan_id: plan.id.clone(),
        task_artifact_id: task.id.clone(),
        source,
        preview: Box::new(preview),
    })
}

fn inspect(db: &Connection, context: &ApprovalContext) -> Result<WorkflowApprovalPreview> {
    let decision = &context.decision;
    super::super::control::require_stage(
        db,
        &decision.task.project_id,
        &decision.workflow_run_id,
        &decision.node_instance_id,
        None,
        &context.inputs,
    )?;
    let rule = load_rule(
        db,
        &decision.task.project_id,
        &context.workflow_id,
        &decision.node_instance_id,
    )?;
    inspect_selected(db, context, rule)
}

fn inspect_selected(
    db: &Connection,
    context: &ApprovalContext,
    rule: Option<(String, ApprovalRuleSettings)>,
) -> Result<WorkflowApprovalPreview> {
    let decision = &context.decision;
    require_current_plan(
        db,
        &decision.task.project_id,
        &decision.workflow_run_id,
        &decision.plan_revision,
    )?;
    validate_linked_task(db, decision)?;
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

pub(in crate::workflows) fn activate_selected_approval(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    plan: &WorkflowArtifact,
    task: &WorkflowArtifact,
) -> Result<bool> {
    let node_id = &node.instance.id;
    let rule = load_rule(db, &run.project_id, &run.version.id, node_id)?;
    if !matches!(&rule, Some((_, ApprovalRuleSettings::Automatic { .. })))
        || !super::super::registry::supports_pinned_execution(&node.definition)
    {
        return Ok(false);
    }
    let Ok(context) = ApprovalContext::from_inputs(db, run, node_id, plan, task) else {
        // Missing plan evidence leaves this approval waiting. Its ordinary preview explains the error.
        return Ok(false);
    };
    let preview = inspect_selected(db, &context, rule)?;
    if preview.decision_artifact_id.is_some()
        || preview.outcome != ApprovalRuleOutcome::ConditionsMet
        || super::super::authoring::ensure_package_runnable(db, &run.project_id, &run.version)
            .is_err()
        || crate::tasks::validate_repository_delivery_open(
            db,
            &run.project_id,
            &context.facts.task_id,
        )
        .is_err()
    {
        return Ok(false);
    }
    Ok(apply_if_automatic(db, &context, preview)?
        .decision_artifact_id
        .is_some())
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

fn apply_and_refresh(
    db: &Connection,
    context: &ApprovalContext,
    preview: WorkflowApprovalPreview,
) -> Result<WorkflowApprovalPreview> {
    let pending = preview.decision_artifact_id.is_none();
    let preview = apply_if_automatic(db, context, preview)?;
    if pending && preview.decision_artifact_id.is_some() {
        super::super::control::refresh(
            db,
            &context.decision.task.project_id,
            &context.decision.workflow_run_id,
        )?;
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
