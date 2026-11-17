use serde::{Deserialize, Serialize};

use crate::model_catalog::ModelSelection;

use super::{ConditionPredicate, command::CommandConfiguration, mcp_tools::McpToolConfiguration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinNode {
    Agent,
    PromptDecision,
    PromptOutcome,
    ValidateOutput,
    CollectEvidence,
    QualityGate,
    TypedStart,
    SummarizeResult,
    LoadWorkContext,
    ManageWork,
    MaterializeBacklog,
    RunBacklog,
    SetFields,
    FilterMapList,
    BuildText,
    RequestInput,
    Approval,
    CompareModels,
    FindWork,
    UpdateProgress,
    Start,
    LoadContext,
    Plan,
    ReviewPlan,
    LinkWork,
    ApproveImplementation,
    PrepareWorktree,
    Implement,
    ReviewCandidate,
    ReadGit,
    ReviewDiffCandidate,
    VerifyReviewDiff,
    PublishDiffReport,
    VerifyCandidate,
    VerifyTestPhase,
    VerifyIntegratedChecks,
    SearchPublicRepositories,
    OpenOrUpdatePullRequest,
    WaitForCi,
    MergePullRequest,
    AcceptCandidate,
    VerifyCommit,
    VerifyMerge,
    ActivateMaintenance,
    Command,
    McpTool,
    Condition,
    Parallel,
    Join,
    Subflow,
    RepeatUntil,
    RepeatOutput,
    IterationLimit,
    DeliveryAttempt,
    BuildList,
    ForEach,
    ForEachBegin,
    ForEachGate,
    ForEachCollect,
    Result,
}

impl BuiltinNode {
    pub(super) fn generated_iteration(self) -> bool {
        matches!(
            self,
            Self::ForEachBegin
                | Self::ForEachGate
                | Self::ForEachCollect
                | Self::RepeatOutput
                | Self::DeliveryAttempt
        )
    }

    pub(super) fn per_unit(self) -> bool {
        matches!(
            self,
            Self::Implement
                | Self::DeliveryAttempt
                | Self::ReviewCandidate
                | Self::VerifyCandidate
                | Self::VerifyTestPhase
                | Self::OpenOrUpdatePullRequest
                | Self::WaitForCi
                | Self::MergePullRequest
                | Self::AcceptCandidate
                | Self::VerifyCommit
                | Self::VerifyMerge
                | Self::ActivateMaintenance
                | Self::Result
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    AgentResponse,
    AgileEvent,
    ResultSummary,
    WorkflowContext,
    JsonObject,
    EvidenceSet,
    TestPhaseEvidence,
    IntegratedRevision,
    IntegratedChecksEvidence,
    GithubResearch,
    GithubPullRequest,
    GithubCi,
    Backlog,
    RenderedText,
    ComparisonResults,
    AgileTarget,
    AgileProgressReceipt,
    Markdown,
    Plan,
    Review,
    Task,
    Approval,
    Release,
    Worktree,
    Candidate,
    Checks,
    Acceptance,
    Git,
    GitInspection,
    DiffReport,
    Merge,
    CommandResult,
    McpToolResult,
    JsonList,
    JsonItem,
    IterationResults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "artifact", rename_all = "snake_case")]
pub enum PortType {
    Control,
    Data(ArtifactKind),
    Evidence,
}

impl PortType {
    pub(super) fn accepts(self, source: Self) -> bool {
        self == source || matches!((self, source), (Self::Evidence, Self::Data(_)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Port {
    pub name: String,
    pub value: PortType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationKind {
    PromptDecision,
    PromptOutcome,
    CandidateAcceptance,
    SearchPublicRepositories,
    VerifyTestPhase,
    IntegratedChecks,
    RunBacklog,
    TypedStart,
    ValidateOutput,
    QualityGate,
    LoadWorkContext,
    ManageWork,
    SetFields,
    FilterMapList,
    BuildText,
    RequestInput,
    Approval,
    CompareModels,
    FindWork,
    UpdateProgress,
    None,
    AgentRole,
    RequiredChecks,
    CandidateChecks,
    Command,
    McpTool,
    Condition,
    SubflowEntry,
    RepeatUntil,
    RepeatOutput,
    DeliveryAttempt,
    BuildList,
    ForEach,
    ForEachBegin,
    ForEachGate,
    ForEachCollect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeConfiguration {
    PromptDecision(super::prompt::PromptDecisionConfiguration),
    PromptOutcome(super::prompt::PromptOutcomeConfiguration),
    CandidateAcceptance(super::candidate_acceptance::CandidateAcceptanceConfiguration),
    SearchPublicRepositories(super::github::SearchConfiguration),
    VerifyTestPhase(crate::repository::verification::test_phase::TestPhaseConfiguration),
    IntegratedChecks(super::integrated_checks::IntegratedChecksConfiguration),
    RunBacklog(super::backlog::execution::RunBacklogConfiguration),
    TypedStart(super::launch::TypedStartConfiguration),
    ValidateOutput(super::data_nodes::ValidateOutputConfiguration),
    QualityGate(super::data_nodes::QualityGateConfiguration),
    LoadWorkContext {
        mode: super::agile_nodes::LoadWorkContextMode,
    },
    ManageWork {
        action: super::agile_nodes::ManageWorkConfiguration,
    },
    SetFields(super::data_nodes::SetFieldsConfiguration),
    FilterMapList(super::data_nodes::FilterMapListConfiguration),
    BuildText(super::data_nodes::BuildTextConfiguration),
    RequestInput(super::decision_nodes::RequestInputConfiguration),
    Approval(super::decision_nodes::ApprovalConfiguration),
    CompareModels(super::comparison::stage::WorkflowComparisonConfiguration),
    FindWork {
        query: super::agile_nodes::FindWorkConfiguration,
    },
    UpdateProgress {
        action: super::agile_nodes::UpdateProgressConfiguration,
    },
    None,
    AgentRole {
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ModelSelection>,
        #[serde(
            default,
            skip_serializing_if = "super::stage_requirements::StageRequirementConfiguration::is_empty"
        )]
        requirements: super::stage_requirements::StageRequirementConfiguration,
    },
    RequiredChecks {
        names: Vec<String>,
        #[serde(
            rename = "parallelGroups",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        parallel_groups: Vec<Vec<String>>,
    },
    StagedChecks {
        names: Vec<String>,
        #[serde(
            rename = "parallelGroups",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        parallel_groups: Vec<Vec<String>>,
        #[serde(rename = "deferredNames")]
        deferred_names: Vec<String>,
        #[serde(
            rename = "strategyChecks",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        strategy_checks: Vec<String>,
    },
    Command(CommandConfiguration),
    McpTool(McpToolConfiguration),
    // Authoring only. Publication resolves this against the exact referenced preset.
    PresetOverrides {
        fields: serde_json::Map<String, serde_json::Value>,
    },
    Condition {
        predicate: ConditionPredicate,
    },
    SubflowEntry {
        outputs: Vec<Port>,
    },
    RepeatUntil(super::repetition::RepeatUntilConfiguration),
    RepeatOutput(super::repetition::RepeatOutputConfiguration),
    DeliveryAttempt {
        ordinal: u16,
        limit: u16,
    },
    BuildList(super::iteration::BuildListConfiguration),
    ForEach(super::iteration::ForEachConfiguration),
    ForEachBegin(super::iteration::ForEachBeginConfiguration),
    ForEachGate(super::iteration::ForEachGateConfiguration),
    ForEachCollect(super::iteration::ForEachCollectConfiguration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    PromptDecision,
    PromptOutcome,
    SearchPublicRepositories,
    OpenOrUpdatePullRequest,
    WaitForCi,
    MergePullRequest,
    VerifyTestPhase,
    VerifyIntegratedChecks,
    MaterializeBacklog,
    RunBacklog,
    SummarizeResult,
    ValidateOutput,
    CollectEvidence,
    QualityGate,
    ReadWorkContext,
    ManageWork,
    SetFields,
    FilterMapList,
    BuildText,
    AwaitWorkflowInput,
    AwaitWorkflowApproval,
    CompareModels,
    FindWork,
    UpdateProgress,
    BindInput,
    ReadContext,
    PlanAgent,
    ReadOnlyAgent,
    FindOrCreateWork,
    RecordImplementationRelease,
    ProvisionWorktree,
    WriteAgent,
    CheckCandidate,
    ReadSelectedGit,
    CheckSelectedGit,
    ReportSelectedGit,
    RecordCandidateAcceptance,
    VerifyGitCommit,
    VerifyGitMerge,
    ActivateMaintenanceFixture,
    ExecuteCommand,
    ExecuteMcpTool,
    EvaluateCondition,
    FanOut,
    JoinBranches,
    ExpandSubflow,
    RepeatSubflow,
    SelectRepeatOutput,
    StopAtIterationLimit,
    ReserveDeliveryAttempt,
    BuildList,
    ExpandForEach,
    BindIterationList,
    GateIterationItem,
    CollectIterationResults,
    PublishResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    None,
    ReadEvidence,
    InvokeModel,
    ChangeTaskRecords,
    ReleaseImplementation,
    ProvisionManagedCheckout,
    WriteRepository,
    InvokeLocalTool,
    VerifyCandidate,
    RecordAcceptance,
    VerifyRepositoryCommit,
    VerifyRepositoryMerge,
    RecordMaintenanceActivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDefinition {
    pub kind: BuiltinNode,
    pub revision: u32,
    pub title: String,
    pub executor: Executor,
    pub configuration: ConfigurationKind,
    pub effect: Effect,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}

pub(super) fn supports_pinned_execution(pinned: &NodeDefinition) -> bool {
    let mut registry = builtin_registry();
    let Some(current) = registry.iter_mut().find(|entry| entry.kind == pinned.kind) else {
        return false;
    };
    // A published Git graph used these two display titles in its persisted definitions.
    match (pinned.kind, pinned.title.as_str()) {
        (BuiltinNode::VerifyCommit, "Commit") => current.title = "Commit".into(),
        (BuiltinNode::VerifyMerge, "Merge") => current.title = "Merge".into(),
        _ => {}
    }
    if pinned.kind == BuiltinNode::Command && pinned.revision == 1 {
        current.revision = 1;
        current.outputs.retain(|port| port.name != "failed");
    }
    if pinned.kind == BuiltinNode::TypedStart && pinned.revision == 1 {
        current.revision = 1;
        current.outputs.retain(|port| port.name != "plan");
    }
    if pinned.kind == BuiltinNode::Agent && pinned.revision == 1 {
        current.revision = 1;
        current.inputs.retain(|port| port.name != "review_subject");
    }
    if pinned.kind == BuiltinNode::PromptOutcome {
        let outputs = pinned
            .outputs
            .iter()
            .filter_map(|port| {
                let name = port.name.strip_prefix("value-")?;
                match port.value {
                    PortType::Data(kind) => Some((name.to_owned(), kind)),
                    _ => None,
                }
            })
            .collect();
        return super::prompt::PromptOutcomeConfiguration { outputs }
            .definition(current)
            .is_ok_and(|definition| definition == *pinned);
    }
    if pinned.kind == BuiltinNode::ReviewDiffCandidate && pinned.revision == 1 {
        current.revision = 1;
        current.inputs.retain(|port| port.name != "brief");
        current
            .outputs
            .retain(|port| !matches!(port.name.as_str(), "failed" | "report"));
    }
    if matches!(pinned.kind, BuiltinNode::Plan | BuiltinNode::ReviewPlan) && pinned.revision == 1 {
        current.revision = 1;
        current.inputs.retain(|port| port.name != "brief");
        current.outputs.retain(|port| port.name != "failed");
    }
    if pinned.kind == BuiltinNode::AcceptCandidate && pinned.revision <= 3 {
        current.revision = pinned.revision;
        current
            .inputs
            .retain(|port| port.name != "selected_reviews");
    }
    if matches!(
        pinned.kind,
        BuiltinNode::Implement | BuiltinNode::ReviewCandidate
    ) && pinned.revision == 1
    {
        current.revision = 1;
        current
            .outputs
            .retain(|port| !matches!(port.name.as_str(), "failed" | "report"));
        current.inputs.retain(|port| port.name != "brief");
    }
    if pinned.kind == BuiltinNode::AcceptCandidate && pinned.revision == 1 {
        current.revision = 1;
        current.configuration = ConfigurationKind::None;
        current
            .inputs
            .retain(|port| !matches!(port.name.as_str(), "quality" | "build"));
    }
    if matches!(
        pinned.kind,
        BuiltinNode::VerifyCandidate | BuiltinNode::VerifyCommit
    ) && pinned.revision == 1
    {
        current.revision = 1;
        current.outputs.retain(|port| port.name != "failed");
    }
    if pinned.kind == BuiltinNode::VerifyCandidate && pinned.revision == 1 {
        current.configuration = ConfigurationKind::RequiredChecks;
    }
    if pinned.kind == BuiltinNode::VerifyCandidate && pinned.revision <= 2 {
        current.revision = pinned.revision;
        current.inputs.retain(|port| port.name != "strategy");
    }
    if pinned.kind == BuiltinNode::AcceptCandidate && pinned.revision <= 2 {
        current.revision = pinned.revision;
        current.outputs.retain(|port| port.name != "failed");
    }
    if pinned.kind == BuiltinNode::RunBacklog && pinned.revision == 1 {
        current.revision = 1;
        current.outputs.retain(|port| port.name != "integrated");
    }
    if pinned.kind == BuiltinNode::MaterializeBacklog && pinned.revision == 1 {
        current.revision = 1;
        current
            .inputs
            .retain(|port| !matches!(port.name.as_str(), "design" | "strategy"));
    }
    if pinned.kind == BuiltinNode::RepeatOutput {
        return super::repetition::matches_output_definition(current, pinned);
    }
    *current == *pinned
}

pub(super) fn prompt_outcome_matches(
    config: &super::prompt::PromptOutcomeConfiguration,
    pinned: &NodeDefinition,
) -> bool {
    builtin_registry()
        .into_iter()
        .find(|definition| definition.kind == BuiltinNode::PromptOutcome)
        .and_then(|base| config.definition(&base).ok())
        .is_some_and(|definition| definition == *pinned)
}

pub fn builtin_registry() -> Vec<NodeDefinition> {
    use ArtifactKind::*;
    use BuiltinNode::*;
    use PortType::{Control, Data, Evidence};

    let entries = [
        (
            PromptDecision,
            "Resolve prompt decision",
            Executor::PromptDecision,
            ConfigurationKind::PromptDecision,
            Effect::ReadEvidence,
            &[("ready", Control), ("response", Evidence)][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("decision", Data(JsonObject)),
            ][..],
        ),
        (
            PromptOutcome,
            "Collect prompt outcome",
            Executor::PromptOutcome,
            ConfigurationKind::PromptOutcome,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("decision", Data(JsonObject)),
                ("evidence", Data(EvidenceSet)),
            ],
            &[
                ("completed", Control),
                ("failed", Control),
                ("outcome", Data(JsonObject)),
            ],
        ),
        (
            SearchPublicRepositories,
            "Search public repositories",
            Executor::SearchPublicRepositories,
            ConfigurationKind::SearchPublicRepositories,
            Effect::ReadEvidence,
            &[("ready", Control), ("query", Data(RenderedText))][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("research", Data(GithubResearch)),
            ][..],
        ),
        (
            OpenOrUpdatePullRequest,
            "Open or update PR",
            Executor::OpenOrUpdatePullRequest,
            ConfigurationKind::None,
            Effect::WriteRepository,
            &[
                ("ready", Control),
                ("git", Data(Git)),
                ("risk", Data(JsonObject)),
            ][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("pull", Data(GithubPullRequest)),
            ][..],
        ),
        (
            WaitForCi,
            "Wait for CI",
            Executor::WaitForCi,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[("ready", Control), ("pull", Data(GithubPullRequest))][..],
            &[
                ("passed", Control),
                ("approval_required", Control),
                ("failed", Control),
                ("stale", Control),
                ("ci", Data(GithubCi)),
            ][..],
        ),
        (
            MergePullRequest,
            "Merge PR",
            Executor::MergePullRequest,
            ConfigurationKind::None,
            Effect::VerifyRepositoryMerge,
            &[
                ("ready", Control),
                ("ci", Data(GithubCi)),
                ("approval", Data(JsonObject)),
            ][..],
            &[
                ("merged", Control),
                ("failed", Control),
                ("stale", Control),
                ("integration", Data(Merge)),
            ][..],
        ),
        (
            VerifyTestPhase,
            "Verify test phase",
            Executor::VerifyTestPhase,
            ConfigurationKind::VerifyTestPhase,
            Effect::VerifyCandidate,
            &[
                ("ready", Control),
                ("release", Data(Release)),
                ("worktree", Data(Worktree)),
                ("candidate", Data(Candidate)),
                ("prior", Data(TestPhaseEvidence)),
                ("tests", Data(JsonObject)),
            ][..],
            &[
                ("verified", Control),
                ("failed", Control),
                ("blocked", Control),
                ("evidence", Data(TestPhaseEvidence)),
            ][..],
        ),
        (
            VerifyIntegratedChecks,
            "Verify integrated checks",
            Executor::VerifyIntegratedChecks,
            ConfigurationKind::IntegratedChecks,
            Effect::VerifyCandidate,
            &[("ready", Control), ("integrated", Data(IntegratedRevision))][..],
            &[
                ("verified", Control),
                ("failed", Control),
                ("blocked", Control),
                ("evidence", Data(IntegratedChecksEvidence)),
            ][..],
        ),
        (
            RunBacklog,
            "Run backlog",
            Executor::RunBacklog,
            ConfigurationKind::RunBacklog,
            Effect::ChangeTaskRecords,
            &[("ready", Control), ("backlog", Data(Backlog))][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("deliveries", Data(JsonObject)),
                ("integrated", Data(IntegratedRevision)),
            ][..],
        ),
        (
            MaterializeBacklog,
            "Materialize backlog",
            Executor::MaterializeBacklog,
            ConfigurationKind::None,
            Effect::ChangeTaskRecords,
            &[
                ("ready", Control),
                ("plan", Data(JsonObject)),
                ("sdd", Data(JsonObject)),
                ("bdd", Data(JsonObject)),
                ("design", Data(JsonObject)),
                ("strategy", Data(JsonObject)),
            ][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("backlog", Data(Backlog)),
            ][..],
        ),
        (
            QualityGate,
            "Quality gate",
            Executor::QualityGate,
            ConfigurationKind::QualityGate,
            Effect::ReadEvidence,
            &[("ready", Control), ("evidence", Data(EvidenceSet))][..],
            &[
                ("passed", Control),
                ("revise", Control),
                ("blocked", Control),
                ("decision", Data(JsonObject)),
            ][..],
        ),
        (
            CollectEvidence,
            "Collect evidence",
            Executor::CollectEvidence,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[("ready", Control), ("evidence", Evidence)][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("evidence", Data(EvidenceSet)),
                ("text", Data(RenderedText)),
            ][..],
        ),
        (
            BuiltinNode::SetFields,
            "Set fields",
            Executor::SetFields,
            ConfigurationKind::SetFields,
            Effect::ReadEvidence,
            &[("ready", Control), ("source", Evidence)][..],
            &[
                ("completed", Control),
                ("failed", Control),
                ("object", Data(JsonObject)),
            ][..],
        ),
        (
            ValidateOutput,
            "Validate output",
            Executor::ValidateOutput,
            ConfigurationKind::ValidateOutput,
            Effect::ReadEvidence,
            &[("ready", Control), ("source", Evidence)],
            &[
                ("completed", Control),
                ("failed", Control),
                ("object", Data(JsonObject)),
            ],
        ),
        (
            BuiltinNode::FilterMapList,
            "Filter and map list",
            Executor::FilterMapList,
            ConfigurationKind::FilterMapList,
            Effect::ReadEvidence,
            &[("ready", Control), ("items", Data(JsonList))],
            &[
                ("completed", Control),
                ("failed", Control),
                ("items", Data(JsonList)),
            ],
        ),
        (
            BuiltinNode::BuildText,
            "Build text",
            Executor::BuildText,
            ConfigurationKind::BuildText,
            Effect::ReadEvidence,
            &[("ready", Control), ("values", Data(JsonObject))],
            &[
                ("completed", Control),
                ("failed", Control),
                ("text", Data(RenderedText)),
            ],
        ),
        (
            BuiltinNode::RequestInput,
            "Request input",
            Executor::AwaitWorkflowInput,
            ConfigurationKind::RequestInput,
            Effect::None,
            &[("ready", Control)],
            &[("answered", Control), ("answers", Data(JsonObject))],
        ),
        (
            BuiltinNode::Approval,
            "Approval",
            Executor::AwaitWorkflowApproval,
            ConfigurationKind::Approval,
            Effect::None,
            &[("ready", Control), ("evidence", Evidence)],
            &[
                ("approved", Control),
                ("rejected", Control),
                ("decision", Data(JsonObject)),
            ],
        ),
        (
            BuiltinNode::CompareModels,
            "Compare models",
            Executor::CompareModels,
            ConfigurationKind::CompareModels,
            Effect::InvokeModel,
            &[
                ("ready", Control),
                ("request", Data(RenderedText)),
                ("context", Data(Markdown)),
            ],
            &[
                ("completed", Control),
                ("failed", Control),
                ("results", Data(ComparisonResults)),
            ],
        ),
        (
            BuiltinNode::FindWork,
            "Find work",
            Executor::FindWork,
            ConfigurationKind::FindWork,
            Effect::ReadEvidence,
            &[("ready", Control)],
            &[
                ("completed", Control),
                ("failed", Control),
                ("matches", Data(JsonList)),
                ("task", Data(Task)),
                ("target", Data(AgileTarget)),
            ],
        ),
        (
            BuiltinNode::UpdateProgress,
            "Update progress",
            Executor::UpdateProgress,
            ConfigurationKind::UpdateProgress,
            Effect::ChangeTaskRecords,
            &[("ready", Control), ("target", Data(AgileTarget))],
            &[
                ("completed", Control),
                ("failed", Control),
                ("progress", Data(AgileProgressReceipt)),
                ("target", Data(AgileTarget)),
            ],
        ),
        (
            Start,
            "Start",
            Executor::BindInput,
            ConfigurationKind::None,
            Effect::None,
            &[][..],
            &[("start", Control), ("document", Data(Markdown))][..],
        ),
        (
            TypedStart,
            "Start",
            Executor::BindInput,
            ConfigurationKind::TypedStart,
            Effect::None,
            &[],
            &[
                ("start", Control),
                ("inputs", Data(JsonObject)),
                ("document", Data(Markdown)),
                ("task", Data(Task)),
                ("event", Data(AgileEvent)),
                ("plan", Data(ArtifactKind::Plan)),
            ],
        ),
        (
            Agent,
            "Agent",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[
                ("ready", Control),
                ("prompt", Data(RenderedText)),
                ("comparison", Data(ComparisonResults)),
                ("review_subject", Data(EvidenceSet)),
            ],
            &[
                ("completed", Control),
                ("failed", Control),
                ("response", Data(AgentResponse)),
            ],
        ),
        (
            SummarizeResult,
            "Result",
            Executor::SummarizeResult,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("summary", Data(RenderedText)),
                ("evidence", Evidence),
            ],
            &[("completed", Control), ("summary", Data(ResultSummary))],
        ),
        (
            LoadWorkContext,
            "Load work context",
            Executor::ReadWorkContext,
            ConfigurationKind::LoadWorkContext,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("task", Data(Task)),
                ("document", Data(Markdown)),
            ],
            &[
                ("loaded", Control),
                ("failed", Control),
                ("context", Data(WorkflowContext)),
            ],
        ),
        (
            ManageWork,
            "Create or link work",
            Executor::ManageWork,
            ConfigurationKind::ManageWork,
            Effect::ChangeTaskRecords,
            &[
                ("ready", Control),
                ("title", Data(RenderedText)),
                ("task", Data(Task)),
            ],
            &[
                ("completed", Control),
                ("failed", Control),
                ("task", Data(Task)),
                ("target", Data(AgileTarget)),
            ],
        ),
        (
            LoadContext,
            "Load context",
            Executor::ReadContext,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[("ready", Control), ("document", Data(Markdown))],
            &[("loaded", Control), ("context", Data(Markdown))],
        ),
        (
            BuiltinNode::Plan,
            "Plan",
            Executor::PlanAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[("ready", Control), ("context", Data(Markdown))],
            &[
                ("planned", Control),
                ("plan", Data(ArtifactKind::Plan)),
                ("failed", Control),
            ],
        ),
        (
            ReviewPlan,
            "Review plan",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[
                ("ready", Control),
                ("plan", Data(ArtifactKind::Plan)),
                ("brief", Data(RenderedText)),
            ],
            &[
                ("reviewed", Control),
                ("review", Data(Review)),
                ("failed", Control),
            ],
        ),
        (
            LinkWork,
            "Create or link work",
            Executor::FindOrCreateWork,
            ConfigurationKind::None,
            Effect::ChangeTaskRecords,
            &[
                ("ready", Control),
                ("plan", Data(ArtifactKind::Plan)),
                ("review", Data(Review)),
            ],
            &[("linked", Control), ("task", Data(Task))],
        ),
        (
            ApproveImplementation,
            "Approve implementation",
            Executor::RecordImplementationRelease,
            ConfigurationKind::None,
            Effect::ReleaseImplementation,
            &[
                ("ready", Control),
                ("plan", Data(ArtifactKind::Plan)),
                ("task", Data(Task)),
            ],
            &[
                ("approved", Control),
                ("decision", Data(ArtifactKind::Approval)),
            ],
        ),
        (
            PrepareWorktree,
            "Prepare worktree",
            Executor::ProvisionWorktree,
            ConfigurationKind::None,
            Effect::ProvisionManagedCheckout,
            &[
                ("ready", Control),
                ("task", Data(Task)),
                ("decision", Data(ArtifactKind::Approval)),
            ],
            &[
                ("prepared", Control),
                ("worktree", Data(Worktree)),
                ("release", Data(Release)),
            ],
        ),
        (
            Implement,
            "Implement",
            Executor::WriteAgent,
            ConfigurationKind::AgentRole,
            Effect::WriteRepository,
            &[
                ("ready", Control),
                ("plan", Data(ArtifactKind::Plan)),
                ("release", Data(Release)),
                ("worktree", Data(Worktree)),
                ("brief", Data(RenderedText)),
            ],
            &[
                ("implemented", Control),
                ("candidate", Data(Candidate)),
                ("failed", Control),
            ],
        ),
        (
            ReviewCandidate,
            "Review candidate",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[
                ("ready", Control),
                ("candidate", Data(Candidate)),
                ("brief", Data(RenderedText)),
            ],
            &[
                ("reviewed", Control),
                ("review", Data(Review)),
                ("failed", Control),
                ("report", Data(JsonObject)),
            ],
        ),
        (
            ReadGit,
            "Read Git",
            Executor::ReadSelectedGit,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[("ready", Control), ("document", Data(Markdown))],
            &[("read", Control), ("git", Data(GitInspection))],
        ),
        (
            ReviewDiffCandidate,
            "Review diff",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[
                ("ready", Control),
                ("git", Data(GitInspection)),
                ("brief", Data(RenderedText)),
            ],
            &[
                ("reviewed", Control),
                ("review", Data(Review)),
                ("failed", Control),
                ("report", Data(JsonObject)),
            ],
        ),
        (
            VerifyReviewDiff,
            "Check selected diff",
            Executor::CheckSelectedGit,
            ConfigurationKind::RequiredChecks,
            Effect::VerifyCandidate,
            &[("ready", Control), ("git", Data(GitInspection))],
            &[("verified", Control), ("checks", Data(Checks))],
        ),
        (
            PublishDiffReport,
            "Publish diff report",
            Executor::ReportSelectedGit,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("git", Data(GitInspection)),
                ("review", Data(Review)),
                ("checks", Data(Checks)),
            ],
            &[("published", Control), ("report", Data(DiffReport))],
        ),
        (
            VerifyCandidate,
            "Verify candidate",
            Executor::CheckCandidate,
            ConfigurationKind::CandidateChecks,
            Effect::VerifyCandidate,
            &[
                ("ready", Control),
                ("candidate", Data(Candidate)),
                ("strategy", Data(JsonObject)),
            ],
            &[
                ("verified", Control),
                ("failed", Control),
                ("checks", Data(Checks)),
            ],
        ),
        (
            AcceptCandidate,
            "Accept candidate",
            Executor::RecordCandidateAcceptance,
            ConfigurationKind::CandidateAcceptance,
            Effect::RecordAcceptance,
            &[
                ("ready", Control),
                ("candidate", Data(Candidate)),
                ("review", Data(Review)),
                ("checks", Data(Checks)),
                ("quality", Data(JsonObject)),
                ("build", Data(Checks)),
                ("selected_reviews", Data(JsonObject)),
            ],
            &[
                ("accepted", Control),
                ("failed", Control),
                ("acceptance", Data(Acceptance)),
            ],
        ),
        (
            VerifyCommit,
            "Verify commit",
            Executor::VerifyGitCommit,
            ConfigurationKind::None,
            Effect::VerifyRepositoryCommit,
            &[
                ("ready", Control),
                ("candidate", Data(Candidate)),
                ("acceptance", Data(Acceptance)),
            ],
            &[
                ("verified", Control),
                ("failed", Control),
                ("git", Data(Git)),
            ],
        ),
        (
            VerifyMerge,
            "Verify merge",
            Executor::VerifyGitMerge,
            ConfigurationKind::None,
            Effect::VerifyRepositoryMerge,
            &[("ready", Control), ("git", Data(Git))],
            &[("verified", Control), ("integration", Data(Merge))],
        ),
        (
            ActivateMaintenance,
            "Activate maintenance candidate",
            Executor::ActivateMaintenanceFixture,
            ConfigurationKind::None,
            Effect::RecordMaintenanceActivation,
            &[("ready", Control), ("git", Data(Git))],
            &[("activated", Control), ("source", Data(Git))],
        ),
        (
            Command,
            "Command",
            Executor::ExecuteCommand,
            ConfigurationKind::Command,
            Effect::WriteRepository,
            &[
                ("ready", Control),
                ("release", Data(Release)),
                ("worktree", Data(Worktree)),
            ],
            &[
                ("completed", Control),
                ("failed", Control),
                ("result", Data(CommandResult)),
            ],
        ),
        (
            McpTool,
            "MCP tool",
            Executor::ExecuteMcpTool,
            ConfigurationKind::McpTool,
            Effect::InvokeLocalTool,
            &[("ready", Control), ("arguments", Evidence)],
            &[
                ("completed", Control),
                ("failed", Control),
                ("result", Data(McpToolResult)),
            ],
        ),
        (
            Condition,
            "Condition",
            Executor::EvaluateCondition,
            ConfigurationKind::Condition,
            Effect::ReadEvidence,
            &[("ready", Control), ("value", Evidence)],
            &[("matched", Control), ("unmatched", Control)],
        ),
        (
            Parallel,
            "Parallel",
            Executor::FanOut,
            ConfigurationKind::None,
            Effect::None,
            &[("ready", Control)],
            &[("branches", Control)],
        ),
        (
            Join,
            "Join",
            Executor::JoinBranches,
            ConfigurationKind::None,
            Effect::None,
            &[("branches", Control)],
            &[("joined", Control)],
        ),
        (
            Subflow,
            "Subflow",
            Executor::ExpandSubflow,
            ConfigurationKind::None,
            Effect::None,
            &[][..],
            &[][..],
        ),
        (
            RepeatUntil,
            "Repeat until",
            Executor::RepeatSubflow,
            ConfigurationKind::RepeatUntil,
            Effect::None,
            &[][..],
            &[][..],
        ),
        (
            IterationLimit,
            "Iteration limit",
            Executor::StopAtIterationLimit,
            ConfigurationKind::None,
            Effect::None,
            &[("ready", Control)],
            &[("failed", Control)],
        ),
        (
            DeliveryAttempt,
            "Reserve delivery attempt",
            Executor::ReserveDeliveryAttempt,
            ConfigurationKind::DeliveryAttempt,
            Effect::ChangeTaskRecords,
            &[("ready", Control)],
            &[("admitted", Control), ("exhausted", Control)],
        ),
        (
            RepeatOutput,
            "Select approved iteration",
            Executor::SelectRepeatOutput,
            ConfigurationKind::RepeatOutput,
            Effect::ReadEvidence,
            &[][..],
            &[("completed", Control), ("failed", Control)],
        ),
        (
            BuildList,
            "Build list",
            Executor::BuildList,
            ConfigurationKind::BuildList,
            Effect::ReadEvidence,
            &[("ready", Control), ("value", Evidence)],
            &[
                ("completed", Control),
                ("failed", Control),
                ("items", Data(JsonList)),
            ],
        ),
        (
            ForEach,
            "For each",
            Executor::ExpandForEach,
            ConfigurationKind::ForEach,
            Effect::None,
            &[("ready", Control), ("items", Data(JsonList))],
            &[
                ("completed", Control),
                ("results", Data(IterationResults)),
                ("failed", Control),
            ],
        ),
        (
            ForEachBegin,
            "Pin iteration list",
            Executor::BindIterationList,
            ConfigurationKind::ForEachBegin,
            Effect::ReadEvidence,
            &[("ready", Control), ("items", Data(JsonList))],
            &[
                ("ready", Control),
                ("failed", Control),
                ("items", Data(JsonList)),
            ],
        ),
        (
            ForEachGate,
            "Select iteration item",
            Executor::GateIterationItem,
            ConfigurationKind::ForEachGate,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("items", Data(JsonList)),
                ("previous", Evidence),
            ],
            &[("iterate", Control), ("item", Data(JsonItem))],
        ),
        (
            ForEachCollect,
            "Collect iteration results",
            Executor::CollectIterationResults,
            ConfigurationKind::ForEachCollect,
            Effect::ReadEvidence,
            &[
                ("ready", Control),
                ("items", Data(JsonList)),
                ("finished", Control),
                ("result", Evidence),
            ],
            &[("completed", Control), ("results", Data(IterationResults))],
        ),
        (
            Result,
            "Result",
            Executor::PublishResult,
            ConfigurationKind::None,
            Effect::ReadEvidence,
            &[("ready", Control), ("task", Data(Task)), ("git", Data(Git))],
            &[("completed", Control), ("delivery", Data(Git))],
        ),
    ];
    let ports = |entries: &[(&str, PortType)]| {
        entries
            .iter()
            .map(|(name, value)| Port {
                name: (*name).into(),
                value: *value,
            })
            .collect()
    };
    entries
        .into_iter()
        .map(
            |(kind, title, executor, configuration, effect, inputs, outputs)| NodeDefinition {
                kind,
                revision: match kind {
                    BuiltinNode::AcceptCandidate => 4,
                    BuiltinNode::VerifyCandidate => 3,
                    BuiltinNode::Command
                    | BuiltinNode::TypedStart
                    | BuiltinNode::Plan
                    | BuiltinNode::ReviewPlan
                    | BuiltinNode::Implement
                    | BuiltinNode::ReviewCandidate
                    | BuiltinNode::ReviewDiffCandidate
                    | BuiltinNode::RunBacklog
                    | BuiltinNode::MaterializeBacklog
                    | BuiltinNode::VerifyCommit
                    | BuiltinNode::Agent => 2,
                    _ => 1,
                },
                title: title.into(),
                executor,
                configuration,
                effect,
                inputs: ports(inputs),
                outputs: ports(outputs),
            },
        )
        .collect()
}

impl ArtifactKind {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::AgentResponse => "agent_response",
            Self::AgileEvent => "agile_event",
            Self::ResultSummary => "result_summary",
            Self::WorkflowContext => "workflow_context",
            // Protected payloads and pinned ports distinguish these JSON artifacts.
            Self::JsonObject
            | Self::EvidenceSet
            | Self::Backlog
            | Self::TestPhaseEvidence
            | Self::GithubResearch
            | Self::GithubPullRequest
            | Self::GithubCi
            | Self::IntegratedRevision
            | Self::IntegratedChecksEvidence => "json_object",
            Self::RenderedText => "rendered_text",
            Self::ComparisonResults => "comparison_results",
            Self::AgileTarget => "agile_target",
            Self::AgileProgressReceipt => "agile_progress_receipt",
            Self::Markdown => "markdown",
            Self::Plan => "plan",
            Self::Review => "review",
            Self::Task => "task",
            Self::Approval => "approval",
            Self::Release => "release",
            Self::Worktree => "worktree",
            Self::Candidate => "candidate",
            Self::Checks => "checks",
            Self::Acceptance => "acceptance",
            // Protected payloads and pinned ports distinguish repository Git evidence types.
            Self::Git | Self::Merge | Self::GitInspection | Self::DiffReport => "git",
            Self::CommandResult => "command_result",
            Self::McpToolResult => "mcp_tool_result",
            Self::JsonList => "json_list",
            Self::JsonItem => "json_item",
            Self::IterationResults => "iteration_results",
        }
    }
}

impl NodeConfiguration {
    pub(super) fn matches(&self, kind: ConfigurationKind) -> bool {
        match (self, kind) {
            (Self::None, ConfigurationKind::None) => true,
            (Self::PromptDecision(config), ConfigurationKind::PromptDecision) => config.valid(),
            (Self::PromptOutcome(config), ConfigurationKind::PromptOutcome) => config.valid(),
            (Self::None, ConfigurationKind::CandidateAcceptance) => true,
            (Self::CandidateAcceptance(config), ConfigurationKind::CandidateAcceptance) => {
                config.valid()
            }
            (
                Self::SearchPublicRepositories(config),
                ConfigurationKind::SearchPublicRepositories,
            ) => config.valid(),
            (Self::VerifyTestPhase(config), ConfigurationKind::VerifyTestPhase) => config.valid(),
            (Self::IntegratedChecks(config), ConfigurationKind::IntegratedChecks) => config.valid(),
            (Self::RunBacklog(config), ConfigurationKind::RunBacklog) => config.valid(),
            (Self::TypedStart(config), ConfigurationKind::TypedStart) => config.valid(),
            (Self::QualityGate(config), ConfigurationKind::QualityGate) => config.valid(),
            (Self::ValidateOutput(config), ConfigurationKind::ValidateOutput) => {
                config.validate().is_ok()
            }
            (Self::LoadWorkContext { .. }, ConfigurationKind::LoadWorkContext)
            | (Self::ManageWork { .. }, ConfigurationKind::ManageWork) => true,
            (Self::SetFields(config), ConfigurationKind::SetFields) => config.validate().is_ok(),
            (Self::FilterMapList(config), ConfigurationKind::FilterMapList) => {
                config.validate().is_ok()
            }
            (Self::BuildText(config), ConfigurationKind::BuildText) => config.validate().is_ok(),
            (Self::RequestInput(config), ConfigurationKind::RequestInput) => config.is_valid(),
            (Self::Approval(config), ConfigurationKind::Approval) => config.is_valid(),
            (Self::CompareModels(config), ConfigurationKind::CompareModels) => config.valid(),
            (Self::FindWork { query }, ConfigurationKind::FindWork) => query.valid(),
            (Self::UpdateProgress { .. }, ConfigurationKind::UpdateProgress) => true,
            (
                Self::AgentRole {
                    role,
                    model,
                    requirements,
                },
                ConfigurationKind::AgentRole,
            ) => {
                !role.trim().is_empty()
                    && requirements.is_valid()
                    && model.as_ref().is_none_or(|selection| {
                        !selection.profile_id.trim().is_empty()
                            && !selection.model_id.trim().is_empty()
                    })
            }
            (
                Self::RequiredChecks {
                    names,
                    parallel_groups,
                },
                ConfigurationKind::RequiredChecks | ConfigurationKind::CandidateChecks,
            ) => {
                names.iter().all(|name| !name.trim().is_empty())
                    && crate::repository::verification::valid_parallel_check_groups(parallel_groups)
            }
            (
                Self::StagedChecks {
                    names,
                    parallel_groups,
                    deferred_names,
                    strategy_checks,
                },
                ConfigurationKind::CandidateChecks,
            ) => {
                !deferred_names.is_empty()
                    && names.iter().all(|name| !name.trim().is_empty())
                    && crate::repository::verification::valid_parallel_check_groups(parallel_groups)
                    && crate::repository::verification::valid_deferred_check_names(
                        names,
                        parallel_groups,
                        deferred_names,
                    )
                    && strategy_checks.len() <= 32
                    && strategy_checks.iter().enumerate().all(|(index, name)| {
                        !name.trim().is_empty()
                            && name.len() <= 256
                            && !name.chars().any(char::is_control)
                            && !strategy_checks[..index].contains(name)
                            && !names.contains(name)
                            && !deferred_names.contains(name)
                            && !parallel_groups
                                .iter()
                                .flatten()
                                .any(|grouped| grouped == name)
                    })
            }
            (Self::Command(config), ConfigurationKind::Command) => config.is_valid(),
            (Self::McpTool(config), ConfigurationKind::McpTool) => config.is_valid(),
            (Self::Condition { predicate }, ConfigurationKind::Condition) => predicate.valid(),
            (Self::SubflowEntry { outputs }, ConfigurationKind::SubflowEntry) => {
                super::subflow::valid_entry_ports(outputs)
            }
            (Self::RepeatUntil(configuration), ConfigurationKind::RepeatUntil) => {
                configuration.valid()
            }
            (Self::RepeatOutput(configuration), ConfigurationKind::RepeatOutput) => {
                configuration.valid()
            }
            (Self::DeliveryAttempt { ordinal, limit }, ConfigurationKind::DeliveryAttempt) => {
                (1..=32).contains(ordinal) && (1..=32).contains(limit)
            }
            (Self::BuildList(configuration), ConfigurationKind::BuildList) => configuration.valid(),
            (Self::ForEach(configuration), ConfigurationKind::ForEach) => configuration.valid(),
            (Self::ForEachBegin(configuration), ConfigurationKind::ForEachBegin) => {
                configuration.valid()
            }
            (Self::ForEachGate(configuration), ConfigurationKind::ForEachGate) => {
                configuration.valid()
            }
            (Self::ForEachCollect(configuration), ConfigurationKind::ForEachCollect) => {
                configuration.valid()
            }
            _ => false,
        }
    }
}

impl Executor {
    pub(super) fn scheduler_profile(self) -> Option<&'static str> {
        match self {
            Self::PlanAgent => Some("planning"),
            Self::ReadOnlyAgent | Self::ExecuteMcpTool | Self::CompareModels => Some("read_only"),
            Self::WriteAgent | Self::ExecuteCommand => Some("approved_write"),
            _ => None,
        }
    }
}
