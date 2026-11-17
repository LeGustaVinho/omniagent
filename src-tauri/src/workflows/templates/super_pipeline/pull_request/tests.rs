use super::*;
use crate::{store::Store, workflows::tests::approved_delivery_repository};

#[tokio::test]
async fn only_stale_evidence_reopens_pull_request_and_needs_a_fresh_approval() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("pr-retry.sqlite"))?;
    let project = approved_delivery_repository(&store, dir.path())
        .await?
        .project_id;
    let mut presets = vec![];
    let fields = [
        ("ready", PortType::Control),
        ("git", PortType::Data(ArtifactKind::Git)),
        ("risk", PortType::Data(ArtifactKind::JsonObject)),
    ];
    let mut flow = Flow::subflow("Pull request retry", &mut presets, &fields);
    let failures = append(
        &mut flow,
        ("start", "ready"),
        ("start", "git"),
        ("start", "risk"),
    );
    flow.join("delivery-failed", &failures);
    let ready = flow.subflow_member(
        "retry-scenario",
        PackageDestination::Project,
        &fields,
        &[("failed", ("delivery-failed", "joined"))],
    );
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            WorkflowPackage {
                name: "Pull request retry".into(),
                members: vec![ready, attempt(&mut presets)],
            },
        )
        .await?;
    let simulate = |outcomes| {
        let project = project.clone();
        let draft_id = draft.id.clone();
        let revision = draft.revision.clone();
        let store = &store;
        async move {
            let scenario = serde_json::from_value(json!({
                "inputs": {"ready": true, "git": {"id": "verified-commit"}, "risk": {"highRisk": true}},
                "outcomes": outcomes,
            }))?;
            let trace = store
                .simulate_workflow_package_revision(
                    project,
                    draft_id,
                    revision,
                    "retry-scenario".into(),
                    scenario,
                )
                .await?;
            Ok::<Value, anyhow::Error>(serde_json::to_value(trace)?)
        }
    };

    let open = sample(json!({"pull": {"number": 12}}));
    let stale = json!({"state": "failed", "failurePort": "stale"});
    let ci = |base| {
        sample(json!({
            "ci": {"pull": {"requiresHuman": true}, "snapshot": {"state": "passed", "baseSha": base}},
        }))
    };
    let approved = sample(json!({"decision": {"approved": true}}));
    let rejected = sample(json!({"decision": {"approved": false}}));

    let drift = json!({
        "pull-request-1::open-pr": open,
        "pull-request-1::wait-for-ci": stale,
        "pull-request-2::open-pr": open,
        "pull-request-2::wait-for-ci": ci("base-2"),
        "pull-request-2::approve-high-risk-merge": approved,
        "pull-request-2::merge-approved": stale,
        "pull-request-3::open-pr": open,
        "pull-request-3::wait-for-ci": ci("base-3"),
    });
    let waiting = simulate(drift.clone()).await?;
    assert_eq!(
        state(&waiting, "pull-request-3::approve-high-risk-merge"),
        "waiting"
    );
    assert_eq!(state(&waiting, "pull-request-3::merge-approved"), "waiting");
    assert_eq!(state(&waiting, "delivery-failed"), "waiting");

    let mut repaired = drift.clone();
    repaired["pull-request-3::approve-high-risk-merge"] = approved;
    repaired["pull-request-3::merge-approved"] =
        sample(json!({"integration": {"integrationSha": "merged-3"}}));
    let merged = simulate(repaired).await?;
    assert_eq!(state(&merged, "pull-request-3::merged"), "completed");
    assert_eq!(state(&merged, "delivery-failed"), "skipped");

    let failed = simulate(json!({
        "pull-request-1::open-pr": open,
        "pull-request-1::wait-for-ci": sample(json!({"ci": {"snapshot": {"state": "failed"}}})),
    }))
    .await?;
    assert_eq!(state(&failed, "pull-request-2::open-pr"), "skipped");
    assert_eq!(state(&failed, "delivery-failed"), "completed");

    let denied = simulate(json!({
        "pull-request-1::open-pr": open,
        "pull-request-1::wait-for-ci": ci("base-1"),
        "pull-request-1::approve-high-risk-merge": rejected,
    }))
    .await?;
    assert_eq!(state(&denied, "pull-request-2::open-pr"), "skipped");
    assert_eq!(state(&denied, "delivery-failed"), "completed");

    let mut exhausted = drift;
    exhausted["pull-request-3::wait-for-ci"] = stale;
    let terminal = simulate(exhausted).await?;
    assert_eq!(state(&terminal, "pull-request-3::stale"), "completed");
    assert_eq!(state(&terminal, "delivery-failed"), "completed");
    Ok(())
}

fn sample(outputs: Value) -> Value {
    json!({"state": "completed", "outputs": outputs})
}

fn state<'a>(trace: &'a Value, node: &str) -> &'a str {
    trace["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["nodeId"] == node)
        .unwrap_or_else(|| panic!("Missing trace node {node}"))["state"]
        .as_str()
        .unwrap()
}
