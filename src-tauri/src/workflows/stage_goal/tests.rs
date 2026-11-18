use std::{collections::BTreeSet, path::PathBuf};

use super::*;
use crate::providers::goal::NativeGoalAction;

fn configuration(evidence: StageGoalEvidence) -> StageGoalConfiguration {
    StageGoalConfiguration {
        objective: "Implement and verify the bounded fixture".into(),
        criteria: vec![StageGoalCriterion {
            text: "Verify the recorded result".into(),
            evidence,
        }],
        max_turns: Some(3),
        max_seconds: Some(120),
        token_budget: None,
    }
}

fn scope(mode: SessionMode) -> SessionScope {
    SessionScope {
        mode,
        provider: Provider::Codex,
        profile_id: "profile".into(),
        account_binding: Some("fixture".into()),
        executable: PathBuf::from("C:/fixture/codex.exe"),
        checkout: PathBuf::from("D:/fixture/source"),
        cli_version: "fixture".into(),
        runner: "direct".into(),
        permission_revision: "bounded".into(),
        model: Some("fixture-model".into()),
        effort: None,
    }
}

fn proof(token_budget_verified: bool) -> NativeGoalEvidence {
    use NativeGoalAction::*;
    NativeGoalEvidence {
        scope_key: "fixture-qualified-scope".into(),
        evidence_id: "native-lifecycle-evidence".into(),
        controls: BTreeSet::from([SeedContext, Start, Status, Pause, Resume, Cancel]),
        token_budget_verified,
    }
}

#[test]
fn exposes_native_only_for_registered_controls_compatible_with_the_goal_limits() {
    let scope = scope(SessionMode::ApprovedWrite);
    let goal = configuration(StageGoalEvidence::CandidateGate);
    assert!(matches!(
        assess_route(&goal, &scope, true, Some(&proof(true))),
        StageGoalRoute::Managed { .. }
    ));
    assert!(matches!(
        assess_route(&goal, &scope, false, None),
        StageGoalRoute::Blocked { .. }
    ));
    let token_goal = StageGoalConfiguration {
        max_turns: None,
        token_budget: Some(500),
        ..goal
    };
    assert!(matches!(
        assess_route(&token_goal, &scope, true, None),
        StageGoalRoute::Blocked { .. }
    ));
    assert!(matches!(
        assess_route(&token_goal, &scope, true, Some(&proof(false))),
        StageGoalRoute::Blocked { .. }
    ));
    let mut incomplete = proof(true);
    incomplete.controls.remove(&NativeGoalAction::Resume);
    assert!(matches!(
        assess_route(&token_goal, &scope, true, Some(&incomplete)),
        StageGoalRoute::Blocked { .. }
    ));
    assert_eq!(
        assess_route(&token_goal, &scope, true, Some(&proof(true))),
        StageGoalRoute::NativeCandidate {
            evidence_id: "native-lifecycle-evidence".into()
        }
    );
}
