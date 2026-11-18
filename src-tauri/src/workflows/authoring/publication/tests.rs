use std::{fs, path::Path};

use serde_json::json;

use crate::{
    conversations::SessionScope,
    providers::{Provider, SessionMode},
    repository::ProjectCheckout,
    workflows::{
        BuiltinNode, ManualWorkflowLaunch, NodeConfiguration, NodeInstance, WorkflowGraph,
        WorkflowLaunchSource, tests::approved_delivery_repository,
    },
};

use super::super::{AuthoringDraft, tests::finish_request};
use super::*;

fn graph(name: &str) -> WorkflowGraph {
    WorkflowGraph {
        name: name.into(),
        nodes: vec![NodeInstance {
            id: "start".into(),
            kind: BuiltinNode::Start,
            configuration: NodeConfiguration::None,
        }],
        wires: vec![],
    }
}

async fn fixture(store: &Store, path: &Path) -> Result<(ProjectCheckout, String, SessionScope)> {
    let repo = approved_delivery_repository(store, path).await?;
    let conversation = store
        .create_conversation(repo.checkout_id.clone(), "Workflow publication".into())
        .await?;
    let executable = Path::new(&repo.checkout_root).join("codex.exe");
    fs::write(&executable, b"fixture")?;
    let scope = SessionScope {
        provider: Provider::Codex,
        profile_id: "author-fixture".into(),
        account_binding: None,
        checkout: repo.checkout_root.clone().into(),
        executable,
        cli_version: "fixture".into(),
        runner: "fixture-runner".into(),
        permission_revision: "read-only".into(),
        mode: SessionMode::ReadOnly,
        model: Some("author-model".into()),
        effort: None,
    };
    Ok((repo, conversation.id, scope))
}

async fn proposal(
    store: &Store,
    repo: &ProjectCheckout,
    draft: &AuthoringDraft,
    scope: &SessionScope,
    graph: &WorkflowGraph,
    brief: &str,
) -> Result<(AuthoringDraft, String)> {
    let run = finish_request(store, draft, scope, serde_json::to_string(graph)?, &[brief]).await?;
    Ok((
        store
            .inspect_workflow_authoring(repo.project_id.clone(), draft.id.clone())
            .await?,
        run,
    ))
}

