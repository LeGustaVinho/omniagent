use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use rusqlite::params;

use crate::{repository, store::Store};

use super::{
    DescriptionEditorConflict, DescriptionEditorSave, DescriptionEditorState, DescriptionOwner,
};

async fn project(store: &Store, path: &Path) -> Result<String> {
    fs::create_dir(path)?;
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .status()?
            .success()
    );
    Ok(repository::register(store, path.to_path_buf())
        .await?
        .project_id)
}

#[tokio::test]
async fn recovers_protected_epic_task_and_subtask_drafts_with_distinct_base_states() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let database = fixture.path().join("store.sqlite");
    let store = Store::open(database.clone())?;
    let project = project(&store, &fixture.path().join("repository")).await?;
    let epic = store
        .create_project_epic(project.clone(), "Epic".into())
        .await?;
    let task = store
        .create_project_task_in_epic(project.clone(), epic.id.clone(), "Task".into())
        .await?;
    let subtask = store
        .create_project_subtask(project.clone(), task.id.clone(), "Subtask".into())
        .await?;
    let epic_owner = DescriptionOwner::Epic(epic.id);
    let task_owner = DescriptionOwner::Work(task.id);
    let subtask_owner = DescriptionOwner::Work(subtask.id.clone());

    let empty = store
        .project_description_editor(project.clone(), epic_owner.clone())
        .await?;
    assert_eq!(
        empty,
        DescriptionEditorState {
            saved: None,
            draft: None
        }
    );
    let null = store
        .save_project_description(project.clone(), task_owner.clone(), None, None)
        .await?;
    let old = store
        .save_project_description(
            project.clone(),
            subtask_owner.clone(),
            None,
            Some("Original subtask text".into()),
        )
        .await?;
    store
        .ensure_project_checklist(project.clone(), subtask.id.clone())
        .await?;
    let checklist = store
        .append_project_checklist_item(
            project.clone(),
            subtask.id.clone(),
            "Workflow checkpoint".into(),
        )
        .await?;

    let mut expected = Vec::new();
    for (owner, base, content) in [
        (epic_owner, None, "Epic rascunho ☕"),
        (task_owner, Some(null), "Task rascunho 📝"),
        (subtask_owner, Some(old), "- [x] Markdown checkbox"),
    ] {
        let draft = store
            .save_project_description_draft(
                project.clone(),
                owner.clone(),
                base.as_ref().map(|base| base.id.clone()),
                None,
                content.into(),
            )
            .await?;
        assert_eq!(draft.base, base);
        let protected: Vec<u8> = store.execute({
            let project = project.clone();
            let revision = draft.revision.clone();
            move |db| {
                Ok(db.query_row(
                    "SELECT body FROM project_description_drafts WHERE project_id = ?1 AND draft_revision = ?2",
                    params![project, revision], |row| row.get(0),
                )?)
            }
        }).await?;
        assert!(
            !protected
                .windows(content.len())
                .any(|window| window == content.as_bytes())
        );
        expected.push((owner, base, draft));
    }
    drop(store);
    let store = Store::open(database)?;
    for (owner, base, draft) in expected {
        assert_eq!(
            store
                .project_description_editor(project.clone(), owner.clone())
                .await?,
            DescriptionEditorState {
                saved: base.clone(),
                draft: Some(draft.clone())
            }
        );
        let saved = store
            .save_project_description_editor(
                project.clone(),
                owner.clone(),
                base.as_ref().map(|base| base.id.clone()),
                Some(draft.revision),
                Some(draft.markdown.clone()),
            )
            .await?;
        let DescriptionEditorSave::Saved { revision } = saved else {
            bail!("the recovered description could not be saved: {saved:?}");
        };
        assert_eq!(revision.owner, owner);
        assert_eq!(
            revision.revision,
            base.as_ref().map_or(1, |base| base.revision + 1)
        );
        assert_eq!(revision.markdown.as_deref(), Some(draft.markdown.as_str()));
        assert_eq!(
            store
                .project_description_editor(project.clone(), owner)
                .await?,
            DescriptionEditorState {
                saved: Some(revision),
                draft: None
            }
        );
        if let Some(base) = base {
            assert_eq!(
                store
                    .project_description_revision(project.clone(), base.id.clone())
                    .await?,
                Some(base)
            );
        }
    }
    let workflow = store
        .project_checklist(project, subtask.id)
        .await?
        .context("missing workflow checklist")?;
    assert_eq!(workflow.items, [checklist]);
    Ok(())
}

