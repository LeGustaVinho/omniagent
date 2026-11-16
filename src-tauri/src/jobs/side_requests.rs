use std::{cell::Cell, io, sync::Arc};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use tokio::{runtime::Handle, sync::watch, task::JoinHandle};

use crate::{
    conversations::{SessionScope, context_entries, load_conversation, load_messages},
    dispatch::ContextSnapshot,
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, SessionMode,
        capabilities::{
            CapabilityScope, ObservedResult, insert_side_fixture_evidence,
            side_fixture_qualifiable, verified_side_profile,
        },
        profiles::verify_account_binding,
        side::{
            self, OutputFixture, SideInput, SideOutcome, SidePin, SideResponse, SideRoute, SideSource,
            SideUnavailable,
        },
    },
    runner::{DirectWindowsRunner, LaunchRequest, Runner},
    runs::{self, RunRecord, RunStatus},
    store::Store,
};

use super::{
    Job, JobOrigin, JobRequest, insert_job, instructions::preserves_destination, load_job,
    scheduler::LaunchGate,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SidePurpose {
    Question,
    Suggestion,
    Validation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposerSuggestionMode {
    Off,
    Native,
    Omniagent,
}

impl ComposerSuggestionMode {
    fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Native => "native",
            Self::Omniagent => "omniagent",
        }
    }
}

impl SidePurpose {
    fn key(self) -> &'static str {
        match self {
            Self::Question | Self::Validation => "question",
            Self::Suggestion => "suggestion",
        }
    }
}

