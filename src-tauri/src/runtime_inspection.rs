use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    conversations::SessionScope,
    diagnostics,
    dispatch::EventSource,
    jobs::{AdmissionEnvironment, InspectionPolicy, InspectionQueue, load_inspection_queue},
    mcp_bridge::{RunScope, read_active_scope},
    protection,
    providers::{
        Capability, Provider, ProviderEvent, ProviderEventData, SessionMode,
        capabilities::{CapabilityAssessment, CapabilityStatus, CapabilitySubject, revision},
    },
    runs::{RunRecord, load_run},
    store::Store,
    tasks::load_project_task,
};

const DIAGNOSTIC_LIMIT: usize = 4;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeInspection {
    version: u16,
    build: BuildIdentity,
    run: RunRecord,
    profile: ProfileSummary,
    policy: Option<InspectionPolicy>,
    settings_revision: String,
    capabilities: Option<CapabilitySummary>,
    task: Option<TaskSummary>,
    queue: InspectionQueue,
    diagnostics: Vec<DiagnosticSummary>,
    more_diagnostics: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildIdentity {
    version: String,
    target: String,
    profile: String,
    source_commit: Option<String>,
    source_modified: Option<bool>,
}

impl BuildIdentity {
    fn running() -> Self {
        let commit = env!("OMNIAGENT_BUILD_COMMIT");
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            target: env!("OMNIAGENT_BUILD_TARGET").into(),
            profile: env!("OMNIAGENT_BUILD_PROFILE").into(),
            source_commit: (!commit.is_empty()).then(|| commit.into()),
            source_modified: env!("OMNIAGENT_BUILD_DIRTY").parse().ok(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileSummary {
    provider: Provider,
    mode: SessionMode,
    runner: String,
    cli_version: String,
    permission_revision: String,
    requested_model: Option<String>,
    reported_model: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskSummary {
    id: String,
    title: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapabilitySummary {
    configuration_revision: String,
    protocol: BTreeMap<Capability, CapabilityStatus>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticSummary {
    sequence: String,
    source: EventSource,
    kind: DiagnosticKind,
    message: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticKind {
    Protocol,
    Error,
}

fn bounded_preview(text: &str, limit: usize) -> String {
    diagnostics::preview(text)
        .chars()
        .take(limit)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

pub(crate) fn save_capability_summary(
    db: &Connection,
    run: &str,
    assessment: &CapabilityAssessment,
) -> Result<()> {
    let summary = CapabilitySummary {
        configuration_revision: revision(&assessment.scope.configuration)?,
        protocol: assessment
            .entries
            .iter()
            .filter_map(|entry| match entry.descriptor.subject {
                CapabilitySubject::Protocol { capability } => Some((capability, entry.status())),
                _ => None,
            })
            .collect(),
    };
    db.execute(
        "INSERT INTO agent_capability_summaries (run_id, summary) VALUES (?1, ?2)
         ON CONFLICT(run_id) DO UPDATE SET summary = excluded.summary",
        params![run, serde_json::to_string(&summary)?],
    )?;
    Ok(())
}

fn load_capability_summary(db: &Connection, run: &str) -> Result<Option<CapabilitySummary>> {
    let summary: Option<String> = db
        .query_row(
            "SELECT summary FROM agent_capability_summaries WHERE run_id = ?1",
            [run],
            |row| row.get(0),
        )
        .optional()?;
    summary
        .map(|json| Ok(serde_json::from_str(&json)?))
        .transpose()
}

fn load_diagnostics(db: &Connection, run: &str) -> Result<(Vec<DiagnosticSummary>, bool)> {
    let mut query = db.prepare(
        "SELECT seq, source, payload FROM agent_events
         WHERE run_id = ?1 AND inspect_diagnostic = 1 ORDER BY seq DESC LIMIT ?2",
    )?;
    let mut rows = query.query(params![run, (DIAGNOSTIC_LIMIT + 1) as u32])?;
    let mut diagnoses = Vec::new();
    while let Some(row) = rows.next()? {
        if diagnoses.len() == DIAGNOSTIC_LIMIT {
            return Ok((diagnoses, true));
        }
        let payload: Vec<u8> = row.get(2)?;
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        let (kind, message) = match event.data {
            ProviderEventData::Diagnostic { message } => (DiagnosticKind::Protocol, message),
            ProviderEventData::Error { message } => (DiagnosticKind::Error, message),
            _ => anyhow::bail!("The diagnostic index does not match its event"),
        };
        diagnoses.push(DiagnosticSummary {
            sequence: row.get::<_, i64>(0)?.to_string(),
            source: serde_json::from_value(serde_json::Value::String(row.get(1)?))?,
            kind,
            message: bounded_preview(&message, 256),
        });
    }
    Ok((diagnoses, false))
}

impl Store {
    pub(crate) async fn inspect_omniagent(
        &self,
        expected: RunScope,
        response_budget: usize,
    ) -> Result<RuntimeInspection> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                read_active_scope(&tx, &expected.run_id)?.as_ref() == Some(&expected),
                "The inspection requires the same active run and project"
            );
            let run = load_run(&tx, &expected.run_id)?;
            let (serialized_scope, reported_model): (String, Option<String>) = tx.query_row(
                "SELECT s.scope, a.reported_model FROM agent_attempts a
                 JOIN agent_sessions s ON s.id = a.agent_session_id WHERE a.run_id = ?1",
                [&run.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let scope: SessionScope = serde_json::from_str(&serialized_scope)
                .context("The run profile is unavailable")?;
            let env = AdmissionEnvironment {
                runner: scope.runner.clone(),
                bridge_available: true,
            };
            let (queue, policy) = load_inspection_queue(&tx, &run.project_id, &run.id, &env)?;
            let task = queue
                .current
                .as_ref()
                .and_then(|job| job.task_id.as_ref())
                .map(|id| load_project_task(&tx, &run.project_id, id))
                .transpose()?
                .flatten()
                .map(|task| TaskSummary {
                    id: task.id,
                    title: bounded_preview(&task.title, 120),
                });
            let (diagnostics, more_diagnostics) = load_diagnostics(&tx, &run.id)?;
            let capabilities = load_capability_summary(&tx, &run.id)?;
            let settings_revision = revision(&(
                &queue.settings,
                &queue.project_policy_revision,
                &scope.permission_revision,
            ))?;
            let snapshot = RuntimeInspection {
                version: 1,
                build: BuildIdentity::running(),
                run,
                profile: ProfileSummary {
                    provider: scope.provider,
                    mode: scope.mode,
                    runner: bounded_preview(&scope.runner, 64),
                    cli_version: bounded_preview(&scope.cli_version, 64),
                    permission_revision: bounded_preview(&scope.permission_revision, 96),
                    requested_model: scope
                        .model
                        .as_deref()
                        .map(|model| bounded_preview(model, 80)),
                    reported_model: reported_model
                        .as_deref()
                        .map(|model| bounded_preview(model, 80)),
                },
                policy,
                settings_revision,
                capabilities,
                task,
                queue,
                diagnostics,
                more_diagnostics,
            };
            ensure!(
                serde_json::to_vec(&snapshot)?.len() <= response_budget,
                "The inspection exceeds the response budget"
            );
            let audit = ProviderEvent {
                location: Default::default(),
                data: ProviderEventData::RuntimeInspected {
                    jobs: (snapshot.queue.submitted.len()
                        + usize::from(snapshot.queue.current.is_some()))
                        as u32,
                    diagnostics: snapshot.diagnostics.len() as u32,
                    task: snapshot.task.is_some(),
                    capabilities: snapshot.capabilities.is_some(),
                },
            };
            tx.execute(
                "INSERT INTO agent_events (run_id, source, payload) VALUES (?1, ?2, ?3)",
                params![
                    expected.run_id,
                    EventSource::Application.as_sql(),
                    protection::protect(&serde_json::to_vec(&audit)?)?,
                ],
            )?;
            tx.commit()?;
            Ok(snapshot)
        })
        .await
    }
}
