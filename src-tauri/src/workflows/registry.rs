use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model_catalog::ModelSelection;

use super::{ConditionPredicate, command::CommandConfiguration, mcp_tools::McpToolConfiguration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinNode {
    #[serde(alias = "conversation_agent")]
    Agent,
    TypedStart,
    Human,
    Wait,
    End,
    Start,
    Fork,
    JoinResults,
    #[serde(alias = "loop_gate", alias = "loop_step", alias = "loop_result")]
    Loop,
    Subflow,

    #[serde(untagged)]
    Archived(ArchivedNode),
}

/// Wire names from earlier workflow formats. These carry no registry definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchivedNode {
    PromptDecision,
    PromptOutcome,
    ValidateOutput,
    CollectEvidence,
    QualityGate,
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
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Archived(_))
    }

    pub(super) fn generated_iteration(self) -> bool {
        matches!(
            self,
            Self::Archived(ArchivedNode::ForEachBegin)
                | Self::Archived(ArchivedNode::ForEachGate)
                | Self::Archived(ArchivedNode::ForEachCollect)
                | Self::Archived(ArchivedNode::RepeatOutput)
                | Self::Archived(ArchivedNode::DeliveryAttempt)
        )
    }

    pub(super) fn per_unit(self) -> bool {
        matches!(
            self,
            Self::Archived(ArchivedNode::Implement)
                | Self::Archived(ArchivedNode::DeliveryAttempt)
                | Self::Archived(ArchivedNode::ReviewCandidate)
                | Self::Archived(ArchivedNode::VerifyCandidate)
                | Self::Archived(ArchivedNode::VerifyTestPhase)
                | Self::Archived(ArchivedNode::OpenOrUpdatePullRequest)
                | Self::Archived(ArchivedNode::WaitForCi)
                | Self::Archived(ArchivedNode::MergePullRequest)
                | Self::Archived(ArchivedNode::AcceptCandidate)
                | Self::Archived(ArchivedNode::VerifyCommit)
                | Self::Archived(ArchivedNode::VerifyMerge)
                | Self::Archived(ArchivedNode::ActivateMaintenance)
                | Self::Archived(ArchivedNode::Result)
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
    End,
    Human,
    Wait,
    Fork,
    JoinResults,
    JoinFailures,
    LegacyReviewVerdict,
    Loop,
    LoopGate,
    LoopStep,
    LoopResult,
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

impl ConfigurationKind {
    pub(super) fn is_loop_phase(self) -> bool {
        matches!(self, Self::LoopGate | Self::LoopStep | Self::LoopResult)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeConfiguration {
    End,
    Human(super::decision_nodes::HumanConfiguration),
    Wait(super::timed::WaitConfiguration),
    Fork(super::orchestration::ForkConfiguration),
    JoinResults(super::orchestration::JoinResultsConfiguration),
    JoinFailures,
    LegacyReviewVerdict,
    Loop(super::orchestration::LoopConfiguration),
    LoopGate(super::orchestration::LoopGateConfiguration),
    LoopStep(super::orchestration::LoopStepConfiguration),
    LoopResult(super::orchestration::LoopResultConfiguration),
    #[serde(rename_all = "camelCase")]
    PromptOverrides {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        role_overrides: BTreeMap<String, Value>,
    },
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
    End,
    ConversationAgent,
    AwaitDeadline,
    AwaitHuman,
    AdmitBranches,
    AggregateBranches,
    ExpandLoop,
    GateLoop,
    StepLoop,
    AggregateLoop,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_schemas: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub output_schemas: BTreeMap<String, Value>,
}

pub(super) fn supports_pinned_execution(pinned: &NodeDefinition) -> bool {
    let Some(mut current) = builtin_registry()
        .into_iter()
        .find(|entry| entry.kind == pinned.kind)
    else {
        return false;
    };
    match pinned.kind {
        BuiltinNode::End if pinned.title == "Return" => current.title = "Return".into(),
        BuiltinNode::TypedStart if pinned.revision == 1 => {
            current.revision = 1;
            current.outputs.retain(|port| port.name != "plan");
        }
        BuiltinNode::Agent if pinned.executor == Executor::ConversationAgent => {
            configure_conversation_agent(&mut current);
            if pinned.title == "Conversation Agent" {
                current.title = pinned.title.clone();
                current.outputs = vec![control("completed"), object("result"), control("failed")];
            }
        }
        BuiltinNode::Agent if matches!(pinned.revision, 3 | 4) => {
            configure_structured_agent(&mut current, pinned.executor == Executor::WriteAgent);
            if pinned.revision == 3 && pinned.executor == Executor::WriteAgent {
                current.revision = 3;
                current.inputs.extend([
                    Port { name: "release".into(), value: PortType::Data(ArtifactKind::Release) },
                    Port { name: "worktree".into(), value: PortType::Data(ArtifactKind::Worktree) },
                    Port { name: "plan".into(), value: PortType::Data(ArtifactKind::Plan) },
                ]);
            }
        }
        BuiltinNode::Agent => {
            if pinned.title == "Agent" {
                current.title = pinned.title.clone();
            }
            if pinned.revision == 1 {
                current.revision = 1;
                current.inputs.retain(|port| port.name != "review_subject");
            }
        }
        BuiltinNode::JoinResults if pinned.configuration == ConfigurationKind::JoinFailures => {
            configure_failure_join(&mut current);
        }
        BuiltinNode::JoinResults
            if pinned.configuration == ConfigurationKind::LegacyReviewVerdict =>
        {
            configure_legacy_review_verdict(&mut current);
        }
        BuiltinNode::Loop if pinned.configuration.is_loop_phase() => {
            let Some((executor, title)) = loop_phase_definition(pinned.configuration) else {
                return false;
            };
            current.configuration = pinned.configuration;
            current.executor = executor;
            current.title = title.into();
        }
        _ => {}
    }
    if matches!(
        pinned.configuration,
        ConfigurationKind::Fork | ConfigurationKind::JoinResults
    ) || pinned.configuration.is_loop_phase()
    {
        current.inputs.clone_from(&pinned.inputs);
        current.outputs.clone_from(&pinned.outputs);
    }
    current.input_schemas.clone_from(&pinned.input_schemas);
    current.output_schemas.clone_from(&pinned.output_schemas);
    current == *pinned
}

pub(super) fn loop_phase_definition(kind: ConfigurationKind) -> Option<(Executor, &'static str)> {
    match kind {
        ConfigurationKind::LoopGate => Some((Executor::GateLoop, "Start loop iteration")),
        ConfigurationKind::LoopStep => Some((Executor::StepLoop, "Record loop iteration")),
        ConfigurationKind::LoopResult => Some((Executor::AggregateLoop, "Collect loop outputs")),
        _ => None,
    }
}

pub(super) fn configure_conversation_agent(definition: &mut NodeDefinition) {
    definition.revision = 1;
    definition.executor = Executor::ConversationAgent;
    definition.configuration = ConfigurationKind::None;
    definition.inputs = vec![control("ready"), object("input")];
    definition.outputs = vec![control("completed"), control("failed"), object("result")];
}

pub(super) fn configure_failure_join(definition: &mut NodeDefinition) {
    definition.executor = Executor::JoinBranches;
    definition.configuration = ConfigurationKind::JoinFailures;
    definition.inputs = vec![control("branches")];
    definition.outputs = vec![control("joined")];
}

pub(super) fn configure_legacy_review_verdict(definition: &mut NodeDefinition) {
    definition.title = "Cross-review verdict".into();
    definition.executor = Executor::EvaluateCondition;
    definition.configuration = ConfigurationKind::LegacyReviewVerdict;
    definition.effect = Effect::ReadEvidence;
    definition.inputs = vec![
        control("ready"),
        Port {
            name: "value".into(),
            value: PortType::Evidence,
        },
    ];
    definition.outputs = vec![control("matched"), control("unmatched")];
}

fn control(name: &str) -> Port {
    Port {
        name: name.into(),
        value: PortType::Control,
    }
}

fn object(name: &str) -> Port {
    Port {
        name: name.into(),
        value: PortType::Data(ArtifactKind::JsonObject),
    }
}

pub(super) fn configure_structured_agent(definition: &mut NodeDefinition, write: bool) {
    let port = |name: &str, value| Port {
        name: name.into(),
        value,
    };
    definition.revision = 3;
    definition.title = "AI Node Agent".into();
    definition.inputs = vec![
        port("ready", PortType::Control),
        port("input", PortType::Data(ArtifactKind::JsonObject)),
    ];
    definition.outputs = vec![
        port("completed", PortType::Control),
        port("failed", PortType::Control),
        port("result", PortType::Data(ArtifactKind::JsonObject)),
    ];
    if write {
        definition.revision = 4;
        definition.executor = Executor::WriteAgent;
        definition.effect = Effect::WriteRepository;
    }
}

pub fn builtin_registry() -> Vec<NodeDefinition> {
    use ArtifactKind::*;
    use BuiltinNode::*;
    use PortType::{Control, Data};

    type Ports = &'static [(&'static str, PortType)];
    let entries: [(
        BuiltinNode,
        &str,
        Executor,
        ConfigurationKind,
        Effect,
        Ports,
        Ports,
    ); 10] = [
        (
            BuiltinNode::Wait,
            "Wait",
            Executor::AwaitDeadline,
            ConfigurationKind::Wait,
            Effect::None,
            &[("ready", Control), ("input", Data(JsonObject))],
            &[("completed", Control), ("result", Data(JsonObject))],
        ),
        (
            BuiltinNode::Human,
            "Human in the Loop",
            Executor::AwaitHuman,
            ConfigurationKind::Human,
            Effect::None,
            &[("ready", Control), ("input", Data(JsonObject))],
            &[
                ("answered", Control),
                ("approved", Control),
                ("rejected", Control),
                ("expired", Control),
                ("failed", Control),
                ("response", Data(JsonObject)),
                ("approved_result", Data(JsonObject)),
            ],
        ),
        (
            BuiltinNode::End,
            "End",
            Executor::End,
            ConfigurationKind::End,
            Effect::None,
            &[("ready", Control), ("result", Data(JsonObject))],
            &[("completed", Control), ("result", Data(JsonObject))],
        ),
        (
            BuiltinNode::Fork,
            "Parallel / Fork",
            Executor::AdmitBranches,
            ConfigurationKind::Fork,
            Effect::None,
            &[("ready", Control), ("input", Data(JsonObject))],
            &[],
        ),
        (
            BuiltinNode::JoinResults,
            "Join",
            Executor::AggregateBranches,
            ConfigurationKind::JoinResults,
            Effect::None,
            &[],
            &[
                ("joined", Control),
                ("failed", Control),
                ("results", Data(JsonObject)),
            ],
        ),
        (
            BuiltinNode::Loop,
            "Loop",
            Executor::ExpandLoop,
            ConfigurationKind::Loop,
            Effect::None,
            &[("ready", Control), ("input", Data(JsonObject))],
            &[
                ("completed", Control),
                ("limit_reached", Control),
                ("failed", Control),
                ("results", Data(JsonObject)),
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
            "AI Node Agent",
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
            Subflow,
            "Subflow",
            Executor::ExpandSubflow,
            ConfigurationKind::None,
            Effect::None,
            &[][..],
            &[][..],
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
                revision: if matches!(kind, BuiltinNode::TypedStart | BuiltinNode::Agent) {
                    2
                } else {
                    1
                },
                title: title.into(),
                executor,
                configuration,
                effect,
                inputs: ports(inputs),
                outputs: ports(outputs),
                input_schemas: BTreeMap::new(),
                output_schemas: BTreeMap::new(),
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
    pub(super) fn is_generated(&self) -> bool {
        match self {
            Self::LoopGate(_)
            | Self::LoopStep(_)
            | Self::LoopResult(_)
            | Self::JoinFailures
            | Self::LegacyReviewVerdict => true,
            Self::AgentRole { requirements, .. } => requirements
                .structured
                .as_ref()
                .is_some_and(|config| config.review_verdict),
            _ => false,
        }
    }

    pub(super) fn matches(&self, kind: ConfigurationKind) -> bool {
        match (self, kind) {
            (Self::None, ConfigurationKind::None) => true,
            (Self::End, ConfigurationKind::End) => true,
            (Self::JoinFailures, ConfigurationKind::JoinFailures) => true,
            (Self::LegacyReviewVerdict, ConfigurationKind::LegacyReviewVerdict) => true,
            (Self::Human(config), ConfigurationKind::Human) => config.is_valid(),
            (Self::Wait(config), ConfigurationKind::Wait) => config.is_valid(),
            (Self::Fork(_), ConfigurationKind::Fork)
            | (Self::JoinResults(_), ConfigurationKind::JoinResults)
            | (Self::Loop(_), ConfigurationKind::Loop)
            | (Self::LoopGate(_), ConfigurationKind::LoopGate)
            | (Self::LoopStep(_), ConfigurationKind::LoopStep)
            | (Self::LoopResult(_), ConfigurationKind::LoopResult) => true,
            (Self::TypedStart(config), ConfigurationKind::TypedStart) => config.valid(),
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
            (Self::SubflowEntry { outputs }, ConfigurationKind::SubflowEntry) => {
                super::subflow::valid_entry_ports(outputs)
            }
            _ => false,
        }
    }
}

impl Executor {
    pub(super) fn scheduler_profile(self) -> Option<&'static str> {
        match self {
            Self::PlanAgent | Self::ConversationAgent => Some("planning"),
            Self::ReadOnlyAgent | Self::ExecuteMcpTool | Self::CompareModels => Some("read_only"),
            Self::WriteAgent | Self::ExecuteCommand => Some("approved_write"),
            _ => None,
        }
    }
}
