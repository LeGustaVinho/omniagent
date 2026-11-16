use anyhow::{Context, Result};
use rusqlite::Connection;

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
