use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    context_records::{
        ContextProvenance, ContextRecordContent, ContextRecordKind, ContextReviewDecision,
        ContextReviewStatus,
        assembly::{ContextVersionRef, DispatchRoleContext, RoleContextPolicy},
    },
    tasks::DescriptionOwner,
};

use super::{
    AcceptedUnit, ArtifactValue, AttemptWorktreeEvidence, CandidateSelection, CandidateTarget,
    ContextSnapshot, Delivery, EventSource, ProviderEvent, ProviderEventData, RunStatus, Store,
    TaskDeliveryEvidence, TaskWorkflowState, accept_candidate, approve_delivery_fixture_for_work,
    approved_delivery_repository, claim_fixture_job, commit, first_reviewer, fixture_admission, fs,
    git, implementation_submission, json, repository, review_candidate, verified, verify_candidate,
};
use anyhow::{Context, Result, ensure};

const SELECTED_TEXT: &str = "Included history canary\n";
const SELECTED_ADDITION: &str = "Selected untracked bytes\r\n";

#[tokio::test]
async fn completed_task_preserves_accepted_patch_and_dispatch_sources_after_restart() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let db = temp.path().join("historical-delivery.sqlite");
    let store = Store::open(db.clone())?;
    let repo = approved_delivery_repository(&store, temp.path()).await?;
    let note_path = std::path::Path::new(&repo.checkout_root).join("brief.md");
    let project = repo.project_id.clone();
    let task = store
        .create_project_task(project.clone(), "Inspect saved delivery".into())
        .await?
        .id;
    let owner = DescriptionOwner::Work(task.clone());
    let description = store
        .save_project_description(
            project.clone(),
            owner.clone(),
            None,
            Some("Original task description".into()),
        )
        .await?;
    let note = store
        .propose_context_record(
            project.clone(),
            Some(task.clone()),
            ContextRecordKind::Note,
            ContextRecordContent {
                title: "Original architecture note".into(),
                markdown: "Original context decision body".into(),
            },
            ContextProvenance::Operator,
        )
        .await?;
    store
        .review_context_revision(
            project.clone(),
            note.record_id.clone(),
            note.id.clone(),
            ContextReviewStatus::Proposed,
            ContextReviewDecision::Accept,
        )
        .await?;
    let fixture =
        approve_delivery_fixture_for_work(&store, temp.path(), repo, Some(task.clone())).await?;
    let [unit] = fixture.units.as_slice() else {
        anyhow::bail!("expected one delivered unit");
    };
    let worktree = store
        .live_managed_worktree(project.clone(), task.clone())
        .await?
        .context("missing managed checkout")?
        .worktree;
    let document = store
        .workflow_run_artifacts(project.clone(), fixture.run.id.clone())
        .await?
        .into_iter()
        .find_map(|artifact| match artifact.value {
            ArtifactValue::Markdown(revision) if artifact.node_instance_id == "start" => {
                Some(revision)
            }
            _ => None,
        })
        .context("the workflow's selected note is missing")?;
    let document_before = store
        .repository_markdown(document.checkout_id.clone(), document.id.clone())
        .await?
        .context("the selected note bytes are missing")?;

    let mut submission = implementation_submission(&fixture, &worktree, unit)?;
    submission.input.role_context = Some(RoleContextPolicy {
        revision_id: "historical-implementation-policy".into(),
        role: "implementer".into(),
        available_bytes: 8192,
        required: Vec::new(),
        explicit_links: vec![ContextVersionRef {
            record_id: note.record_id.clone(),
            revision_id: note.id.clone(),
        }],
    });
    let job = store.submit_workflow_agent(submission).await?;
    let writer = claim_fixture_job(&store, job, fixture_admission()).await?;
    let writer_id = writer.run.id.clone();
    store.mark_agent_running(writer_id.clone()).await?;
    ensure!(repository::capture_worktree_start(&store, writer_id.clone()).await?);
    let context = store
        .task_dispatch_context(writer_id.clone())
        .await?
        .context("task context is missing")?;
    let DispatchRoleContext::Assembled(role) = store
        .dispatch_role_context(writer_id.clone(), "Implement the historical fixture".into())
        .await?
    else {
        anyhow::bail!("expected configured role context");
    };
    let mut prompt = "Original implementation request".to_owned();
    context.append_to_prompt(&mut prompt)?;
    prompt.push_str(&role.selection.prompt_suffix()?);
    let frame = serde_json::to_vec(&json!({ "prompt": prompt }))?;
    let snapshot: ContextSnapshot = serde_json::from_value(json!({
        "version": 13, "contextId": writer.context_id, "scope": writer.scope,
        "submittedMessages": writer.input, "baseCommit": worktree.base_commit,
        "nativeHistory": { "mode": "fresh" }, "taskContext": context, "roleContext": role,
    }))?;
    let dispatch = store
        .prepare_dispatch(writer_id.clone(), snapshot, vec![frame.clone()])
        .await?;
    store
        .settle_dispatch(dispatch.clone(), Delivery::Submitted)
        .await?;
    fs::write(worktree.root.join("first.txt"), SELECTED_TEXT)?;
    fs::write(
        worktree.root.join("second.txt"),
        "Excluded history canary\n",
    )?;
    fs::write(
        worktree.root.join("accepted-addition.txt"),
        SELECTED_ADDITION,
    )?;
    repository::save_worktree_candidate(&store, writer_id.clone()).await?;
    let response = ProviderEventData::AssistantFinal {
        text: "Original implementation response".into(),
    };
    store
        .record_agent_observation(
            writer_id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: Default::default(),
                data: response.clone(),
            },
        )
        .await?;
    store
        .finish_agent_attempt(writer_id.clone(), RunStatus::Succeeded, None)
        .await?;
    let Some(AttemptWorktreeEvidence::Saved { artifact: capture }) = store
        .attempt_worktree_evidence(project.clone(), writer_id.clone())
        .await?
    else {
        anyhow::bail!("missing confirmed implementation capture");
    };
    let selected = store
        .select_worktree_candidate(
            CandidateTarget {
                project_id: project.clone(),
                task_id: task.clone(),
                unit_id: unit.unit.id.clone(),
                run_id: writer_id.clone(),
                capture_revision: capture.revision.clone(),
            },
            CandidateSelection {
                tracked_paths: Some(vec!["first.txt".into()]),
                untracked_paths: vec!["accepted-addition.txt".into()],
            },
        )
        .await?;
    let candidate = store
        .publish_workflow_candidate(
            project.clone(),
            fixture.run.id.clone(),
            "implement".into(),
            task.clone(),
            selected.id.clone(),
        )
        .await?;
    let review = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &candidate,
        &first_reviewer(),
    )
    .await?;
    let checks = verify_candidate(&store, &fixture, unit, &candidate).await?;
    let acceptance = accept_candidate(&store, &fixture, unit, &candidate, &review, &checks).await?;
    let other_review = review_candidate(
        &store,
        &fixture,
        &worktree,
        unit,
        &candidate,
        &first_reviewer(),
    )
    .await?;
    let other_acceptance =
        accept_candidate(&store, &fixture, unit, &candidate, &other_review, &checks).await?;
    assert_ne!(acceptance.id, other_acceptance.id);
    assert!(
        matches!((&acceptance.value, &other_acceptance.value), (ArtifactValue::Acceptance(a), ArtifactValue::Acceptance(b))
        if a.evidence.id == b.evidence.id)
    );
    let accepted = AcceptedUnit {
        candidate,
        acceptance,
    };
    let old_head = git(&worktree.root, &["rev-parse", "HEAD"])?;
    for (path, reviewed) in [
        ("first.txt", SELECTED_TEXT),
        ("accepted-addition.txt", SELECTED_ADDITION),
    ] {
        let path = worktree.root.join(path);
        fs::write(&path, "New worktree text after approval\n")?;
        let failure = commit(&store, &fixture, unit, &accepted, None)
            .await
            .unwrap_err();
        assert!(
            failure.to_string().contains("approved candidate is stale"),
            "{failure:#}"
        );
        assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, old_head);
        assert_eq!(
            fs::read_to_string(&path)?,
            "New worktree text after approval\n"
        );
        assert!(
            store
                .project_task_deliveries(project.clone(), task.clone())
                .await?
                .is_empty()
        );
        fs::write(&path, reviewed)?;
    }
    fs::write(
        worktree.root.join("second.txt"),
        "Unselected text edited after approval\n",
    )?;
    let receipt = verified(
        &store,
        &fixture,
        unit,
        commit(&store, &fixture, unit, &accepted, None).await?,
    )
    .await?;

    assert!(receipt.pending_changes_retained);
    assert_eq!(
        fs::read_to_string(worktree.root.join("second.txt"))?,
        "Unselected text edited after approval\n"
    );
    let reviewed_patch = store
        .verified_delivery_patch(project.clone(), task.clone(), receipt.id.clone())
        .await?
        .context("missing patch before switching tasks")?;
    let awaiting = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("missing task state before resolving excluded changes")?;
    assert_eq!(awaiting.state, TaskWorkflowState::InReview);
    assert!(awaiting.pending_repository_changes);
    git(
        &worktree.root,
        &[
            "restore",
            "--source=HEAD",
            "--staged",
            "--worktree",
            "--",
            "second.txt",
        ],
    )?;
    store
        .discard_pending_worktree_changes(
            project.clone(),
            worktree.checkout_id.clone(),
            task.clone(),
        )
        .await?;

    fs::write(note_path.clone(), "New live source note")?;
    let new_document = store
        .capture_repository_markdown(document.checkout_id.clone(), note_path)
        .await?;
    let new_description = store
        .save_project_description(
            project.clone(),
            owner,
            Some(description.id.clone()),
            Some("New task description".into()),
        )
        .await?;
    let new_note = store
        .propose_context_revision(
            project.clone(),
            note.record_id.clone(),
            note.id.clone(),
            ContextRecordContent {
                title: "New architecture note".into(),
                markdown: "New context decision body".into(),
            },
            ContextProvenance::Operator,
        )
        .await?;
    store
        .review_context_revision(
            project.clone(),
            note.record_id.clone(),
            new_note.id.clone(),
            ContextReviewStatus::Proposed,
            ContextReviewDecision::Accept,
        )
        .await?;

    drop(store);

    let store = Store::open(db)?;
    let state = store
        .project_task_delivery_state(project.clone(), task.clone())
        .await?
        .context("completed task missing after reopening")?;
    assert_eq!(state.state, TaskWorkflowState::Completed);
    let Some(delivery) = state.completion else {
        anyhow::bail!("task completion evidence missing");
    };
    assert!(
        matches!(delivery.evidence, TaskDeliveryEvidence::VerifiedCommit { ref workflow_run_id, ref unit_evidence_ids, ref completion_sha, .. }
        if workflow_run_id == &fixture.run.id && unit_evidence_ids == &[receipt.id.clone()] && completion_sha == &receipt.sha)
    );
    let reopened = store
        .verified_delivery_commit(project.clone(), task.clone(), receipt.id.clone())
        .await?
        .context("missing verified Git receipt")?;
    assert_eq!(reopened.branch, worktree.branch);
    assert_eq!(reopened.sha, receipt.sha);
    let patch = store
        .verified_delivery_patch(project.clone(), task.clone(), reopened.id.clone())
        .await?
        .context("missing accepted patch")?;
    assert_eq!(patch, reviewed_patch);
    assert_eq!(patch.candidate_id, selected.id);
    assert_eq!(patch.tree, reopened.tree);
    let diff = String::from_utf8(STANDARD.decode(patch.patch_base64)?)?;
    assert!(
        diff.contains("+Included history canary") && diff.contains("+Selected untracked bytes"),
        "{diff}"
    );
    assert_eq!(
        store
            .attempt_worktree_evidence(project.clone(), writer_id.clone())
            .await?,
        Some(AttemptWorktreeEvidence::Saved { artifact: capture }),
    );
    assert!(
        !diff.contains("second.txt") && !diff.contains("Excluded history canary"),
        "{diff}"
    );
    assert!(
        store
            .verified_delivery_patch(project.clone(), "another-task".into(), reopened.id.clone())
            .await?
            .is_none()
    );

    let saved = store
        .agent_dispatch(writer.conversation_id.clone(), writer_id.clone())
        .await?
        .context("original dispatch missing")?;
    assert_eq!(saved.id, dispatch);
    assert_eq!(saved.delivery, Delivery::Submitted);
    assert_eq!(saved.frames, vec![frame]);
    let history_description = saved
        .snapshot
        .task_context
        .context("missing historical task context")?
        .descriptions
        .task
        .context("missing historical task description")?;
    assert_eq!(history_description, description);
    assert_ne!(history_description.id, new_description.id);
    let historical_role = saved
        .snapshot
        .role_context
        .context("missing historical role context")?;
    assert_eq!(
        historical_role.selection.decisions[0].title,
        note.content.title
    );
    assert_eq!(
        historical_role.selection.blocks[0].markdown,
        note.content.markdown
    );
    assert_ne!(
        historical_role.selection.blocks[0].markdown,
        new_note.content.markdown
    );
    let orchestration = store
        .run_orchestration(
            project.clone(),
            writer_id.clone(),
            Some(writer.conversation_id.clone()),
        )
        .await?
        .context("missing implementation inspector")?;
    let orchestration = serde_json::to_value(orchestration)?;
    assert_eq!(
        orchestration["taskDescriptions"]["task"]["id"],
        description.id
    );
    assert_eq!(
        orchestration["context"]["decisions"][0]["revision"]["version"]["revisionId"],
        note.id
    );
    assert_eq!(orchestration["document"]["revisionId"], document.id);
    assert_ne!(document.id, new_document.revision.id);
    assert_eq!(
        store
            .repository_markdown(document.checkout_id, document.id)
            .await?,
        Some(document_before)
    );
    assert!(
        store
            .agent_observations(writer.conversation_id, writer_id, 0)
            .await?
            .iter()
            .any(|event| event.source == EventSource::Provider && event.event.data == response)
    );

    let artifacts = store
        .workflow_run_artifacts(project.clone(), fixture.run.id.clone())
        .await?;
    let accepted_review = artifacts
        .iter()
        .find_map(|artifact| match &artifact.value {
            ArtifactValue::Acceptance(accepted)
                if accepted.evidence.id == reopened.acceptance_id
                    && reopened.acceptance_artifact_id.as_ref() == Some(&artifact.id) =>
            {
                Some(&accepted.review_artifact_id)
            }
            _ => None,
        })
        .context("acceptance review link missing after restart")?;
    assert_eq!(accepted_review, &review.id);
    let ArtifactValue::Review { run_id } = review.value else {
        anyhow::bail!("review has wrong evidence type");
    };
    let review_run = store
        .project_task_runs(project, task)
        .await?
        .into_iter()
        .find(|attempt| attempt.run_id == run_id)
        .context("review history missing")?;
    let review_snapshot = store
        .agent_dispatch(review_run.conversation_id, run_id)
        .await?
        .context("missing reviewed checkout evidence")?;
    let reviewed_checkout = review_snapshot
        .snapshot
        .scoped_launch
        .and_then(|scoped| scoped.candidate_checkout)
        .context("missing materialized candidate evidence")?;
    assert_eq!(reviewed_checkout.candidate_id, selected.id);
    assert_eq!(reviewed_checkout.tree, receipt.tree);
    Ok(())
}
