use std::{collections::VecDeque, sync::Mutex};

use serde_json::json;

use super::*;

const HEAD: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";
const TEST: &str = "3333333333333333333333333333333333333333";
const TREE: &str = "4444444444444444444444444444444444444444";
const MERGE: &str = "5555555555555555555555555555555555555555";
const NEXT: &str = "6666666666666666666666666666666666666666";
const SYNC_HEAD: &str = "7777777777777777777777777777777777777777";
const ADVANCED: &str = "8888888888888888888888888888888888888888";
const REQUEST: &str = "630b9d5e-3f2a-4f7e-8b0c-2d5f9a8c1e42";
const LIST: &str = "repos/omni/project/pulls?state=all&head=omni%3Atask%2Funit&per_page=100&page=1";
const PR: &str = "repos/omni/project/pulls/7";
const HEAD_REF: &str = "repos/omni/project/branches/task%2Funit";
const BASE_REF: &str = "repos/omni/project/branches/main";

struct Call {
    method: &'static str,
    endpoint: String,
    payload: Option<Value>,
    response: ApiResponse,
}

impl Call {
    fn get(endpoint: impl Into<String>, body: Value) -> Self {
        Self::new("GET", endpoint, None, 200, body)
    }

    fn new(
        method: &'static str,
        endpoint: impl Into<String>,
        payload: Option<Value>,
        status: u16,
        body: Value,
    ) -> Self {
        Self {
            method,
            endpoint: endpoint.into(),
            payload,
            response: ApiResponse {
                status,
                body,
                next_page: false,
            },
        }
    }
}

struct Fixture(Mutex<VecDeque<Call>>);

impl GithubTransport for Fixture {
    async fn request(
        &self,
        method: &str,
        endpoint: &str,
        payload: Option<Value>,
    ) -> Result<ApiResponse> {
        let call = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("Unexpected {method} {endpoint}"));
        assert_eq!(
            (method, endpoint, payload),
            (call.method, call.endpoint.as_str(), call.payload)
        );
        Ok(call.response)
    }
}

fn github(calls: impl IntoIterator<Item = Call>) -> GithubCli<Fixture> {
    GithubCli::with_transport(Fixture(Mutex::new(calls.into_iter().collect())))
}

fn slug() -> RepositorySlug {
    RepositorySlug {
        owner: "omni".into(),
        name: "project".into(),
    }
}

fn pull(body: &str, base: &str, merged: bool) -> Value {
    pull_at(body, HEAD, base, merged)
}

fn pull_at(body: &str, head: &str, base: &str, merged: bool) -> Value {
    let repo = json!({"id": 12, "name": "project", "owner":{"login":"omni"}});
    json!({"node_id": "PR_kwxyz", "number": 7, "state": if merged { "closed" } else { "open" },
        "title": "Implement unit", "body": body, "mergeable": true, "merged": merged,
        "merge_commit_sha": if merged { MERGE } else { TEST },
        "head": {"ref":"task/unit", "sha":head, "repo":repo},
        "base": {"ref":"main", "sha":base, "repo":repo}
    })
}

fn expected_pull() -> PullRequest {
    serde_json::from_value::<ApiPullRequest>(pull("", BASE, false))
        .unwrap()
        .publishable(&slug())
        .unwrap()
}

fn branch(name: &str, sha: &str) -> Value {
    json!({"name":name, "commit":{"sha":sha}})
}

fn commit(sha: &str, tree: &str, base: &str, head: &str) -> Value {
    json!({"sha":sha,"tree":{"sha":tree},"parents":[{"sha":base},{"sha":head}]})
}

fn required() -> Vec<RequiredCheck> {
    vec![RequiredCheck {
        context: "CI / unit".into(),
        app_id: Some(42),
    }]
}

fn approved_ci() -> CiSnapshot {
    CiSnapshot {
        pull_number: 7,
        head_sha: HEAD.into(),
        base_sha: BASE.into(),
        tested_sha: TEST.into(),
        tested_tree: Some(TREE.into()),
        state: CiState::Passed,
        checks: vec![CheckEvidence {
            context: "CI / unit".into(),
            app_id: Some(42),
            state: CiState::Passed,
            source: "check_run".into(),
            id: 123,
        }],
        required_checks: required(),
        reason: None,
    }
}

