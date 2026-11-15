use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningMode {
    CodexCollaboration,
    ClaudePermissionMode,
    AcpSessionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    CodexPlanItem,
    ClaudeExitPlanMode,
    AntigravityPlanCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub content: String,
    pub priority: PlanPriority,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

pub(crate) const PROPOSAL_FORMAT: &str = "When the plan is ready for review, enclose the full Markdown plan in <proposed_plan> and </proposed_plan>. Leave out these markers when asking for more information or if a plan is unavailable.";

pub(crate) fn marked_proposal(text: &str) -> Option<String> {
    let (_, remainder) = text.rsplit_once("<proposed_plan>")?;
    let (proposal, _) = remainder.split_once("</proposed_plan>")?;
    let proposal = proposal.trim();
    (!proposal.is_empty()).then(|| proposal.to_owned())
}
