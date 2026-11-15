use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    StructuredTurn,
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

pub(crate) const STRUCTURED_PLANNING_PROMPT: &str = r#"Read-only planning response format. Do not edit files or call a native question tool. Return a single JSON object at the end of this turn.
If you need an answer before drafting the plan, return {"kind":"question","message":"Explain why you need the answer","schema":{"type":"object","properties":{"field_id":{"type":"string","title":"Question or label"}},"required":["field_id"]}}. Use stable field keys. Supported field types are string, boolean, integer, number, and arrays of distinct string choices. Use enum values or titled choices for options and only include defaults that match the field. Ask only for information needed to plan.
If you have enough information, return {"kind":"plan","markdown":"Complete proposed plan in Markdown"}."#;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StructuredOutput {
    Question { message: String, schema: Value },
    Plan { markdown: String },
}

pub(crate) enum StructuredPlan {
    Question {
        form: Value,
        normalized: super::questions::QuestionForm,
    },
    Plan(String),
}

impl StructuredPlan {
    pub(crate) fn decode(text: &str) -> Result<Self> {
        let text = text.trim();
        let json = text
            .strip_prefix("```json\n")
            .or_else(|| text.strip_prefix("```json\r\n"))
            .and_then(|body| body.strip_suffix("```"))
            .unwrap_or(text);
        match serde_json::from_str(json).context("expected one structured planning result")? {
            StructuredOutput::Question { message, schema } => {
                let normalized = super::questions::QuestionForm::schema(message.clone(), &schema)?;
                ensure!(
                    !normalized.fields.is_empty(),
                    "the structured question contains no fields"
                );
                Ok(Self::Question {
                    form: serde_json::json!({"message":message,"schema":schema}),
                    normalized,
                })
            }
            StructuredOutput::Plan { markdown } => {
                ensure!(!markdown.trim().is_empty(), "the structured plan is empty");
                Ok(Self::Plan(markdown))
            }
        }
    }
}
