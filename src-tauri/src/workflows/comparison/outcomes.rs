use anyhow::Result;
use rusqlite::params;
use serde::Serialize;

use crate::{
    dispatch::read_background_dispatch_result, jobs::FailureObservation, protection,
    providers::ProviderEventData, store::Store,
};

use super::evidence::provider_events;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonOutcome {
    job_id: String,
    run_id: String,
    reported_model: Option<String>,
    duration_seconds: Option<i64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    reasoning: Vec<ReasoningEntry>,
    response: Option<String>,
    failure: Option<FailureObservation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningEntry {
    text: String,
    summary: bool,
}

impl Store {
    pub async fn model_comparison_outcomes(
        &self,
        project: String,
        group: String,
    ) -> Result<Vec<ComparisonOutcome>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let mut query = tx.prepare(
                "SELECT c.job_id, a.run_id, g.source_checkout_id, g.prompt, r.status,
                        terminal_turn.opened_at, terminal_turn.closed_at, agent.reported_model, session.provider_session_id,
                        agent.provider_turn_id, failure.report,
                        EXISTS (SELECT 1 FROM context_snapshots s JOIN dispatches d ON d.snapshot_id = s.id
                                JOIN history_deleted_dispatch_payloads gone ON gone.dispatch_id = d.id
                                WHERE s.run_id = a.run_id AND d.ordinal = 1)
                 FROM model_comparison_groups g JOIN model_comparison_candidates c ON c.group_id = g.id
                 JOIN job_attempts a ON a.job_id = c.job_id AND a.ordinal = (SELECT MAX(newest.ordinal) FROM job_attempts newest WHERE newest.job_id = c.job_id)
                 JOIN runs r ON r.id = a.run_id AND r.status IN ('succeeded','failed','interrupted','cancelled')
                 JOIN agent_attempts agent ON agent.run_id = a.run_id
                 JOIN turns terminal_turn ON terminal_turn.id = agent.turn_id
                 JOIN agent_sessions session ON session.id = agent.agent_session_id
                 LEFT JOIN job_failures failure ON failure.run_id = r.id
                 WHERE g.project_id = ?1 AND g.id = ?2 ORDER BY c.position",
            )?;
            let mut rows = query.query(params![project, group])?;
            let mut outcomes = Vec::new();
            while let Some(row) = rows.next()? {
                let job_id: String = row.get(0)?;
                let run_id: String = row.get(1)?;
                let opened: i64 = row.get(5)?;
                let closed: Option<i64> = row.get(6)?;
                let session: Option<String> = row.get(8)?;
                let turn: Option<String> = row.get(9)?;
                let failure: Option<String> = row.get(10)?;
                let deleted: bool = row.get(11)?;
                let response = if row.get::<_, String>(4)? == "succeeded" && !deleted {
                    let checkout: String = row.get(2)?;
                    let prompt: Vec<u8> = row.get(3)?;
                    read_background_dispatch_result(&tx, &run_id, &job_id, &checkout, &String::from_utf8(protection::unprotect(&prompt)?)?)
                        .ok().map(|(_, original)| original)
                } else { None };
                let mut result = ComparisonOutcome {
                    job_id, run_id: run_id.clone(), reported_model: row.get(7)?,
                    duration_seconds: closed.map(|ended| ended.saturating_sub(opened)),
                    input_tokens: None, output_tokens: None, cached_input_tokens: None, reasoning_tokens: None,
                    reasoning: Vec::new(), response, failure: failure.as_deref().map(serde_json::from_str).transpose()?,
                };
                if !deleted {
                    for event in provider_events(&tx, &run_id, session.as_deref(), turn.as_deref())? {
                        match event {
                            ProviderEventData::Usage { input, output, cached_input } => {
                                result.input_tokens = input.or(result.input_tokens);
                                result.output_tokens = output.or(result.output_tokens);
                                result.cached_input_tokens = cached_input.or(result.cached_input_tokens);
                            }
                            ProviderEventData::ReasoningUsage { tokens } => result.reasoning_tokens = Some(tokens),
                            ProviderEventData::Reasoning { text, summary } => result.reasoning.push(ReasoningEntry { text, summary }),
                            _ => {}
                        }
                    }
                }
                outcomes.push(result);
            }
            Ok(outcomes)
        }).await
    }
}