#[tokio::test]
async fn reviewed_workflow_publication_preserves_stale_drafts_and_survives_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("protected.sqlite");
    let store = Store::open(db.clone())?;
    let (repo, conversation, scope) = fixture(&store, temp.path()).await?;
    let project = repo.project_id.clone();
    let original = store
        .save_workflow_revision(
            project.clone(),
            "existing-review".into(),
            graph("Existing published pipeline"),
        )
        .await?;
    let first = store
        .start_workflow_authoring(
            conversation.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            Some(original.clone()),
            "First review request".into(),
        )
        .await?;
    let second = store
        .start_workflow_authoring(
            conversation.clone(),
            repo.checkout_id.clone(),
            scope.clone(),
            Some(original.clone()),
            "Competing review request".into(),
        )
        .await?;
    let initial_draft = first
        .head
        .as_ref()
        .context("Expected base graph")?
        .id
        .clone();
    assert!(
        store
            .publish_reviewed_workflow(
                project.clone(),
                first.id.clone(),
                initial_draft.clone(),
                Some(original.clone())
            )
            .await
            .is_err()
    );
    let (first, source_run) = proposal(
        &store,
        &repo,
        &first,
        &scope,
        &graph("Agent proposed pipeline"),
        "First review request",
    )
    .await?;
    let (second, _) = proposal(
        &store,
        &repo,
        &second,
        &scope,
        &graph("Competing draft pipeline"),
        "Competing review request",
    )
    .await?;
    let agent_revision = first
        .head
        .as_ref()
        .context("Expected agent output")?
        .id
        .clone();
    let final_draft = store
        .save_workflow_authoring_canvas(
            project.clone(),
            first.id.clone(),
            agent_revision.clone(),
            graph("Reviewed pipeline"),
            Some(serde_json::from_value(
                json!({"layout": {"start": {"x": 25, "y": 50}}}),
            )?),
        )
        .await?;
    let reviewed_revision = final_draft
        .head
        .as_ref()
        .context("Expected reviewed draft")?
        .id
        .clone();
    assert!(
        store
            .publish_reviewed_workflow(
                project.clone(),
                first.id.clone(),
                reviewed_revision.clone(),
                None
            )
            .await
            .is_err()
    );
    assert!(
        store
            .publish_reviewed_workflow(
                project.clone(),
                first.id.clone(),
                agent_revision,
                Some(original.clone())
            )
            .await
            .is_err()
    );
    assert!(
        store
            .publish_reviewed_workflow(
                "other-project".into(),
                first.id.clone(),
                reviewed_revision.clone(),
                Some(original.clone())
            )
            .await
            .is_err()
    );
    let WorkflowPublicationOutcome::Published { publication } = store
        .publish_reviewed_workflow(
            project.clone(),
            final_draft.id.clone(),
            reviewed_revision.clone(),
            Some(original.clone()),
        )
        .await?
    else {
        anyhow::bail!("Expected a published workflow")
    };
    assert_eq!(
        publication.decision_source,
        PublicationDecision::DesktopUser
    );
    assert_eq!(publication.base, Some(original.clone()));
    assert_eq!(publication.reviewed_revision_id, reviewed_revision);
    assert_eq!(
        publication.originating_run_id.as_deref(),
        Some(source_run.as_str())
    );
    assert!(publication.originating_request_id.is_some());
    let published = store
        .workflow_definition(project.clone(), publication.workflow.clone())
        .await?
        .context("Missing published revision")?;
    assert_eq!(published.name, "Reviewed pipeline");
    assert_eq!(
        published.nodes[0].definition,
        final_draft.head.as_ref().unwrap().definitions["start"]
    );
    let competing_revision = second
        .head
        .as_ref()
        .context("Expected competing draft revision")?
        .id
        .clone();
    let WorkflowPublicationOutcome::StaleBase { current } = store
        .publish_reviewed_workflow(
            project.clone(),
            second.id.clone(),
            competing_revision.clone(),
            Some(original.clone()),
        )
        .await?
    else {
        anyhow::bail!("A stale base may not publish")
    };
    assert_eq!(current, published);
    let competing = store
        .inspect_workflow_authoring(project.clone(), second.id)
        .await?;
    assert_eq!(
        competing
            .head
            .context("Expected the unsuperseded draft")?
            .id,
        competing_revision
    );
    assert!(competing.publication.is_none());
    assert!(
        store
            .save_workflow_authoring_draft(
                project.clone(),
                first.id.clone(),
                reviewed_revision.clone(),
                graph("Invalid edit")
            )
            .await
            .is_err()
    );
    assert!(store.list_workflow_runs(project.clone()).await?.is_empty());
    drop(store);
    let store = Store::open(db)?;
    let restored = store
        .inspect_workflow_authoring(project.clone(), first.id.clone())
        .await?;
    assert_eq!(restored.publication, Some(publication.clone()));
    let WorkflowPublicationOutcome::Published {
        publication: replayed,
    } = store
        .publish_reviewed_workflow(
            project.clone(),
            first.id,
            reviewed_revision,
            Some(original.clone()),
        )
        .await?
    else {
        anyhow::bail!("Expected the durable receipt")
    };
    assert_eq!(replayed, publication);
    assert_eq!(
        store.workflow_authoring_bases(project.clone()).await?.len(),
        2
    );
    assert_eq!(
        store
            .workflow_definition(project.clone(), original)
            .await?
            .unwrap()
            .name,
        "Existing published pipeline"
    );
    let markdown = store
        .capture_repository_markdown(
            repo.checkout_id.clone(),
            Path::new(&repo.checkout_root).join("brief.md"),
        )
        .await?;
    let run = store
        .launch_workflow_manual(ManualWorkflowLaunch {
            project_id: project.clone(),
            workflow: publication.workflow.clone(),
            source: WorkflowLaunchSource {
                checkout_id: repo.checkout_id.clone(),
                revision_id: markdown.revision.id.clone(),
            },
            request_id: new_id(),
            work_id: None,
        })
        .await?;
    assert_eq!(run.version, publication.workflow);
    let launch = store
        .workflow_launch(project.clone(), run.id)
        .await?
        .context("Missing explicit launch receipt")?;
    assert_eq!(launch.version, publication.workflow);
    assert_eq!(launch.source, Some(markdown.revision));
    let followup = store
        .start_workflow_authoring(
            conversation,
            repo.checkout_id,
            scope,
            Some(publication.workflow),
            "Continue from published canvas".into(),
        )
        .await?;
    assert_eq!(
        serde_json::to_value(&followup.head.unwrap().layout)?,
        json!({"start": {"x": 25.0, "y": 50.0}})
    );
    Ok(())
}
