use crate::workflows::{ApprovalDecisionSource, WorkflowRevision};

use super::*;

fn handoff(id: &str, version: &str, source: &str, digest: &str) -> MaintenanceHandoff {
    MaintenanceHandoff {
        id: id.into(),
        project_id: "project".into(),
        workflow_run_id: "workflow-run".into(),
        workflow: WorkflowRevision {
            id: "workflow".into(),
            revision: "workflow-revision".into(),
        },
        node_id: "activate".into(),
        git_artifact_id: "verified-git".into(),
        task_id: "task".into(),
        unit_id: "unit".into(),
        plan_revision: "plan".into(),
        settings_revision: "current-settings".into(),
        version: version.into(),
        source_revision: source.into(),
        source_tree: "tree".into(),
        candidate_revision: "candidate".into(),
        sha256: digest.into(),
        size: 100,
        package_name: "installer.exe".into(),
        commit_evidence_id: "commit".into(),
        verification_id: "verification".into(),
        checks: vec!["tests".into()],
        approval_artifact_id: "approval".into(),
        approval_source: ApprovalDecisionSource::Operator,
        acceptance_artifact_id: "acceptance".into(),
        review_artifact_id: "review".into(),
        review_run_id: "review-run".into(),
        build_executable_revision: "builder".into(),
        recorded_at: "2026-11-16T00:00:00Z".into(),
        activation: None,
        signed_activation: None,
    }
}

#[test]
fn only_current_granted_handoffs_matching_the_advertised_version_and_source_remain() {
    let target = AutomaticUpdateTarget {
        project_id: "project".into(),
        maintenance_settings_revision: "current-settings".into(),
    };
    let unrelated = MaintenanceHandoff {
        project_id: "unrelated".into(),
        ..handoff("unrelated", "1.2.0", "signed-source", "sha")
    };
    let superseded = MaintenanceHandoff {
        settings_revision: "older".into(),
        ..handoff("superseded", "1.2.0", "signed-source", "sha")
    };
    let handoffs = [
        unrelated,
        superseded,
        handoff("other-source", "1.2.0", "unsigned-source", "sha"),
        handoff("other-version", "1.3.0", "signed-source", "sha"),
        handoff("first-package", "v1.2.0", "signed-source", "different"),
        handoff("second-package", "1.2.0", "signed-source", "sha"),
    ];
    let candidates: Vec<_> = eligible_handoffs(&handoffs, &target)
        .into_iter()
        .filter(|handoff| matches_advertisement(handoff, "1.2.0", "signed-source"))
        .map(|handoff| handoff.id.as_str())
        .collect();
    assert_eq!(candidates, ["first-package", "second-package"]);
}

#[test]
fn attempted_package_is_excluded_under_a_new_handoff_for_the_same_bytes() {
    let target = AutomaticUpdateTarget {
        project_id: "project".into(),
        maintenance_settings_revision: "current-settings".into(),
    };
    let attempted = MaintenanceHandoff {
        settings_revision: "older".into(),
        ..handoff("attempted", "v1.2.0", "signed-source", "attempted-sha")
    };
    let handoffs = [
        attempted,
        handoff("duplicate", "1.2.0", "signed-source", "attempted-sha"),
        handoff("different-digest", "1.2.0", "signed-source", "new-sha"),
    ];
    let mut candidates = eligible_handoffs(&handoffs, &target);
    exclude_attempted_packages(
        &mut candidates,
        &handoffs,
        &HashSet::from(["attempted".into()]),
    );
    assert_eq!(
        candidates
            .iter()
            .map(|handoff| handoff.id.as_str())
            .collect::<Vec<_>>(),
        ["different-digest"]
    );
}