#[derive(Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SideSubmission {
    Queued {
        job_id: String,
    },
    Unavailable {
        provider: Provider,
        model: Option<String>,
        reason: SideUnavailable,
    },
    Skipped,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSideRequest {
    pub job: Job,
    pub conversation_id: String,
    pub source_run_id: String,
    pub context_id: String,
    pub purpose: SidePurpose,
    pub provider: Provider,
    pub profile_id: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: SessionMode,
    pub question: String,
    pub response: Option<SideResponse>,
    pub evidence_id: Option<String>,
    pub evidence_checked_at: Option<String>,
    pub output_fixture: Option<OutputFixture>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SidePayload {
    pub scope: SessionScope,
    pub profile: CapabilityScope,
    pub evidence_id: Option<String>,
    pub purpose: SidePurpose,
    pub route: SideRoute,
    pub pin: SidePin,
    pub question: String,
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_fixture: Option<OutputFixture>,
}

impl SidePayload {
    pub(super) fn input(&self, id: &str) -> SideInput {
        SideInput {
            request_id: id.into(),
            pin: self.pin.clone(),
            question: self.question.clone(),
        }
    }

    pub(super) fn still_qualified(&self, db: &Connection) -> Result<bool> {
        if self.purpose == SidePurpose::Validation {
            return Ok(side_fixture_qualifiable(&self.profile));
        }
        Ok(self.evidence_id.is_some()
            && verified_side_profile(db, &self.profile)? == self.evidence_id)
    }
}

pub(super) struct SideAttempt {
    pub run: RunRecord,
    pub job_id: String,
    pub payload: SidePayload,
}

struct SideSourceRun {
    run_id: String,
    context_id: String,
    reported_model: Option<String>,
    reported_effort: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    cursor: Option<String>,
    snapshot: Vec<u8>,
}

impl Store {
    pub async fn composer_suggestion_mode(&self) -> Result<ComposerSuggestionMode> {
        self.execute(|db| read_suggestion_mode(db)).await
    }

    pub async fn set_composer_suggestion_mode(
        &self,
        mode: ComposerSuggestionMode,
    ) -> Result<ComposerSuggestionMode> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE composer_suggestions_settings SET mode = ?1 WHERE id = 1",
                [mode.key()],
            )?;
            if mode != ComposerSuggestionMode::Omniagent {
                tx.execute(
                    "UPDATE job_side_requests SET discarded = 1 WHERE purpose = 'suggestion' AND NOT discarded",
                    [],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.job_signal.send_replace(());
        Ok(mode)
    }

    pub async fn enqueue_side_request(
        &self,
        conversation: String,
        expected_context: Option<String>,
        expected_source_run: Option<String>,
        scope: SessionScope,
        purpose: SidePurpose,
        question: String,
    ) -> Result<SideSubmission> {
        self.enqueue_side_request_with_fixture(conversation, expected_context, expected_source_run, scope, purpose, question, None).await
    }

    pub async fn enqueue_side_request_with_fixture(
        &self,
        conversation: String,
        expected_context: Option<String>,
        expected_source_run: Option<String>,
        scope: SessionScope,
        purpose: SidePurpose,
        question: String,
        output_fixture: Option<OutputFixture>,
    ) -> Result<SideSubmission> {
        ensure!(output_fixture.is_none() || (purpose == SidePurpose::Validation && expected_source_run.is_some()),
            "An output fixture requires an explicit validation of a recorded source run");
        let question = match purpose {
            SidePurpose::Question => question.trim().to_owned(),
            SidePurpose::Suggestion => "Suggest one useful follow-up prompt the user might send. Return only a JSON object with a single string field named prompt.".into(),
            SidePurpose::Validation => output_fixture.map(|fixture| fixture.label().to_owned()).unwrap_or_else(||
                "Return the exact code labeled 'Verification code' in the supplied context, without commentary. If a file-reading tool is available, try reading fixture.txt first. The code is in the supplied context even when file access is unavailable.".into()),
        };
        ensure!(
            !question.is_empty() && question.len() <= 8192,
            "The side question must contain between 1 and 8192 bytes"
        );
        let result = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let chat = load_conversation(&tx, &conversation)?;
                ensure!(
                    expected_context.as_ref().is_none_or(|id| id == &chat.context_id),
                    "The conversation changed before the side request was queued"
                );
                let expected_context = chat.context_id;
                let (project, root): (String, String) = tx.query_row(
                    "SELECT project_id, root FROM checkouts WHERE id = ?1",
                    [&chat.checkout_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                ensure!(
                    scope.checkout == std::path::Path::new(&root),
                    "The side request belongs to another checkout"
                );
                let unavailable = |reason| SideSubmission::Unavailable {
                    provider: scope.provider,
                    model: scope.model.clone(),
                    reason,
                };
                if purpose == SidePurpose::Validation {
                    ensure!(matches!(scope.mode, SessionMode::ReadOnly | SessionMode::Plan),
                        "The isolated validation requires a read-only or planning profile");
                }
                if let Some(fixture) = output_fixture
                    && (scope.provider != Provider::Codex || scope.runner != DirectWindowsRunner::IDENTITY || !fixture.available()) {
                    return Ok(unavailable(SideUnavailable::OutputFixtureUnavailable));
                }
                let fresh_route = match scope.provider {
                    Provider::Codex => SideRoute::FreshCodexSession,
                    Provider::Claude => SideRoute::FreshClaudeSession,
                    Provider::Antigravity => {
                        return Ok(unavailable(SideUnavailable::ToolFreeFreshRouteUnavailable));
                    }
                };
                if purpose == SidePurpose::Suggestion
                    && !suggestion_eligible(&tx, &conversation, &expected_context)?
                {
                    return Ok(SideSubmission::Skipped);
                }
                let Some(source) = source_run(&tx, &conversation, &expected_context)? else {
                    return Ok(unavailable(SideUnavailable::SourceBoundaryUnavailable));
                };
                if expected_source_run.as_ref().is_some_and(|id| id != &source.run_id) {
                    return Ok(SideSubmission::Skipped);
                }
                let snapshot: ContextSnapshot =
                    serde_json::from_slice(&protection::unprotect(&source.snapshot)?)?;
                let Some(profile) = latest_reported_scope(&tx, &source.run_id)?
                    .or(snapshot.capabilities.map(|assessment| assessment.scope))
                    .filter(|profile| {
                        snapshot.scope == scope
                            && profile.reported_model == source.reported_model
                            && profile.reported_effort == source.reported_effort
                    })
                else {
                    return Ok(unavailable(SideUnavailable::ProfileMismatch));
                };
                let (route, evidence_id) = if purpose == SidePurpose::Validation {
                    if !side_fixture_qualifiable(&profile) {
                        return Ok(unavailable(SideUnavailable::UnverifiedProfile));
                    }
                    if let Some(job_id) = queued_validation(&tx, &source.run_id)? {
                        let existing_payload: Vec<u8> = tx.query_row(
                            "SELECT payload FROM job_side_requests WHERE job_id = ?1", [&job_id], |row| row.get(0)
                        )?;
                        if decode::<SidePayload>(&existing_payload)?.output_fixture != output_fixture {
                            return Ok(unavailable(SideUnavailable::Busy));
                        }
                        return Ok(SideSubmission::Queued { job_id });
                    }
                    (fresh_route, None)
                } else {
                    let Some(evidence_id) = verified_side_profile(&tx, &profile)? else {
                        return Ok(unavailable(SideUnavailable::UnverifiedProfile));
                    };
                    let verified_route: Option<String> = tx.query_row(
                        "SELECT route FROM side_validation_evidence WHERE evidence_id = ?1",
                        [&evidence_id], |row| row.get(0),
                    ).optional()?;
                    let route = match verified_route {
                        Some(route) => serde_json::from_value(serde_json::Value::String(route))?,
                        None if scope.provider == Provider::Codex => SideRoute::CodexSample,
                        None => fresh_route,
                    };
                    (route, Some(evidence_id))
                };
                let provider_source = if route == SideRoute::CodexSample {
                    match (source.session_id, source.turn_id, source.cursor) {
                        (Some(session_id), Some(turn_id), Some(cursor))
                            if cursor == source.context_id =>
                        {
                            Some(SideSource {
                                session_id,
                                turn_id,
                            })
                        }
                        _ => return Ok(unavailable(SideUnavailable::SourceBoundaryUnavailable)),
                    }
                } else {
                    None
                };
                if purpose == SidePurpose::Suggestion && source.context_id != expected_context {
                    return Ok(SideSubmission::Skipped);
                }
                let context = if purpose == SidePurpose::Validation {
                    uuid::Uuid::new_v4().to_string()
                } else if route != SideRoute::CodexSample {
                    let revisions = context_entries(&tx, &conversation, &source.context_id)?;
                    let messages = load_messages(&tx, &conversation, &revisions)?;
                    let context = serde_json::to_string(&messages)?;
                    if context.len() > 64 * 1024 {
                        return Ok(unavailable(SideUnavailable::InvalidInput));
                    }
                    context
                } else {
                    String::new()
                };
                let title = match purpose {
                    SidePurpose::Question => "Side question",
                    SidePurpose::Suggestion => "Composer suggestion",
                    SidePurpose::Validation => output_fixture.map(OutputFixture::label).unwrap_or("Validate isolated side requests"),
                };
                let job = insert_job(
                    &tx,
                    project,
                    None,
                    JobOrigin::Operator,
                    JobRequest {
                        checkout_id: Some(chat.checkout_id),
                        task_id: None,
                        mode: if purpose == SidePurpose::Validation { scope.mode } else { SessionMode::ReadOnly },
                        instructions: title.into(),
                        dependencies: Vec::new(),
                        requires_bridge: false,
                    },
                    if purpose == SidePurpose::Suggestion {
                        i32::MIN
                    } else {
                        0
                    },
                    None,
                )?;
                let payload = SidePayload {
                    scope,
                    profile,
                    evidence_id,
                    purpose,
                    route,
                    question,
                    context,
                    output_fixture,
                    pin: SidePin {
                        context_id: source.context_id,
                        provider_source,
                    },
                };
                tx.execute(
                    "INSERT INTO job_side_requests(job_id, conversation_id, context_id, source_run_id, purpose, payload, validation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        job.id,
                        conversation,
                        payload.pin.context_id,
                        source.run_id,
                        purpose.key(),
                        protection::protect(&serde_json::to_vec(&payload)?)?,
                        purpose == SidePurpose::Validation,
                    ],
                )?;
                tx.commit()?;
                Ok(SideSubmission::Queued { job_id: job.id })
            })
            .await?;
        if matches!(result, SideSubmission::Queued { .. }) {
            self.job_signal.send_replace(());
        }
        Ok(result)
    }

    pub async fn saved_side_requests(&self, conversation: String) -> Result<Vec<SavedSideRequest>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT j.project_id, s.job_id, s.context_id, s.source_run_id, s.payload, response.payload, validation.evidence_id, e.checked_at
                 FROM job_side_requests s JOIN jobs j ON j.id = s.job_id
                 LEFT JOIN job_attempts a ON a.job_id = j.id AND a.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id)
                 LEFT JOIN job_side_responses response ON response.run_id = a.run_id
                 LEFT JOIN side_validation_evidence validation ON validation.run_id = response.run_id
                 LEFT JOIN capability_evidence e ON e.id = validation.evidence_id
                 WHERE s.conversation_id = ?1 AND NOT s.discarded ORDER BY j.sequence",
            )?;
            let mut rows = query.query([&conversation])?;
            let mut saved = Vec::new();
            while let Some(row) = rows.next()? {
                let job = load_job(db, row.get_ref(0)?.as_str()?, row.get_ref(1)?.as_str()?)?.context("Missing side request job")?;
                let payload: SidePayload = decode(row.get_ref(4)?.as_blob()?)?;
                let response: Option<SideResponse> = row.get::<_, Option<Vec<u8>>>(5)?.as_deref().map(decode).transpose()?;
                let evidence_checked_at = if let Some(fixture) = response.as_ref().and_then(|response| response.output_fixture.as_ref()) {
                    fixture.checked_at.clone()
                } else {
                    row.get(7)?
                };
                saved.push(SavedSideRequest {
                    job, conversation_id: conversation.clone(), context_id: row.get(2)?, source_run_id: row.get(3)?,
                    purpose: payload.purpose, question: payload.question, response, evidence_id: row.get(6)?,
                    evidence_checked_at,
                    output_fixture: payload.output_fixture,
                    provider: payload.scope.provider, profile_id: payload.scope.profile_id,
                    model: payload.scope.model, effort: payload.scope.effort, mode: payload.scope.mode,
                });
            }
            Ok(saved)
        }).await
    }

    pub async fn side_output_fixture_audio(
        &self,
        project: String,
        conversation: String,
        run: String,
    ) -> Result<Option<String>> {
        self.execute(move |db| {
            let protected: Option<Vec<u8>> = db.query_row(
                "SELECT b.payload FROM side_output_fixture_payloads b
                 JOIN job_attempts a ON a.run_id = b.run_id
                 JOIN job_side_requests s ON s.job_id = a.job_id
                 JOIN jobs j ON j.id = a.job_id
                 WHERE j.project_id = ?1 AND s.conversation_id = ?2 AND b.run_id = ?3 AND s.validation = 1 AND NOT s.discarded",
                params![project, conversation, run], |row| row.get(0),
            ).optional()?;
            protected.map(|payload| Ok(STANDARD.encode(protection::unprotect(&payload)?))).transpose()
        }).await
    }
}

