use serde::{Deserialize, Serialize};

use super::command::CommandConfiguration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinNode {
    Start,
    LoadContext,
    Plan,
    ReviewPlan,
    LinkWork,
    ApproveImplementation,
    PrepareWorktree,
    Implement,
    ReviewCandidate,
    VerifyCandidate,
    AcceptCandidate,
    VerifyCommit,
    VerifyMerge,
    Command,
    Result,
}

impl BuiltinNode {
    pub(super) fn per_unit(self) -> bool {
        matches!(
            self,
            Self::Implement
                | Self::ReviewCandidate
                | Self::VerifyCandidate
                | Self::AcceptCandidate
                | Self::VerifyCommit
                | Self::VerifyMerge
                | Self::Result
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
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
    Merge,
    CommandResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "artifact", rename_all = "snake_case")]
pub enum PortType {
    Control,
    Data(ArtifactKind),
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
    None,
    AgentRole,
    RequiredChecks,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeConfiguration {
    None,
    AgentRole { role: String },
    RequiredChecks { names: Vec<String> },
    Command(CommandConfiguration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    BindInput,
    ReadContext,
    PlanAgent,
    ReadOnlyAgent,
    FindOrCreateWork,
    RecordImplementationRelease,
    ProvisionWorktree,
    WriteAgent,
    CheckCandidate,
    RecordCandidateAcceptance,
    VerifyGitCommit,
    VerifyGitMerge,
    ExecuteCommand,
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
    VerifyCandidate,
    RecordAcceptance,
    VerifyRepositoryCommit,
    VerifyRepositoryMerge,
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
    *current == *pinned
}

pub fn builtin_registry() -> Vec<NodeDefinition> {
    use ArtifactKind::*;
    use BuiltinNode::*;
    use PortType::{Control, Data};

    let entries = [
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
            &[("planned", Control), ("plan", Data(ArtifactKind::Plan))],
        ),
        (
            ReviewPlan,
            "Review plan",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[("ready", Control), ("plan", Data(ArtifactKind::Plan))],
            &[("reviewed", Control), ("review", Data(Review))],
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
            &[("approved", Control), ("decision", Data(Approval))],
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
                ("decision", Data(Approval)),
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
            ],
            &[("implemented", Control), ("candidate", Data(Candidate))],
        ),
        (
            ReviewCandidate,
            "Review candidate",
            Executor::ReadOnlyAgent,
            ConfigurationKind::AgentRole,
            Effect::InvokeModel,
            &[("ready", Control), ("candidate", Data(Candidate))],
            &[("reviewed", Control), ("review", Data(Review))],
        ),
        (
            VerifyCandidate,
            "Verify candidate",
            Executor::CheckCandidate,
            ConfigurationKind::RequiredChecks,
            Effect::VerifyCandidate,
            &[("ready", Control), ("candidate", Data(Candidate))],
            &[("verified", Control), ("checks", Data(Checks))],
        ),
        (
            AcceptCandidate,
            "Accept candidate",
            Executor::RecordCandidateAcceptance,
            ConfigurationKind::None,
            Effect::RecordAcceptance,
            &[
                ("ready", Control),
                ("candidate", Data(Candidate)),
                ("review", Data(Review)),
                ("checks", Data(Checks)),
            ],
            &[("accepted", Control), ("acceptance", Data(Acceptance))],
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
            &[("verified", Control), ("git", Data(Git))],
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
            Command,
            "Command",
            Executor::ExecuteCommand,
            ConfigurationKind::Command,
            Effect::WriteRepository,
            &[("ready", Control), ("release", Data(Release)), ("worktree", Data(Worktree))],
            &[("completed", Control), ("result", Data(CommandResult))],
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
                revision: 1,
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
            // Existing databases classify both receipts as repository Git evidence.
            // Protected payloads and pinned ports distinguish the two receipts.
            Self::Git | Self::Merge => "git",
            Self::CommandResult => "command_result",
        }
    }
}

impl NodeConfiguration {
    pub(super) fn matches(&self, kind: ConfigurationKind) -> bool {
        match (self, kind) {
            (Self::None, ConfigurationKind::None) => true,
            (Self::AgentRole { role }, ConfigurationKind::AgentRole) => !role.trim().is_empty(),
            (Self::RequiredChecks { names }, ConfigurationKind::RequiredChecks) => {
                names.iter().all(|name| !name.trim().is_empty())
            }
            (Self::Command(config), ConfigurationKind::Command) => config.is_valid(),
            _ => false,
        }
    }
}

impl Executor {
    pub(super) fn scheduler_profile(self) -> Option<&'static str> {
        match self {
            Self::PlanAgent => Some("planning"),
            Self::ReadOnlyAgent => Some("read_only"),
            Self::WriteAgent | Self::ExecuteCommand => Some("approved_write"),
            _ => None,
        }
    }
}
