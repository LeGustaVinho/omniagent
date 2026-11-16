use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    dispatch::{ContextSnapshot, belongs_to_dispatch},
    protection,
    providers::{ProviderEvent, ProviderEventData, configuration::InstructionSource},
    store::Store,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonEnvironment {
    job_id: String,
    dispatch_id: String,
    instructions: Vec<ComparedInstruction>,
    reported_tools: Vec<String>,
    observed_tools: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ComparedInstruction {
    #[serde(flatten)]
    source: InstructionSource,
    observed_read: bool,
}

impl Store {
    pub async fn model_comparison_environments(
        &self,
        project: String,
        group: String,
    ) -> Result<Vec<ComparisonEnvironment>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let mut query = tx.prepare(
                "SELECT c.job_id, a.run_id, dispatch.id, snap.payload, session.provider_session_id, agent.provider_turn_id
                 FROM model_comparison_groups g JOIN model_comparison_candidates c ON c.group_id = g.id
                 JOIN job_attempts a ON a.job_id = c.job_id AND a.ordinal = (SELECT MAX(newest.ordinal) FROM job_attempts newest WHERE newest.job_id = c.job_id)
                 JOIN agent_attempts agent ON agent.run_id = a.run_id
                 JOIN agent_sessions session ON session.id = agent.agent_session_id
                 JOIN context_snapshots snap ON snap.run_id = a.run_id
                 JOIN dispatches dispatch ON dispatch.snapshot_id = snap.id AND dispatch.ordinal = 1 AND dispatch.delivery = 'submitted'
                 WHERE g.project_id = ?1 AND g.id = ?2 ORDER BY c.position",
            )?;
            let rows = query.query_map(params![project, group], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, Option<String>>(5)?)))?;
            rows.map(|row| {
                let (job_id, run, dispatch_id, snapshot, session, turn) = row?;
                let snapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
                let observations = provider_events(&tx, &run, session.as_deref(), turn.as_deref())?;
                Ok(environment(job_id, dispatch_id, snapshot, &observations))
            }).collect()
        }).await
    }
}

pub(super) fn provider_events(
    db: &Connection,
    run: &str,
    session: Option<&str>,
    turn: Option<&str>,
) -> Result<Vec<ProviderEventData>> {
    let mut query = db.prepare(
        "SELECT payload FROM agent_events WHERE run_id = ?1 AND source = 'provider' ORDER BY seq",
    )?;
    let rows = query.query_map([run], |row| row.get::<_, Vec<u8>>(0))?;
    let mut observations = Vec::new();
    for payload in rows {
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload?)?)?;
        if belongs_to_dispatch(&event.location, session, turn) {
            observations.push(event.data);
        }
    }
    Ok(observations)
}

fn environment(
    job_id: String,
    dispatch_id: String,
    snapshot: ContextSnapshot,
    observations: &[ProviderEventData],
) -> ComparisonEnvironment {
    let cwd = snapshot
        .instructions
        .as_ref()
        .map(|inventory| inventory.working_directory.clone())
        .unwrap_or_else(|| snapshot.scope.checkout.clone());
    let mut reads = BTreeSet::new();
    let mut reported_tools = BTreeSet::new();
    let mut observed_tools = BTreeSet::new();
    for event in observations {
        match event {
            ProviderEventData::SessionToolsReported { tools } => {
                reported_tools.extend(tools.iter().cloned());
            }
            ProviderEventData::ContextRead { path, tool } => {
                let provider_path = Path::new(path);
                let absolute = if provider_path.is_absolute() {
                    PathBuf::from(provider_path)
                } else {
                    cwd.join(provider_path)
                };
                reads.insert(absolute);
                observed_tools.insert(tool.clone());
            }
            ProviderEventData::Tool { name, .. } => {
                observed_tools.insert(name.clone());
            }
            _ => {}
        }
    }
    let instructions = snapshot
        .instructions
        .into_iter()
        .flat_map(|inventory| inventory.sources)
        .map(|mut source| {
            let observed_read = reads.iter().any(|path| {
                path.as_os_str()
                    .eq_ignore_ascii_case(source.path.as_os_str())
            });
            if let Ok(relative) = source.path.strip_prefix(&cwd) {
                source.path = relative.into();
            }
            ComparedInstruction {
                source,
                observed_read,
            }
        })
        .collect();
    ComparisonEnvironment {
        job_id,
        dispatch_id,
        instructions,
        reported_tools: reported_tools.into_iter().collect(),
        observed_tools: observed_tools.into_iter().collect(),
    }
}
