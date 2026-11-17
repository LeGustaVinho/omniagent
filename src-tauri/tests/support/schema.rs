use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn remove_context_menu_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP TABLE saved_attachment_reuses;
         DROP TABLE git_reviewed_hunks;
         DROP TABLE git_review_comments;
         DROP TABLE checkout_document_history;
         DROP TABLE conversation_library_organization;
         DROP TABLE conversation_library_sections;",
    )?;
    Ok(())
}

pub fn restore_question_view_before_structured_planning(db: &Connection) -> Result<()> {
    let (_, views) = include_str!("../../migrations/038-question-answers.sql")
        .split_once("CREATE VIEW open_agent_questions AS")
        .context("the previous open-question view is missing")?;
    let (previous, _) = views
        .split_once("CREATE VIEW ready_question_continuations AS")
        .context("the boundary of the previous open-question view is missing")?;
    db.execute_batch(&format!(
        "DROP VIEW open_agent_questions;
         DROP VIEW workflow_structured_planning_jobs;
         CREATE VIEW open_agent_questions AS{previous}"
    ))?;
    Ok(())
}