fn ci_prefix() -> Vec<Call> {
    vec![
        Call::get(PR, pull("", BASE, false)),
        Call::get(HEAD_REF, branch("task/unit", HEAD)),
        Call::get(BASE_REF, branch("main", BASE)),
    ]
}

fn check_calls(app: u64, legacy: Value) -> [Call; 2] {
    check_calls_at(TEST, app, legacy)
}

fn check_calls_at(sha: &str, app: u64, legacy: Value) -> [Call; 2] {
    [
        Call::get(
            format!(
                "repos/omni/project/commits/{sha}/check-runs?filter=latest&per_page=100&page=1"
            ),
            json!({"check_runs":[{"id":123,"name":"CI / unit","head_sha":sha,
                "status":"completed","conclusion":"success","app":{"id": app}}]}),
        ),
        Call::get(
            format!("repos/omni/project/commits/{sha}/statuses?per_page=100&page=1"),
            legacy,
        ),
    ]
}

fn strict_rules() -> Call {
    Call::get(
        "repos/omni/project/rules/branches/main?per_page=100&page=1",
        json!([{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":true,
            "required_status_checks":[{"context":"CI / unit","integration_id":42}]}}]),
    )
}

fn compare_ancestor(ancestor: &str, tip: &str) -> Call {
    Call::get(
        format!("repos/omni/project/compare/{ancestor}...{tip}?per_page=1"),
        json!({"status":"ahead","merge_base_commit":{"sha":ancestor}}),
    )
}

fn synchronized_pull() -> PullRequest {
    serde_json::from_value::<ApiPullRequest>(pull_at("", SYNC_HEAD, NEXT, false))
        .unwrap()
        .publishable(&slug())
        .unwrap()
}

fn synchronized_ci_calls(final_base: &str) -> Vec<Call> {
    let mut calls = vec![
        Call::get(PR, pull_at("", SYNC_HEAD, NEXT, false)),
        Call::get(HEAD_REF, branch("task/unit", SYNC_HEAD)),
        Call::get(BASE_REF, branch("main", NEXT)),
        Call::get(
            format!("repos/omni/project/git/commits/{TEST}"),
            commit(TEST, TREE, NEXT, SYNC_HEAD),
        ),
        Call::get(
            format!(
                "repos/omni/project/commits/{TEST}/check-runs?filter=latest&per_page=100&page=1"
            ),
            json!({"check_runs":[]}),
        ),
        Call::get(
            format!("repos/omni/project/commits/{TEST}/statuses?per_page=100&page=1"),
            json!([]),
        ),
        Call::get(
            format!("repos/omni/project/git/commits/{SYNC_HEAD}"),
            commit(SYNC_HEAD, TREE, HEAD, NEXT),
        ),
    ];
    calls.extend(check_calls_at(SYNC_HEAD, 42, json!([])));
    calls.extend([
        Call::get(HEAD_REF, branch("task/unit", SYNC_HEAD)),
        Call::get(BASE_REF, branch("main", final_base)),
    ]);
    calls
}

#[test]
fn parses_only_direct_github_remote_forms() {
    for url in [
        "https://github.com/omni/project.git",
        "git@github.com:omni/project.git",
        "ssh://git@github.com/omni/project.git",
    ] {
        assert_eq!(RepositorySlug::from_remote_url(url).unwrap(), slug());
    }
    for url in [
        "https://github.com@elsewhere/omni/project.git",
        "https://github.com/omni/project?token=x",
        "https://github.com/omni/other/project",
    ] {
        assert!(RepositorySlug::from_remote_url(url).is_err());
    }
}