#[tokio::test]
async fn protects_owner_scope_and_both_conflict_tokens_without_consuming_recovery() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let store = Store::open(fixture.path().join("store.sqlite"))?;
    let first_project = project(&store, &fixture.path().join("repository")).await?;
    let second_project = project(&store, &fixture.path().join("other")).await?;
    let task = store
        .create_project_task(first_project.clone(), "Task".into())
        .await?;
    let owner = DescriptionOwner::Work(task.id);
    let other_task = store
        .create_project_task(second_project.clone(), "Other task".into())
        .await?;
    let other_owner = DescriptionOwner::Work(other_task.id);
    let draft = store
        .save_project_description_draft(
            first_project.clone(),
            owner.clone(),
            None,
            None,
            "first edit".into(),
        )
        .await?;
    let later = store
        .save_project_description_draft(
            first_project.clone(),
            owner.clone(),
            None,
            Some(draft.revision.clone()),
            "latest edit".into(),
        )
        .await?;
    assert_ne!(draft.revision, later.revision);
    assert!(
        store
            .save_project_description_draft(
                first_project.clone(),
                owner.clone(),
                None,
                Some(draft.revision.clone()),
                "delayed update".into()
            )
            .await
            .is_err()
    );
    assert!(
        !store
            .discard_project_description_draft(
                first_project.clone(),
                owner.clone(),
                draft.revision.clone()
            )
            .await?
    );
    assert!(
        store
            .project_description_editor(second_project.clone(), owner.clone())
            .await
            .is_err()
    );
    assert!(
        !store
            .discard_project_description_draft(
                second_project.clone(),
                other_owner.clone(),
                later.revision.clone()
            )
            .await?
    );

    // The empty editor gained an explicit null revision while its draft stayed on an absent base.
    let current = store
        .save_project_description(first_project.clone(), owner.clone(), None, None)
        .await?;
    assert!(
        store
            .save_project_description_draft(
                second_project,
                other_owner,
                Some(current.id.clone()),
                None,
                "wrong project".into()
            )
            .await
            .is_err()
    );
    let epic = store
        .create_project_epic(first_project.clone(), "Same project".into())
        .await?;
    assert!(
        store
            .save_project_description_draft(
                first_project.clone(),
                DescriptionOwner::Epic(epic.id),
                Some(current.id.clone()),
                None,
                "wrong owner".into()
            )
            .await
            .is_err()
    );

    let expected_state = DescriptionEditorState {
        saved: Some(current.clone()),
        draft: Some(later.clone()),
    };
    let saved_conflict = store
        .save_project_description_editor(
            first_project.clone(),
            owner.clone(),
            None,
            Some(later.revision.clone()),
            Some("old base".into()),
        )
        .await?;
    assert_eq!(
        saved_conflict,
        DescriptionEditorSave::Conflict {
            reason: DescriptionEditorConflict::SavedRevisionChanged,
            state: expected_state.clone(),
        }
    );
    let draft_conflict = store
        .save_project_description_editor(
            first_project.clone(),
            owner.clone(),
            Some(current.id.clone()),
            Some(draft.revision),
            Some("old draft".into()),
        )
        .await?;
    assert_eq!(
        draft_conflict,
        DescriptionEditorSave::Conflict {
            reason: DescriptionEditorConflict::DraftRevisionChanged,
            state: expected_state.clone(),
        }
    );
    assert_eq!(
        store
            .project_description_editor(first_project.clone(), owner.clone())
            .await?,
        expected_state
    );

    let cleared = store
        .save_project_description_editor(
            first_project.clone(),
            owner.clone(),
            Some(current.id),
            Some(later.revision),
            None,
        )
        .await?;
    let DescriptionEditorSave::Saved { revision } = cleared else {
        bail!("could not save using current tokens: {cleared:?}");
    };
    assert_eq!(revision.revision, 1);
    assert_eq!(revision.markdown, None);
    assert!(
        store
            .project_description_editor(first_project, owner)
            .await?
            .draft
            .is_none()
    );
    Ok(())
}