fn suggestion_eligible(db: &Connection, conversation: &str, context: &str) -> Result<bool> {
    if read_suggestion_mode(db)? != ComposerSuggestionMode::Omniagent {
        return Ok(false);
    }
    let unavailable: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM job_side_requests WHERE conversation_id = ?1 AND context_id = ?2 AND purpose = 'suggestion')
             OR EXISTS (SELECT 1 FROM active_agent_attempts WHERE conversation_id = ?1)
             OR EXISTS (SELECT 1 FROM open_agent_questions WHERE conversation_id = ?1)",
        params![conversation, context], |row| row.get(0),
    )?;
    if unavailable {
        return Ok(false);
    }
    empty_draft(db, conversation)
}

fn queued_validation(db: &Connection, run_id: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT s.job_id FROM job_side_requests s JOIN jobs j ON j.id = s.job_id
         WHERE s.source_run_id = ?1 AND s.validation AND j.queue_state <> 'cancelled'
           AND (
               NOT EXISTS (SELECT 1 FROM job_attempts WHERE job_id = s.job_id)
               OR EXISTS (SELECT 1 FROM job_recoveries WHERE job_id = s.job_id)
               OR EXISTS (SELECT 1 FROM job_attempts a JOIN runs r ON r.id = a.run_id
                   WHERE a.job_id = s.job_id AND r.status IN ('starting', 'running'))
           ) LIMIT 1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?)
}

