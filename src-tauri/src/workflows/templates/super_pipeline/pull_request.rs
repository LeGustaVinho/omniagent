use super::*;
use crate::workflows::decision_nodes::ApprovalConfiguration;

pub(super) const NAME: &str = "Publish, check and merge pull request";

const KEY: &str = "pull-request-attempt";
const ATTEMPTS: [&str; 3] = ["pull-request-1", "pull-request-2", "pull-request-3"];

#[cfg(test)]
mod tests;

pub(super) fn append(
    flow: &mut Flow<'_>,
    mut ready: Output<'_>,
    git: Output<'_>,
    risk: Output<'_>,
) -> [Output<'static>; 4] {
    for attempt in ATTEMPTS {
        flow.fixed(
            attempt,
            BuiltinNode::Subflow,
            NodeConfiguration::None,
            ready,
            &[("git", git), ("risk", risk)],
        );
        flow.references
            .insert(attempt.into(), PackageReference::Draft { key: KEY.into() });
        ready = (attempt, "stale");
    }
    let [first, second, last] = ATTEMPTS;
    [
        (first, "failed"),
        (second, "failed"),
        (last, "failed"),
        (last, "stale"),
    ]
}

pub(super) fn attempt(presets: &mut Vec<PackageMember>) -> PackageMember {
    let fields = [
        ("ready", PortType::Control),
        ("git", PortType::Data(ArtifactKind::Git)),
        ("risk", PortType::Data(ArtifactKind::JsonObject)),
    ];
    let mut flow = Flow::subflow(NAME, presets, &fields);
    flow.fixed(
        "open-pr",
        BuiltinNode::OpenOrUpdatePullRequest,
        NodeConfiguration::None,
        ("start", "ready"),
        &[("git", ("start", "git")), ("risk", ("start", "risk"))],
    );
    flow.fixed(
        "wait-for-ci",
        BuiltinNode::WaitForCi,
        NodeConfiguration::None,
        ("open-pr", "completed"),
        &[("pull", ("open-pr", "pull"))],
    );
    let ci = ("wait-for-ci", "ci");
    flow.fixed(
        "approve-high-risk-merge",
        BuiltinNode::Approval,
        NodeConfiguration::Approval(ApprovalConfiguration {
            message: "Review the exact GitHub CI result, commit pair and delivery risk before authorizing this merge."
                .into(),
        }),
        ("wait-for-ci", "approval_required"),
        &[("evidence", ci)],
    );
    flow.fixed(
        "merge-low-risk",
        BuiltinNode::MergePullRequest,
        NodeConfiguration::None,
        ("wait-for-ci", "passed"),
        &[("ci", ci)],
    );
    flow.fixed(
        "merge-approved",
        BuiltinNode::MergePullRequest,
        NodeConfiguration::None,
        ("approve-high-risk-merge", "approved"),
        &[
            ("ci", ci),
            ("approval", ("approve-high-risk-merge", "decision")),
        ],
    );
    flow.join(
        "stale",
        &[
            ("wait-for-ci", "stale"),
            ("merge-low-risk", "stale"),
            ("merge-approved", "stale"),
        ],
    );
    flow.join(
        "failed",
        &[
            ("open-pr", "failed"),
            ("wait-for-ci", "failed"),
            ("approve-high-risk-merge", "rejected"),
            ("merge-low-risk", "failed"),
            ("merge-approved", "failed"),
        ],
    );
    flow.join(
        "merged",
        &[("merge-low-risk", "merged"), ("merge-approved", "merged")],
    );
    flow.subflow_member(
        KEY,
        PackageDestination::Project,
        &fields,
        &[
            ("stale", ("stale", "joined")),
            ("failed", ("failed", "joined")),
            ("merged", ("merged", "joined")),
            ("integration", ("merge-low-risk", "integration")),
            ("approvedIntegration", ("merge-approved", "integration")),
        ],
    )
}
