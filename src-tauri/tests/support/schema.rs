use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn remove_pipeline_automation_and_context_tables(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DROP TABLE model_comparison_markdown_sources;
         DROP TABLE saved_attachment_reuses;
         DROP TABLE git_reviewed_hunks;
         DROP TABLE workflow_agent_review_reuses;
         DROP TABLE workflow_delivery_attempts;
         DROP TABLE git_review_comments;
         DROP TABLE checkout_document_history;
         DROP TABLE workflow_github_source_syncs;
         DROP TABLE workflow_automatic_unit_actions;
         DROP TABLE workflow_integrated_check_actions;
         DROP TABLE workflow_automatic_delivery_outputs;
         DROP TABLE workflow_automatic_delivery_results;
         DROP TABLE workflow_github_destinations;
         DROP TABLE workflow_github_branches;
         DROP TABLE workflow_github_actions;
         DROP TABLE workflow_github_delivery_pins;
         DROP TABLE workflow_github_project_settings;
         DROP TABLE workflow_test_phase_actions;
         DROP TABLE workflow_automatic_preparations;
         DROP TABLE workflow_child_origins;
         DROP TABLE workflow_backlog_task_runs;
         DROP TABLE workflow_backlog_executions;
         DROP TABLE workflow_backlog_tasks;
         DROP TABLE workflow_backlog_epics;
         DROP TABLE workflow_automatic_agents;
         DROP TABLE workflow_automatic_runs;
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