fn read_suggestion_mode(db: &Connection) -> Result<ComposerSuggestionMode> {
    let mode: String = db.query_row(
        "SELECT mode FROM composer_suggestions_settings WHERE id = 1",
        [],
        |r| r.get(0),
    )?;
    match mode.as_str() {
        "off" => Ok(ComposerSuggestionMode::Off),
        "native" => Ok(ComposerSuggestionMode::Native),
        "omniagent" => Ok(ComposerSuggestionMode::Omniagent),
        _ => anyhow::bail!("Unknown composer suggestion mode"),
    }
}

fn empty_draft(db: &Connection, conversation: &str) -> Result<bool> {
    let draft: Option<Vec<u8>> = db.query_row(
        "SELECT payload FROM drafts WHERE kind = 'conversation' AND owner_id = ?1 AND document = ''",
        [conversation], |row| row.get(0),
    ).optional()?;
    Ok(draft
        .as_deref()
        .map(protection::unprotect)
        .transpose()?
        .is_none_or(|bytes| bytes.iter().all(u8::is_ascii_whitespace)))
}

fn source_run(db: &Connection, conversation: &str, active: &str) -> Result<Option<SideSourceRun>> {
    let mut query = db.prepare(
        "SELECT a.run_id, a.result_context_id, a.reported_model, a.reported_effort, session.provider_session_id, a.provider_turn_id,
                session.cursor_context_id, snapshot.payload
         FROM agent_attempts a JOIN runs r ON r.id = a.run_id
         JOIN agent_sessions session ON session.id = a.agent_session_id
         JOIN context_snapshots snapshot ON snapshot.run_id = a.run_id
         JOIN dispatches d ON d.snapshot_id = snapshot.id AND d.ordinal = 1 AND d.delivery = 'submitted'
         WHERE a.conversation_id = ?1 AND NOT a.detached_context AND a.provider_terminal = 'completed'
           AND r.status = 'succeeded' AND a.result_context_id IS NOT NULL ORDER BY a.ordinal DESC",
    )?;
    let mut rows = query.query([conversation])?;
    while let Some(row) = rows.next()? {
        let context_id: String = row.get(1)?;
        if preserves_destination(db, conversation, &context_id, active)? {
            return Ok(Some(SideSourceRun {
                run_id: row.get(0)?,
                context_id,
                reported_model: row.get(2)?,
                reported_effort: row.get(3)?,
                session_id: row.get(4)?,
                turn_id: row.get(5)?,
                cursor: row.get(6)?,
                snapshot: row.get(7)?,
            }));
        }
    }
    Ok(None)
}

