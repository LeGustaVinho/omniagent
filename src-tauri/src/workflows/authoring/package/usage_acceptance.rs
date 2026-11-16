use std::path::Path;

use crate::{
    jobs::AdmissionEnvironment,
    providers::SessionMode,
    runs::RunStatus,
    workflows::{
        BuiltinNode, ConditionPredicate, DeliveryRoles, Endpoint, ManualWorkflowLaunch,
        NodeConfiguration, NodeInputs, NodeInstance, Wire, WorkflowLaunchSource, WorkflowRevision,
        builtin_delivery_graph,
        tests::{approved_delivery_repository, submission},
    },
};
use serde_json::json;

use super::*;

const PRESET: &str = "route-by-note";
const SELECTED: &str = "selected-pipeline";
const OTHER: &str = "other-pipeline";
const GATE: &str = "gate";

fn predicate(path: &str) -> NodeConfiguration {
    NodeConfiguration::Condition {
        predicate: ConditionPredicate::Equals {
            pointer: "/evidence/relativePath".into(),
            value: json!(path),
        },
    }
}

fn member(key: &str, body: PackageArtifactBody) -> PackageMember {
    PackageMember {
        key: key.into(),
        name: key.into(),
        destination: PackageDestination::Project,
        base: None,
        body,
        references: BTreeMap::new(),
    }
}

fn preset(path: &str) -> PackageMember {
    member(
        PRESET,
        PackageArtifactBody::Preset {
            node_kind: BuiltinNode::Condition,
            configuration: predicate(path),
            editable_fields: vec!["predicate".into()],
        },
    )
}

fn wire(from: (&str, &str), to: (&str, &str)) -> Wire {
    Wire {
        from: Endpoint {
            node: from.0.into(),
            port: from.1.into(),
        },
        to: Endpoint {
            node: to.0.into(),
            port: to.1.into(),
        },
    }
}

fn pipeline(key: &str) -> PackageMember {
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    graph.name = key.into();
    graph
        .nodes
        .retain(|node| ["start", "context", "plan"].contains(&node.id.as_str()));
    graph.wires.retain(|wire| {
        matches!(wire.to.node.as_str(), "context" | "plan") && wire.to.port != "ready"
            || wire.to.node == "plan"
    });
    graph.nodes.push(NodeInstance {
        id: GATE.into(),
        kind: BuiltinNode::Condition,
        configuration: predicate("placeholder.md"),
    });
    graph.wires.extend([
        wire(("start", "start"), (GATE, "ready")),
        wire(("start", "document"), (GATE, "value")),
        wire((GATE, "matched"), ("context", "ready")),
    ]);
    let mut pipeline = member(key, PackageArtifactBody::Pipeline { graph });
    pipeline
        .references
        .insert(GATE.into(), PackageReference::Draft { key: PRESET.into() });
    pipeline
}

fn package(members: Vec<PackageMember>) -> WorkflowPackage {
    WorkflowPackage {
        name: "Shared source selection".into(),
        members,
    }
}

async fn publish(
    store: &Store,
    project: &str,
    draft: WorkflowPackageDraft,
) -> Result<WorkflowPackagePublication> {
    let WorkflowPackagePublishOutcome::Published { publication } = store
        .publish_reviewed_workflow_package(project.into(), draft.id, draft.revision)
        .await?
    else {
        anyhow::bail!("Expected an explicitly reviewed publication");
    };
    Ok(publication)
}

fn artifact<'a>(
    publication: &'a WorkflowPackagePublication,
    key: &str,
) -> Result<&'a PublishedPackageArtifact> {
    publication
        .members
        .iter()
        .find(|member| member.key == key)
        .map(|member| &member.artifact)
        .with_context(|| format!("Missing published member {key}"))
}

fn workflow(version: &PackageArtifactVersion) -> WorkflowRevision {
    WorkflowRevision {
        id: version.id.clone(),
        revision: version.revision.clone(),
    }
}

