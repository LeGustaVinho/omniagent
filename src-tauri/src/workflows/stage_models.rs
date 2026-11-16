use std::path::Path;

use super::*;
use crate::{
    jobs::{AdmissionEnvironment, JobRole},
    model_catalog::{MODEL_CATALOG, ObservedModel},
    providers::{Provider, SessionMode},
    runs::RunStatus,
    store::Store,
    workflows::role_templates::{
        ConversationTemplate, SaveConversationTemplate, TemplateReviewPolicy,
    },
};

#[tokio::test]
async fn template_stage_and_comparison_retain_the_exact_model_and_catalog_reference() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let database = dir.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let repo = tests::approved_delivery_repository(&store, dir.path()).await?;
    let project = repo.project_id.clone();
    let profile = "fixture";
    let inventory = ["gpt-6-astra", "gpt-6.1-sol"].map(|id| ObservedModel {
        id: id.into(),
        canonical_id: Some(id.into()),
        name: MODEL_CATALOG
            .lookup(Provider::Codex, id)
            .unwrap()
            .name
            .clone(),
        selectable: true,
        unavailable_reason: None,
    });
    let models = MODEL_CATALOG.project_options(Provider::Codex, profile, &inventory);
    let chosen = models[0].selection.clone();
    let alternate = models[1].selection.clone();
    assert!(models.iter().all(|model| model.rating.is_some()));
    let template = ConversationTemplate {
        name: "Reusable planner".into(),
        role: JobRole::Pipeline,
        model: chosen.clone(),
        effort: None,
        instructions: "Read the pinned inputs.".into(),
        tool_scope: SessionMode::ReadOnly,
        completion_contract: "Save the result.".into(),
        review_policy: TemplateReviewPolicy::Independent,
        requirements: Default::default(),
    };
    let SaveConversationTemplate::Saved { detail } = store
        .save_conversation_template(project.clone(), None, template)
        .await?
    else {
        anyhow::bail!("The template was not saved");
    };
    let template = detail.revision;

    let legacy_json = r#"{"kind":"agent_role","role":"planner"}"#;
    let unconfigured: NodeConfiguration = serde_json::from_str(legacy_json)?;
    assert_eq!(serde_json::to_string(&unconfigured)?, legacy_json);
    let mut graph = builtin_delivery_graph(DeliveryRoles::default());
    let legacy = store
        .save_workflow_revision(project.clone(), "planner".into(), graph.clone())
        .await?;
    let stage = &mut graph
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan")
        .unwrap()
        .configuration;
    let NodeConfiguration::AgentRole { model, .. } = stage else {
        unreachable!()
    };
    *model = Some(chosen.clone());
    let selected = store
        .save_workflow_revision(project.clone(), legacy.id.clone(), graph)
        .await?;
    assert_ne!(selected.revision, legacy.revision);
    let markdown = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: selected.clone(),
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: markdown.revision.id,
            },
            request_id: "pinned-planner-launch".into(),
            work_id: None,
        })
        .await?;
    let context = store
        .current_workflow_host_outputs(project.clone(), run.id.clone())
        .await?["context"]
        .clone();
    let mut incorrect = tests::submission(&repo, &run, "plan", None, SessionMode::Plan);
    incorrect.artifacts = NodeInputs::from([("context".into(), context.clone())]);
    incorrect.scope.model = Some(alternate.model_id.clone());
    assert!(
        store
            .submit_workflow_agent(incorrect)
            .await
            .unwrap_err()
            .to_string()
            .contains("pinned stage override")
    );

    let mut submission = tests::submission(&repo, &run, "plan", None, SessionMode::Plan);
    submission.scope.model = Some(chosen.model_id.clone());
    submission.artifacts = NodeInputs::from([("context".into(), context)]);
    let mut source_scope = submission.scope.clone();
    store.submit_workflow_agent(submission).await?;
    let staged = store
        .claim_next_job(AdmissionEnvironment {
            runner: "fixture-runner".into(),
            bridge_available: false,
        })
        .await?
        .context("The selected stage did not reach scheduler admission")?;
    assert_eq!(
        staged.scope.model.as_deref(),
        Some(chosen.model_id.as_str())
    );
    assert_eq!(staged.scope.profile_id, chosen.profile_id);
    store
        .finish_agent_attempt(staged.run.id, RunStatus::Cancelled, None)
        .await?;

    source_scope.mode = SessionMode::ReadOnly;
    let candidates = [models[1].clone(), models[0].clone()]
        .into_iter()
        .map(|option| {
            let mut scope = source_scope.clone();
            scope.model = Some(option.selection.model_id.clone());
            (scope, Some(option))
        })
        .collect();
    let comparison = store
        .start_model_comparison_with_options(
            project.clone(),
            repo.checkout_id.clone(),
            store
                .comparison_source_commit(project.clone(), repo.checkout_id.clone())
                .await?
                .unwrap(),
            "Compare the saved model options.".into(),
            candidates,
            dir.path().join("comparisons"),
        )
        .await?;
    drop(store);

    let store = Store::open(database)?;
    let loaded_template = store
        .conversation_template_revision(
            project.clone(),
            template.template_id,
            template.version.revision,
        )
        .await?
        .revision
        .template;
    assert_eq!(loaded_template.model, chosen);
    let pinned = store
        .pinned_workflow_node(project.clone(), run.id, "plan".into())
        .await?
        .unwrap();
    assert!(
        matches!(pinned.instance.configuration, NodeConfiguration::AgentRole { model: Some(model), .. } if model == chosen)
    );
    let legacy_graph = store
        .workflow_definition(project.clone(), legacy)
        .await?
        .unwrap();
    assert!(matches!(
        &legacy_graph
            .nodes
            .iter()
            .find(|node| node.instance.id == "plan")
            .unwrap()
            .instance
            .configuration,
        NodeConfiguration::AgentRole { model: None, .. }
    ));
    let comparisons = store.model_comparisons(project).await?;
    assert_eq!(comparisons[0].id, comparison.id);
    let actual = &comparisons[0].candidates;
    assert_eq!(
        actual
            .iter()
            .map(|candidate| &candidate.selection)
            .collect::<Vec<_>>(),
        [&alternate, &chosen]
    );
    for (candidate, expected) in actual.iter().zip(models.iter().rev()) {
        let saved = candidate
            .model_reference
            .as_ref()
            .context("The comparison lost the selected catalog option")?;
        assert_eq!(saved.rating, expected.rating);
        assert_eq!(saved.name, expected.name);
        assert_eq!(saved.catalog_version, expected.catalog_version);
    }
    Ok(())
}