#[tokio::test]
async fn public_search_pins_branch_revision_and_queries_license_at_that_commit() {
    let public = json!({"id":12,"name":"project","owner":{"login":"omni"},"default_branch":"main",
        "private":false,"visibility":"public","description":"example"});
    let mut private = public.clone();
    private["private"] = json!(true);
    let fixture = github([
        Call::get(
            "search/repositories?q=workflow%20is%3Apublic&per_page=2",
            json!({"items":[private,public]}),
        ),
        Call::get("repos/omni/project", public),
        Call::get(BASE_REF, branch("main", BASE)),
        Call::get(
            format!("repos/omni/project/license?ref={BASE}"),
            json!({"license":{"spdx_id":"MIT","name":"MIT License"}}),
        ),
    ]);
    let found = fixture
        .search_public_repositories("workflow", 2)
        .await
        .unwrap();
    let [repository] = found.as_slice() else {
        panic!("expected one public repository");
    };
    assert_eq!(repository.revision, BASE);
    assert_eq!(repository.license.as_deref(), Some("MIT"));
    assert_eq!(repository.url, "https://github.com/omni/project");
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reuses_existing_task_pull_after_branch_synchronization() {
    let marker = task_marker("task-1").unwrap();
    let body = format!("Work notes\n\n{marker}");
    let fixture = github([
        Call::get(LIST, json!([pull_at(&body, SYNC_HEAD, NEXT, false)])),
        Call::get(PR, pull_at(&body, SYNC_HEAD, NEXT, false)),
        Call::get(HEAD_REF, branch("task/unit", SYNC_HEAD)),
        Call::get(PR, pull_at(&body, SYNC_HEAD, NEXT, false)),
    ]);
    let request = PullRequestRequest {
        head_branch: "task/unit".into(),
        head_sha: SYNC_HEAD.into(),
        base_branch: "main".into(),
        task_key: "task-1".into(),
        title: "Implement unit".into(),
        body: "Work notes".into(),
    };
    let pull = fixture
        .open_or_update_pull_request(&slug(), &request)
        .await
        .unwrap();
    assert_eq!(
        (pull.number, pull.head_sha.as_str(), pull.base_sha.as_str()),
        (7, SYNC_HEAD, NEXT)
    );
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_pull_with_same_refs_but_different_task() {
    let body = task_marker("task-other").unwrap();
    let fixture = github([Call::get(LIST, json!([pull(&body, BASE, false)]))]);
    let error = fixture
        .find_pull_request(&slug(), "task/unit", "main", "task-1")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different work"));
}

#[tokio::test]
async fn rejects_an_open_pull_on_the_same_head_to_a_different_destination() {
    let body = task_marker("task-1").unwrap();
    let mut competing = pull(&body, BASE, false);
    competing["base"]["ref"] = json!("release");
    let fixture = github([Call::get(LIST, json!([competing]))]);
    let error = fixture
        .find_pull_request(&slug(), "task/unit", "main", "task-1")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different work"));
}

#[tokio::test]
async fn stale_test_merge_has_no_approved_ci() {
    let mut calls = ci_prefix();
    calls.push(Call::get(
        format!("repos/omni/project/git/commits/{TEST}"),
        commit(TEST, TREE, NEXT, HEAD),
    ));
    let fixture = github(calls);
    let result = fixture
        .inspect_ci(&slug(), &expected_pull(), &required())
        .await
        .unwrap();
    assert_eq!(result.state, CiState::Pending);
    assert!(result.tested_sha.is_empty());
}

#[tokio::test]
async fn all_evidence_for_a_required_context_must_pass() {
    let test_commit = || {
        Call::get(
            format!("repos/omni/project/git/commits/{TEST}"),
            commit(TEST, TREE, BASE, HEAD),
        )
    };
    for (app, legacy, state) in [
        (42, json!([]), CiState::Passed),
        (99, json!([]), CiState::Pending),
        (
            42,
            json!([{"id":100,"state":"failure","context":"ci / UNIT"},
                {"id":99,"state":"success","context":"CI / unit"}]),
            CiState::Failed,
        ),
    ] {
        let mut calls = ci_prefix();
        calls.push(test_commit());
        calls.extend(check_calls(app, legacy));
        if matches!(state, CiState::Passed | CiState::Failed) {
            calls.extend([
                Call::get(HEAD_REF, branch("task/unit", HEAD)),
                Call::get(BASE_REF, branch("main", BASE)),
            ]);
        }
        let fixture = github(calls);
        let result = fixture
            .inspect_ci(&slug(), &expected_pull(), &required())
            .await
            .unwrap();
        assert_eq!(result.state, state);
        assert_eq!(result.tested_sha, TEST);
    }
}

#[tokio::test]
async fn submits_an_exact_protected_merge_and_adopts_only_its_matching_async_request() {
    let mut calls = vec![
        Call::get(PR, pull("", BASE, false)),
        strict_rules(),
        compare_ancestor(BASE, HEAD),
    ];
    calls.extend(ci_prefix());
    calls.push(Call::get(
        format!("repos/omni/project/git/commits/{TEST}"),
        commit(TEST, TREE, BASE, HEAD),
    ));
    calls.extend(check_calls(42, json!([])));
    calls.extend([
        Call::get(HEAD_REF, branch("task/unit", HEAD)),
        Call::get(BASE_REF, branch("main", BASE)),
    ]);
    calls.push(Call::get(PR, pull("", BASE, false)));
    calls.push(Call::get(BASE_REF, branch("main", BASE)));
    calls.push(Call::new("PUT", "repos/omni/project/pulls/7/merge-async",
        Some(json!({"sha":HEAD,"merge_method":"merge","merge_action":"direct_merge","bypass_rules":false})),
        409, json!({"status":"pending", "details":{"uuid":REQUEST,"expected_head_sha":HEAD,
            "merge_method":"merge","merge_action":"direct_merge"}})));
    let fixture = github(calls);
    let result = fixture
        .merge_pull_request(&slug(), &expected_pull(), &approved_ci())
        .await
        .unwrap();
    assert_eq!(
        result,
        MergeOutcome::Pending {
            request_id: Some(REQUEST.into())
        }
    );
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn synchronized_head_uses_its_ci_and_proves_the_protected_merge() {
    let mut calls = synchronized_ci_calls(NEXT);
    calls.extend([
        Call::get(PR, pull_at("", SYNC_HEAD, NEXT, false)),
        strict_rules(),
        compare_ancestor(NEXT, SYNC_HEAD),
    ]);
    calls.extend(synchronized_ci_calls(NEXT));
    calls.extend([
        Call::get(PR, pull_at("", SYNC_HEAD, NEXT, false)),
        Call::get(BASE_REF, branch("main", NEXT)),
        Call::new("PUT", "repos/omni/project/pulls/7/merge-async",
            Some(json!({"sha":SYNC_HEAD,"merge_method":"merge","merge_action":"direct_merge","bypass_rules":false})),
            202, json!({"status":"pending","details":{"uuid":REQUEST,"expected_head_sha":SYNC_HEAD,
                "merge_method":"merge","merge_action":"direct_merge"}})),
        Call::get(format!("repos/omni/project/pulls/7/merge-async/{REQUEST}"),
            json!({"status":"merged","details":{"sha":MERGE}})),
        Call::get(PR, pull_at("", SYNC_HEAD, ADVANCED, true)),
        Call::get(format!("repos/omni/project/git/commits/{MERGE}"), commit(MERGE, TREE, NEXT, SYNC_HEAD)),
        Call::get(BASE_REF, branch("main", ADVANCED)),
        compare_ancestor(MERGE, ADVANCED),
    ]);
    let fixture = github(calls);
    let pull = synchronized_pull();
    let approved = fixture
        .inspect_ci(&slug(), &pull, &required())
        .await
        .unwrap();
    assert_eq!(approved.state, CiState::Passed);
    assert_eq!(
        (
            approved.tested_sha.as_str(),
            approved.tested_tree.as_deref()
        ),
        (SYNC_HEAD, Some(TREE))
    );
    assert_eq!(
        fixture
            .merge_pull_request(&slug(), &pull, &approved)
            .await
            .unwrap(),
        MergeOutcome::Pending {
            request_id: Some(REQUEST.into())
        }
    );

    let MergeOutcome::Verifiable(proof) = fixture
        .poll_merge(&slug(), &pull, &approved, REQUEST)
        .await
        .unwrap()
    else {
        panic!("expected a verified synchronized merge");
    };
    assert_eq!(
        (
            proof.head_sha.as_str(),
            proof.base_sha.as_str(),
            proof.tree_sha.as_str()
        ),
        (SYNC_HEAD, NEXT, TREE)
    );
    assert_eq!(
        (proof.merge_sha.as_str(), proof.destination_sha.as_str()),
        (MERGE, ADVANCED)
    );
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn synchronized_head_does_not_reuse_terminal_ci_when_base_moves_during_collection() {
    let fixture = github(synchronized_ci_calls(ADVANCED));
    let snapshot = fixture
        .inspect_ci(&slug(), &synchronized_pull(), &required())
        .await
        .unwrap();
    assert_eq!(snapshot.state, CiState::Unavailable);
    assert!(snapshot.tested_sha.is_empty());
    assert!(snapshot.checks.is_empty());
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn synchronized_merge_does_not_accept_an_advanced_first_parent() {
    let mut approved = approved_ci();
    approved.head_sha = SYNC_HEAD.into();
    approved.base_sha = NEXT.into();
    approved.tested_sha = SYNC_HEAD.into();
    let fixture = github([
        Call::get(PR, pull_at("", SYNC_HEAD, ADVANCED, true)),
        Call::get(
            format!("repos/omni/project/git/commits/{MERGE}"),
            commit(MERGE, TREE, ADVANCED, SYNC_HEAD),
        ),
    ]);
    let error = fixture
        .verify_merge(&slug(), &synchronized_pull(), &approved)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("approved parents"));
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn blocks_merge_when_destination_does_not_enforce_strict_checks() {
    let fixture = github([
        Call::get(PR, pull("", BASE, false)),
        Call::get(
            "repos/omni/project/rules/branches/main?per_page=100&page=1",
            json!([]),
        ),
        Call::get(
            "repos/omni/project/branches/main/protection",
            json!({"enforce_admins":{"enabled":true},"required_status_checks":{
                "strict":false,"checks":[{"context":"CI / unit","app_id":42}]}}),
        ),
    ]);
    let error = fixture
        .merge_pull_request(&slug(), &expected_pull(), &approved_ci())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("strict required status checks"));
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reports_confirmed_async_failure_as_a_terminal_outcome() {
    let fixture = github([Call::get(
        format!("repos/omni/project/pulls/7/merge-async/{REQUEST}"),
        json!({"status":"failed","details":{"message":"Required approval was dismissed."}}),
    )]);
    let outcome = fixture
        .poll_merge(&slug(), &expected_pull(), &approved_ci(), REQUEST)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Rejected {
            reason: "GitHub rejected the protected merge request"
        }
    );
    assert!(fixture.transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_a_pending_merge_with_bypass_or_a_stacked_pull() {
    let fixture = github([Call::get(
        format!("repos/omni/project/pulls/7/merge-async/{REQUEST}"),
        json!({"status":"pending", "details":{"uuid":REQUEST,"expected_head_sha":HEAD,
            "merge_method":"merge","merge_action":"direct_merge","bypass_rules":true}}),
    )]);
    let error = fixture
        .poll_merge(&slug(), &expected_pull(), &approved_ci(), REQUEST)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("protected merge method"));

    let mut stacked = pull("", BASE, false);
    stacked["stack"] = json!({"base":{"ref":"main","sha":BASE},"size":2,"position":2});
    let fixture = github([Call::get(PR, stacked)]);
    let error = fixture.pull_request(&slug(), 7).await.unwrap_err();
    assert!(error.to_string().contains("Stacked GitHub pull requests"));
}

#[tokio::test]
async fn verifies_merge_proof_after_destination_advances_and_rejects_wrong_parent_order() {
    for (first, second) in [(BASE, HEAD), (HEAD, BASE)] {
        let mut calls = vec![
            Call::get(PR, pull("", NEXT, true)),
            Call::get(
                format!("repos/omni/project/git/commits/{MERGE}"),
                commit(MERGE, TREE, first, second),
            ),
        ];
        if first == BASE {
            calls.push(Call::get(BASE_REF, branch("main", NEXT)));
            calls.push(compare_ancestor(MERGE, NEXT));
        }
        let fixture = github(calls);
        let result = fixture
            .verify_merge(&slug(), &expected_pull(), &approved_ci())
            .await;
        if first == BASE {
            let proof = result.unwrap().unwrap();
            assert_eq!(
                (proof.merge_sha.as_str(), proof.destination_sha.as_str()),
                (MERGE, NEXT)
            );
        } else {
            assert!(result.unwrap_err().to_string().contains("approved parents"));
        }
    }
}