fn latest_reported_scope(db: &Connection, run: &str) -> Result<Option<CapabilityScope>> {
    let mut query = db.prepare(
        "SELECT payload FROM agent_events WHERE run_id = ?1 AND source = 'application' ORDER BY seq DESC",
    )?;
    let mut rows = query.query([run])?;
    while let Some(row) = rows.next()? {
        let observation: ProviderEvent = decode(row.get_ref(0)?.as_blob()?)?;
        if let ProviderEventData::CapabilityAssessmentUpdated { assessment } = observation.data {
            return Ok(Some(assessment.scope));
        }
    }
    Ok(None)
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}

pub(super) fn load(db: &Connection, job: &str) -> Result<Option<SidePayload>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT payload FROM job_side_requests WHERE job_id = ?1",
            [job],
            |row| row.get(0),
        )
        .optional()?;
    payload.as_deref().map(decode).transpose()
}

pub(super) fn suggestion_still_current(
    db: &Connection,
    id: &str,
    payload: &SidePayload,
) -> Result<bool> {
    if payload.purpose != SidePurpose::Suggestion {
        return Ok(true);
    }
    if read_suggestion_mode(db)? != ComposerSuggestionMode::Omniagent {
        return Ok(false);
    }
    let (conversation, active, waiting): (String, String, bool) = db.query_row(
        "SELECT c.id, c.active_context_id, s.discarded OR EXISTS(SELECT 1 FROM active_agent_attempts WHERE conversation_id = c.id)
               OR EXISTS(SELECT 1 FROM open_agent_questions WHERE conversation_id = c.id)
         FROM job_side_requests s JOIN conversations c ON c.id = s.conversation_id WHERE s.job_id = ?1",
        [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(active == payload.pin.context_id && !waiting && empty_draft(db, &conversation)?)
}

pub(crate) fn recover(db: &Connection) -> Result<()> {
    let mut query = db.prepare(
        "SELECT a.run_id FROM job_attempts a JOIN job_side_requests s ON s.job_id = a.job_id
         JOIN runs r ON r.id = a.run_id WHERE r.status IN ('starting', 'running')",
    )?;
    let runs = query
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in runs {
        let run = runs::load_run(db, &id)?;
        let job: String = db.query_row(
            "SELECT job_id FROM job_attempts WHERE run_id = ?1",
            [&run.id],
            |row| row.get(0),
        )?;
        let payload = load(db, &job)?.context("Missing side payload")?;
        let response = payload.input(&job).reply(SideOutcome::Interrupted);
        settle(db, &run, RunStatus::Interrupted, &response)?;
        super::recovery::requeue_unlaunched_job(db, &run.id)?;
    }
    Ok(())
}

pub(super) fn settle(
    db: &Connection,
    run: &RunRecord,
    status: RunStatus,
    response: &SideResponse,
) -> Result<()> {
    db.execute(
        "INSERT INTO job_side_responses VALUES (?1, ?2)",
        params![run.id, protection::protect(&serde_json::to_vec(response)?)?],
    )?;
    runs::advance_run(db, run, status)?;
    Ok(())
}

pub(super) fn start(
    store: Store,
    runner: Arc<dyn Runner>,
    attempt: SideAttempt,
    admission: LaunchGate,
) -> (watch::Sender<bool>, JoinHandle<Result<()>>) {
    let app = Handle::current();
    let (cancel, cancelled) = watch::channel(false);
    let worker = tokio::task::spawn_blocking(move || {
        let SideAttempt {
            run,
            job_id,
            payload,
        } = attempt;
        let stopped = || *cancelled.borrow() || admission.is_closed();
        let profile_matches = runner.identity() == payload.scope.runner
            && runner.executable_revision(&payload.scope.executable)
                == payload.profile.executable_revision
            && runner.execution_platform() == payload.profile.platform;
        let launched = Cell::new(false);
        let launch = |request: LaunchRequest<'_>| {
            let _guard = app.block_on(admission.enter()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::Interrupted, "The scheduler is exiting")
            })?;
            let allowed = app
                .block_on(store.execute({
                    let id = run.id.clone();
                    let job = job_id.clone();
                    move |db| {
                        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                        let side = load(&tx, &job)?.context("Missing side request")?;
                        let allowed = side.still_qualified(&tx)?
                            && suggestion_still_current(&tx, &job, &side)?
                            && super::record_launch_intent(&tx, &id)? == Some(true);
                        if allowed {
                            runs::advance_run(&tx, &runs::load_run(&tx, &id)?, RunStatus::Running)?;
                        }
                        tx.commit()?;
                        Ok(allowed)
                    }
                }))
                .map_err(io::Error::other)?;
            if !allowed {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "The isolated request is no longer authorized",
                ));
            }
            store.event_signal.send_replace(());
            let process = runner.launch(request)?;
            launched.set(true);
            Ok(process)
        };
        let account_matches = !stopped()
            && profile_matches
            && verify_account_binding(&payload.scope, |request| {
                let _guard = app.block_on(admission.enter()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Interrupted, "The scheduler is exiting")
                })?;
                runner.launch(request)
            })
            .is_ok();
        let mut response = if !stopped() && account_matches {
            let validation_context = (payload.purpose == SidePurpose::Validation)
                .then(|| format!("Verification code: {}", payload.context));
            let context = validation_context.as_deref().unwrap_or(&payload.context);
            let outcome = match payload.route {
                SideRoute::FreshClaudeSession => side::run_fresh_until(
                    &payload.scope,
                    launch,
                    payload.input(&job_id),
                    context,
                    payload.profile.reported_model.as_deref(),
                    &stopped,
                ),
                SideRoute::FreshCodexSession => side::run_fresh_codex_until(
                    &payload.scope,
                    launch,
                    payload.input(&job_id),
                    context,
                    payload.profile.reported_model.as_deref(),
                    payload.profile.reported_effort.as_deref(),
                    payload.output_fixture,
                    &stopped,
                ),
                SideRoute::CodexSample => side::run_native_until(
                    &payload.scope,
                    launch,
                    payload.input(&job_id),
                    payload.profile.reported_model.as_deref(),
                    payload.profile.reported_effort.as_deref(),
                    &stopped,
                ),
            };
            match outcome {
                Ok(response) => response,
                Err(error) if error.is::<side::SideProcessStillActive>() => return Err(error),
                Err(_) => payload.input(&job_id).reply(SideOutcome::Failed),
            }
        } else {
            payload.input(&job_id).reply(SideOutcome::Unavailable {
                reason: if profile_matches {
                    SideUnavailable::AccountChanged
                } else {
                    SideUnavailable::ProfileMismatch
                },
            })
        };
        let status = if *cancelled.borrow() {
            response.outcome = SideOutcome::Cancelled;
            RunStatus::Cancelled
        } else if admission.is_closed() {
            response.outcome = SideOutcome::Interrupted;
            RunStatus::Interrupted
        } else {
            if payload.purpose == SidePurpose::Suggestion {
                normalize_suggestion(&mut response.outcome);
            }
            match response.outcome {
                SideOutcome::Answered { .. } | SideOutcome::NoText => RunStatus::Succeeded,
                SideOutcome::Cancelled => RunStatus::Cancelled,
                _ => RunStatus::Failed,
            }
        };
        app.block_on(store.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = runs::load_run(&tx, &run.id)?;
            let cancelled: bool = tx.query_row(
                "SELECT cancel_requested FROM job_attempts WHERE run_id = ?1",
                [&run.id],
                |row| row.get(0),
            )?;
            let mut status = if cancelled {
                response.outcome = SideOutcome::Cancelled;
                RunStatus::Cancelled
            } else {
                status
            };
            if !suggestion_still_current(&tx, &job_id, &payload)?
                && matches!(response.outcome, SideOutcome::Answered { .. })
            {
                response.outcome = SideOutcome::NoText;
            }
            let fixture_result = (payload.purpose == SidePurpose::Validation && payload.output_fixture.is_none() && launched.get()
                && matches!(status, RunStatus::Succeeded | RunStatus::Failed)
                && !matches!(response.outcome, SideOutcome::Unavailable { .. }))
            .then(|| {
                if matches!(&response.outcome, SideOutcome::Answered { route, text, .. } if *route == payload.route && text.trim() == payload.context) {
                    ObservedResult::Succeeded
                } else {
                    status = RunStatus::Failed;
                    ObservedResult::Failed
                }
            });
            if status != RunStatus::Succeeded {
                response.output_fixture = None;
                response.output_fixture_audio = None;
            } else if let Some(receipt) = &mut response.output_fixture {
                receipt.checked_at = Some(tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| row.get(0))?);
            }
            settle(&tx, &current, status, &response)?;
            if let Some(audio) = response.output_fixture_audio.take() {
                tx.execute("INSERT INTO side_output_fixture_payloads VALUES (?1, ?2)",
                    params![run.id, protection::protect(&audio)?])?;
            }
            if let Some(result) = fixture_result {
                let evidence = insert_side_fixture_evidence(&tx, &payload.profile, result, &run.id)?;
                tx.execute("INSERT INTO side_validation_evidence VALUES (?1, ?2, ?3)", params![run.id, evidence, payload.route.as_str()])?;
            }
            if status == RunStatus::Interrupted {
                super::recovery::requeue_unlaunched_job(&tx, &current.id)?;
            }
            tx.commit()?;
            Ok(())
        }))?;
        store.job_signal.send_replace(());
        store.event_signal.send_replace(());
        Ok(())
    });
    (cancel, worker)
}

fn normalize_suggestion(outcome: &mut SideOutcome) {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Suggestion {
        prompt: String,
    }
    if let SideOutcome::Answered { text, .. } = outcome {
        match serde_json::from_str::<Suggestion>(text) {
            Ok(suggestion)
                if !suggestion.prompt.trim().is_empty() && suggestion.prompt.len() <= 2048 =>
            {
                *text = suggestion.prompt.trim().into();
            }
            _ => *outcome = SideOutcome::NoText,
        }
    }
}