#[tokio::test]
async fn reviewing_a_shared_preset_upgrades_explicit_consumers_and_retains_historical_attempts()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("affected-uses.sqlite");
    let store = Store::open(database.clone())?;
    let repo = approved_delivery_repository(&store, directory.path()).await?;
    let project = &repo.project_id;
    let source = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let source = WorkflowLaunchSource {
        checkout_id: repo.checkout_id.clone(),
        revision_id: source.revision.id,
    };
    let launch = |version: &PackageArtifactVersion, request: &str| ManualWorkflowLaunch {
        project_id: project.clone(),
        workflow: workflow(version),
        source: source.clone(),
        request_id: request.into(),
        work_id: None,
    };

    let initial = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![
                preset("brief.md"),
                pipeline(SELECTED),
                pipeline(OTHER),
            ]),
        )
        .await?;
    let initial = publish(&store, project, initial).await?;
    let original_preset = artifact(&initial, PRESET)?;
    let original_pipeline = artifact(&initial, SELECTED)?;
    let other_pipeline = artifact(&initial, OTHER)?;
    let original_reference = ResolvedPackageReference::Published {
        version: original_preset.version.clone(),
    };
    assert_eq!(
        original_pipeline.references.get(GATE),
        Some(&original_reference)
    );
    assert_eq!(
        other_pipeline.references.get(GATE),
        Some(&original_reference)
    );

    let old_run = store
        .launch_workflow_manual(launch(&original_pipeline.version, "historical-run"))
        .await?;
    let old_graph = store
        .inspect_workflow_execution(project.clone(), old_run.id.clone())
        .await?
        .context("Missing original run")?;
    let context = old_graph
        .outputs
        .iter()
        .find(|receipt| receipt.artifact.node_instance_id == "context")
        .context("The original branch did not load its context")?;
    let context_id = context.artifact.id.clone();
    let mut request = submission(&repo, &old_run, "plan", None, SessionMode::Plan);
    request.artifacts = NodeInputs::from([("context".into(), context_id.clone())]);
    let job = store.submit_workflow_agent(request).await?;
    let attempt = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: true,
        })
        .await?
        .context("Missing historical planning attempt")?;
    let attempt_id = attempt.run.id;
    store.mark_agent_running(attempt_id.clone()).await?;
    store
        .finish_agent_attempt(
            attempt_id.clone(),
            RunStatus::Failed,
            Some("Fixture failure on original pipeline".into()),
        )
        .await?;

    let mut edited_preset = preset("different.md");
    edited_preset.base = Some(original_preset.version.clone());
    let mut upgraded = pipeline(SELECTED);
    upgraded.base = Some(original_pipeline.version.clone());
    let draft = store
        .create_workflow_package(
            project.clone(),
            None,
            package(vec![edited_preset, upgraded]),
        )
        .await?;
    let preview = serde_json::to_value(
        store
            .preview_workflow_package_revision(
                project.clone(),
                draft.id.clone(),
                draft.revision.clone(),
            )
            .await?,
    )?;
    let uses = preview["members"][0]["affectedUses"]
        .as_array()
        .context("Missing affected workflows")?;
    assert_eq!(uses.len(), 2);
    for consumer in [original_pipeline, other_pipeline] {
        let version = serde_json::to_value(&consumer.version)?;
        assert!(uses.iter().any(|entry| entry["consumer"] == version
            && entry["slot"] == GATE
            && entry["pinnedRevision"] == original_preset.version.revision));
    }
    assert_eq!(
        preview["members"][1]["dependencies"][GATE],
        json!({ "source":"draft", "key":PRESET })
    );
    assert_eq!(
        preview["members"][0]["changes"][0]["path"],
        "/body/configuration/predicate/value"
    );

    let updated = publish(&store, project, draft).await?;
    let new_preset = artifact(&updated, PRESET)?;
    let updated_pipeline = artifact(&updated, SELECTED)?;
    assert_eq!(new_preset.version.id, original_preset.version.id);
    assert_ne!(
        new_preset.version.revision,
        original_preset.version.revision
    );
    assert_eq!(updated_pipeline.version.id, original_pipeline.version.id);
    assert_ne!(
        updated_pipeline.version.revision,
        original_pipeline.version.revision
    );
    assert_eq!(
        updated_pipeline.references.get(GATE),
        Some(&ResolvedPackageReference::Published {
            version: new_preset.version.clone()
        })
    );
    let new_run = store
        .launch_workflow_manual(launch(&updated_pipeline.version, "selected-upgrade"))
        .await?;
    let unchanged_run = store
        .launch_workflow_manual(launch(&other_pipeline.version, "unselected-version"))
        .await?;
    drop(store);

    let reopened = Store::open(database)?;
    for expected in [
        original_preset,
        new_preset,
        original_pipeline,
        updated_pipeline,
        other_pipeline,
    ] {
        let retained = reopened
            .published_workflow_package_artifact(project.clone(), expected.version.clone())
            .await?;
        assert_eq!(retained.as_ref(), Some(expected));
    }
    for (run, expected, path, route) in [
        (&old_run, original_pipeline, "brief.md", "matched"),
        (&new_run, updated_pipeline, "different.md", "unmatched"),
        (&unchanged_run, other_pipeline, "brief.md", "matched"),
    ] {
        let inspection = reopened
            .inspect_workflow_execution(project.clone(), run.id.clone())
            .await?
            .context("Missing retained execution")?;
        assert_eq!(inspection.definition.version, workflow(&expected.version));
        let gate = inspection
            .definition
            .nodes
            .iter()
            .find(|node| node.instance.id == GATE)
            .context("Missing pinned gate")?;
        assert_eq!(gate.instance.configuration, predicate(path));
        let decision = inspection
            .control
            .iter()
            .find(|receipt| receipt.node_id == GATE)
            .context("Missing retained branch decision")?;
        assert_eq!(decision.selected_ports, [route]);
        if run.id == old_run.id {
            let job = inspection
                .jobs
                .iter()
                .find(|entry| entry.job.id == job.id)
                .context("Missing retained planning job")?;
            assert_eq!(
                job.inputs.as_ref().context("Missing retained job input")?["context"],
                context_id
            );
            assert_eq!(job.attempts[0].run_id, attempt_id);
            assert_eq!(job.attempts[0].status, RunStatus::Failed);
        }
    }
    Ok(())
}
