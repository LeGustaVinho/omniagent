use crate::dispatch::{HistoryDeletionMode, HistoryDeletionTarget};

use super::*;

#[tokio::test]
async fn deletes_recorded_workflow_diff_and_derivatives_without_erasing_verified_git() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("diff-history.sqlite");
    let store = Store::open(database.clone())?;
    let fixture = approved_delivery_fixture(&store, temporary.path()).await?;
    let unit = fixture
        .units
        .first()
        .context("missing first logical unit")?;
    let worktree = store
        .live_managed_worktree(fixture.project.clone(), unit.task_id.clone())
        .await?
        .context("approved worktree is unavailable")?
        .worktree;
    let accepted = capture_and_review(&store, &fixture, &worktree, unit, "first.txt").await?;
    let ArtifactValue::Candidate(candidate) = &accepted.candidate.value else {
        anyhow::bail!("missing selected candidate");
    };
    let run = candidate.target.run_id.clone();
    let candidate_id = candidate.id.clone();
    let evidence = verified(
        &store,
        &fixture,
        unit,
        commit(&store, &fixture, unit, &accepted, None).await?,
    )
    .await?;
    let attempt = store
        .git_saved_attempts(fixture.project.clone())
        .await?
        .into_iter()
        .find(|attempt| attempt.run_id == run)
        .context("recorded diff was not listed")?;
    let target = HistoryDeletionTarget::AttemptDiff {
        conversation_id: attempt.conversation_id,
        run_id: run.clone(),
    };
    assert!(
        store
            .preview_conversation_history_deletion(
                HistoryDeletionTarget::AttemptDiff {
                    conversation_id: "different conversation".into(),
                    run_id: run.clone()
                },
                HistoryDeletionMode::Selected,
            )
            .await
            .is_err()
    );

    let preview = store
        .preview_conversation_history_deletion(target.clone(), HistoryDeletionMode::Selected)
        .await?;
    assert!(preview.attempt_diff_ids.contains(&run));
    assert!(preview.candidate_ids.is_empty());
    store
        .confirm_conversation_history_deletion(
            target.clone(),
            HistoryDeletionMode::Selected,
            preview.revision,
        )
        .await?;
    assert!(matches!(
        store.attempt_worktree_evidence(fixture.project.clone(), run.clone()).await?,
        Some(AttemptWorktreeEvidence::Unavailable { reason }) if reason.contains("removed from history")
    ));
    assert!(
        store
            .selected_candidate(
                fixture.project.clone(),
                unit.task_id.clone(),
                candidate_id.clone()
            )
            .await?
            .is_some()
    );

    let preview = store
        .preview_conversation_history_deletion(
            target.clone(),
            HistoryDeletionMode::WithDerivedCopies,
        )
        .await?;
    assert!(preview.candidate_ids.contains(&candidate_id));
    store
        .confirm_conversation_history_deletion(
            target,
            HistoryDeletionMode::WithDerivedCopies,
            preview.revision,
        )
        .await?;
    drop(store);
    let store = Store::open(database)?;
    assert!(
        store
            .selected_candidate(fixture.project.clone(), unit.task_id.clone(), candidate_id)
            .await?
            .is_none()
    );
    let saved = store.git_saved_attempts(fixture.project.clone()).await?;
    assert!(
        saved
            .iter()
            .any(|attempt| attempt.run_id == run && attempt.history_deleted)
    );
    assert_eq!(git(&worktree.root, &["rev-parse", "HEAD"])?, evidence.sha);
    let history = store
        .verified_delivery_commit(
            fixture.project.clone(),
            unit.task_id.clone(),
            evidence.id.clone(),
        )
        .await?;
    assert_eq!(
        history.context("lost verified Git receipt")?.sha,
        evidence.sha
    );
    Ok(())
}
