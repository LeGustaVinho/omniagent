use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::protection;
use crate::workflows::{
    WorkflowRun,
    launch::{WorkflowLaunchOrigin, load_workflow_launch},
    runtime::load_workflow_run,
};

use super::{ApprovalDecision, ApprovalDecisionSource};

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
    BacklogAutomatic {
        scope: BacklogApprovalScope,
        source_checkout_id: String,
        maximum_units: u32,
        if_unmet: UnmetApproval,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BacklogApprovalScope {
    pub parent_run_id: String,
    pub node_id: String,
    pub child_revision: String,
}

impl ApprovalRuleSettings {
    fn automatic_terms(&self) -> Option<(u32, UnmetApproval)> {
        match self {
            Self::Automatic {
                maximum_units,
                if_unmet,
                ..
            }
            | Self::BacklogAutomatic {
                maximum_units,
                if_unmet,
                ..
            } => Some((*maximum_units, *if_unmet)),
            Self::Human => None,
        }
    }

    fn matches_scope(&self, facts: &ApprovalFacts) -> bool {
        match self {
            Self::Human => false,
            Self::Automatic {
                task_id,
                source_checkout_id,
                ..
            } => task_id == &facts.task_id && source_checkout_id == &facts.source_checkout_id,
            Self::BacklogAutomatic {
                scope,
                source_checkout_id,
                ..
            } => {
                Some(scope) == facts.backlog_scope.as_ref()
                    && source_checkout_id == &facts.source_checkout_id
            }
        }
    }
}

fn backlog_scope(
    db: &Connection,
    run: &WorkflowRun,
    work: &str,
) -> Result<Option<BacklogApprovalScope>> {
    let Some(launch) = load_workflow_launch(db, &run.project_id, &run.id)? else {
        return Ok(None);
    };
    match launch.origin {
        WorkflowLaunchOrigin::Backlog {
            parent_run_id,
            node_id,
            work_id,
        } if work_id == work => Ok(Some(BacklogApprovalScope {
            parent_run_id,
            node_id,
            child_revision: run.version.revision.clone(),
        })),
        _ => Ok(None),
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backlog_scope: Option<BacklogApprovalScope>,
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

fn outcome(settings: &ApprovalRuleSettings, facts: &ApprovalFacts) -> ApprovalRuleOutcome {
    let Some((maximum_units, _)) = settings.automatic_terms() else {
        return ApprovalRuleOutcome::HumanRequired;
    };
    if !settings.matches_scope(facts) {
        return ApprovalRuleOutcome::ScopeDoesNotMatch;
    }
    if facts.unit_count == 0 || facts.unit_count > maximum_units {
        return ApprovalRuleOutcome::UnitLimitExceeded;
    }
    ApprovalRuleOutcome::ConditionsMet
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
    if let ApprovalRuleSettings::BacklogAutomatic { scope, .. } = &settings {
        let Some(run) =
            load_workflow_run(db, &decision.task.project_id, &decision.workflow_run_id)?
        else {
            return Ok(false);
        };
        if backlog_scope(db, &run, &decision.task.task_id)?.as_ref() != Some(scope) {
            return Ok(false);
        }
    }
    Ok(revision == evidence.revision
        && decision.plan_revision == evidence.facts.plan_revision
        && decision.task_artifact_id == evidence.facts.task_artifact_id
        && decision.task.task_id == evidence.facts.task_id
        && decision.source_checkout_id == evidence.facts.source_checkout_id
        && outcome(&settings, &evidence.facts) == ApprovalRuleOutcome::ConditionsMet)
}
