use std::collections::HashSet;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{dispatch::read_background_dispatch_result, protection, store::Store};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonSelection {
    pub group_id: String,
    pub run_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonInput {
    pub group_id: String,
    pub job_id: String,
    pub run_id: String,
    pub dispatch_id: String,
    pub response_revision_id: String,
}

pub(crate) struct SelectedComparisonInputs {
    pub(crate) inputs: Vec<ComparisonInput>,
    pub(crate) context: String,
}

pub(crate) fn select_inputs(
    db: &Connection,
    project: &str,
    checkout: &str,
    selection: &ComparisonSelection,
) -> Result<SelectedComparisonInputs> {
    let mut remaining = selection.run_ids.iter().collect::<HashSet<_>>();
    ensure!(
        !remaining.is_empty() && remaining.len() == selection.run_ids.len(),
        "Select distinct comparison responses"
    );
    let (prompt, commit, tree): (Vec<u8>, String, String) = db.query_row(
        "SELECT prompt, commit_oid, tree_oid FROM model_comparison_groups WHERE id = ?1 AND project_id = ?2 AND source_checkout_id = ?3",
        params![selection.group_id, project, checkout], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?.context("The comparison is outside this project or checkout")?;
    let prompt = String::from_utf8(protection::unprotect(&prompt)?)?;
    let mut query = db.prepare(
        "SELECT c.job_id, a.run_id, d.id FROM model_comparison_candidates c
         JOIN jobs j ON j.id = c.job_id AND j.project_id = ?2
         JOIN job_attempts a ON a.job_id = c.job_id
         JOIN runs r ON r.id = a.run_id AND r.checkout_id = ?3 AND r.status = 'succeeded'
         JOIN context_snapshots s ON s.run_id = a.run_id
         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1 AND d.delivery = 'submitted'
         WHERE c.group_id = ?1 AND NOT EXISTS (SELECT 1 FROM history_deleted_dispatch_payloads gone WHERE gone.dispatch_id = d.id)
         ORDER BY c.position, a.ordinal",
    )?;
    let mut rows = query.query(params![selection.group_id, project, checkout])?;
    let mut inputs = Vec::new();
    let mut responses = Vec::new();
    let mut jobs = HashSet::new();
    while let Some(row) = rows.next()? {
        let run_id: String = row.get(1)?;
        if !remaining.remove(&run_id) {
            continue;
        }
        let job_id: String = row.get(0)?;
        ensure!(
            jobs.insert(job_id.clone()),
            "Select one response per comparison candidate"
        );
        let (result, response) =
            read_background_dispatch_result(db, &run_id, &job_id, checkout, &prompt)?;
        inputs.push(ComparisonInput {
            group_id: selection.group_id.clone(),
            job_id,
            run_id,
            dispatch_id: row.get(2)?,
            response_revision_id: result.output_revision_id.clone(),
        });
        responses.push(serde_json::json!({"run": result, "response": response}));
    }
    ensure!(
        remaining.is_empty(),
        "A selected comparison response is missing, unsuccessful, or outside this group"
    );
    let context = serde_json::to_string(
        &serde_json::json!({"groupId":selection.group_id,"commit":commit,"tree":tree,"request":prompt,"responses":responses}),
    )?;
    Ok(SelectedComparisonInputs { inputs, context })
}

pub(crate) fn bind_inputs(db: &Connection, job: &str, inputs: &[ComparisonInput]) -> Result<()> {
    for (position, input) in inputs.iter().enumerate() {
        db.execute(
            "INSERT INTO model_comparison_inputs VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                job,
                i64::try_from(position)?,
                input.group_id,
                input.job_id,
                input.run_id,
                input.dispatch_id,
                input.response_revision_id
            ],
        )?;
    }
    Ok(())
}

pub(super) fn load_inputs(db: &Connection, job: &str) -> Result<Vec<ComparisonInput>> {
    let mut query = db.prepare(
        "SELECT group_id, source_job_id, source_run_id, source_dispatch_id, response_revision_id
         FROM model_comparison_inputs WHERE job_id = ?1 ORDER BY position",
    )?;
    Ok(query
        .query_map([job], |row| {
            Ok(ComparisonInput {
                group_id: row.get(0)?,
                job_id: row.get(1)?,
                run_id: row.get(2)?,
                dispatch_id: row.get(3)?,
                response_revision_id: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn dispatch_sources_available(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row("SELECT NOT EXISTS (SELECT 1 FROM job_attempts a JOIN model_comparison_inputs input ON input.job_id = a.job_id
        LEFT JOIN message_revisions r ON r.id = input.response_revision_id
        LEFT JOIN history_deleted_revisions removed ON removed.revision_id = input.response_revision_id
        LEFT JOIN history_deleted_dispatch_payloads gone ON gone.dispatch_id = input.source_dispatch_id
        WHERE a.run_id = ?1 AND (gone.dispatch_id IS NOT NULL OR removed.revision_id IS NOT NULL OR r.id IS NULL))", [run], |row| row.get(0))?)
}

impl Store {
    pub async fn model_comparison_job_inputs(
        &self,
        project: String,
        job: String,
    ) -> Result<Vec<ComparisonInput>> {
        self.execute(move |db| {
            ensure!(
                db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ?1 AND project_id = ?2)",
                    params![job, project],
                    |row| row.get::<_, bool>(0)
                )?,
                "The selected job is unavailable in this project"
            );
            load_inputs(db, &job)
        })
        .await
    }
}
