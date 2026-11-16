use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    conversations::MessageRole,
    dispatch::{ContextSnapshot, NativeHistory},
    mcp_bridge::{RunScope, read_active_scope},
    providers::{Provider, SessionMode},
    workflows::digest,
};

use super::*;

mod catalog;
mod edits;
mod model;
#[cfg(test)]
mod tests;
pub(crate) use model::*;

const MAX_OPERATION_BYTES: usize = 10 * 1024;
const MAX_OPERATIONS: usize = 8;
const MAX_WRITES: i64 = 96;

#[derive(Debug)]
struct AuthoringSession {
    run: String,
    project: String,
    authoring_request: String,
    draft: String,
    base: String,
    head: String,
    working: String,
    submitted: Option<String>,
}

#[derive(Serialize)]
enum EditCommand {
    Apply(ApplyOperations),
    Submit(SubmitForReview),
}

impl EditCommand {
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Apply(input) => (&input.request_id, &input.expected_revision),
            Self::Submit(input) => (&input.request_id, &input.expected_revision),
        }
    }
}

impl Store {
    pub(crate) async fn authoring_tools_for_run(&self, scope: RunScope) -> Result<bool> {
        self.execute(move |db| Ok(find_session(db, &scope, false)?.is_some()))
            .await
    }

    pub(crate) async fn read_authoring_catalog(
        &self,
        scope: RunScope,
        request: ReadCatalog,
    ) -> Result<Value> {
        self.execute(move |db| {
            let session = find_session(db, &scope, true)?
                .context("This is not an active package authoring attempt")?;
            catalog::read(db, &session, request)
        })
        .await
    }

    pub(crate) async fn apply_authoring_operations(
        &self,
        scope: RunScope,
        input: ApplyOperations,
    ) -> Result<AuthoringOutcome> {
        ensure!(
            !input.operations.is_empty() && input.operations.len() <= MAX_OPERATIONS,
            "Submit one to {MAX_OPERATIONS} typed operations"
        );
        self.authoring_command(scope, EditCommand::Apply(input))
            .await
    }

    pub(crate) async fn submit_authoring_for_review(
        &self,
        scope: RunScope,
        input: SubmitForReview,
    ) -> Result<AuthoringOutcome> {
        self.authoring_command(scope, EditCommand::Submit(input))
            .await
    }

    async fn authoring_command(
        &self,
        scope: RunScope,
        command: EditCommand,
    ) -> Result<AuthoringOutcome> {
        let bytes = serde_json::to_vec(&command)?;
        ensure!(
            bytes.len() <= MAX_OPERATION_BYTES,
            "The authoring operation exceeds the byte limit"
        );
        let fingerprint = digest(&bytes);
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let session = find_session(&tx, &scope, true)?.context("This is not an active package authoring attempt")?;
            let (request_id, expected) = command.identity();
            bounded_name(request_id)?;
            bounded_name(expected)?;
            if let Some((old, response)) = tx.query_row(
                "SELECT fingerprint, response FROM workflow_package_mcp_calls WHERE run_id = ?1 AND request_id = ?2",
                params![session.run, request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            ).optional()? {
                return if old == fingerprint { open(&response) } else { Ok(AuthoringOutcome::RequestConflict) };
            }
            let count: i64 = tx.query_row("SELECT count(*) FROM workflow_package_mcp_calls WHERE run_id = ?1",
                [&session.run], |row| row.get(0))?;
            ensure!(count < MAX_WRITES, "This authoring attempt reached its operation limit");
            tx.execute(
                "INSERT INTO workflow_package_mcp_sessions
                    (run_id, authoring_request_id, draft_id, base_revision, working_revision)
                 VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(run_id) DO NOTHING",
                params![session.run, session.authoring_request, session.draft, session.base, session.working],
            )?;
            let outcome = if session.submitted.is_some() {
                AuthoringOutcome::Rejected { message: "This attempt already submitted its revision for review.".into() }
            } else if session.head != session.base {
                AuthoringOutcome::RevisionConflict { current_revision: session.head.clone() }
            } else if expected != session.working {
                AuthoringOutcome::RevisionConflict { current_revision: session.working.clone() }
            } else {
                match &command {
                    EditCommand::Apply(input) => apply(&tx, &session, &input.operations)?,
                    EditCommand::Submit(_) => submit(&tx, &session)?,
                }
            };
            tx.execute("INSERT INTO workflow_package_mcp_calls VALUES (?1, ?2, ?3, ?4)",
                params![session.run, request_id, fingerprint, seal(&outcome)?])?;
            tx.commit()?;
            Ok(outcome)
        }).await
    }
}

fn find_session(
    db: &Connection,
    scope: &RunScope,
    dispatched: bool,
) -> Result<Option<AuthoringSession>> {
    if read_active_scope(db, &scope.run_id)?.as_ref() != Some(scope) {
        return Ok(None);
    }
    let row = db.query_row(
        "SELECT d.project_id, request.id, d.id, request.parent_revision, d.head_revision,
                coalesce(mcp.working_revision, request.parent_revision), mcp.submitted_revision, request.payload
         FROM workflow_package_requests request
         JOIN workflow_package_drafts d ON d.id = request.draft_id
         JOIN jobs j ON j.id = request.job_id
         JOIN job_attempts attempt ON attempt.job_id = j.id
         JOIN runs r ON r.id = attempt.run_id AND r.checkout_id = j.checkout_id
         JOIN checkouts checkout ON checkout.id = r.checkout_id AND checkout.project_id = d.project_id
         JOIN agent_attempts agent ON agent.run_id = r.id
         JOIN conversations chat ON chat.id = agent.conversation_id
         LEFT JOIN workflow_package_mcp_sessions mcp ON mcp.run_id = r.id
         WHERE r.id = ?1 AND request.structured_mcp AND chat.background AND j.mode = 'read_only'
           AND j.project_id = d.project_id
           AND (request.result IS NULL OR (request.result = 'saved' AND mcp.submitted_revision IS NOT NULL))
           AND attempt.ordinal = (SELECT max(ordinal) FROM job_attempts WHERE job_id = j.id)
           AND request.rowid = (SELECT max(rowid) FROM workflow_package_requests WHERE draft_id = d.id)
           AND NOT EXISTS(SELECT 1 FROM workflow_package_publications WHERE draft_id = d.id)",
        [&scope.run_id], |row| Ok((AuthoringSession {
            run: scope.run_id.clone(), project: row.get(0)?, authoring_request: row.get(1)?,
            draft: row.get(2)?, base: row.get(3)?, head: row.get(4)?, working: row.get(5)?,
            submitted: row.get(6)?,
        }, row.get::<_, Vec<u8>>(7)?)),
    ).optional()?;
    let Some((session, prompt)) = row else {
        return Ok(None);
    };
    if dispatched {
        let snapshot: Option<Vec<u8>> = db
            .query_row(
                "SELECT snap.payload FROM context_snapshots snap
             JOIN dispatches dispatch ON dispatch.snapshot_id = snap.id AND dispatch.ordinal = 1
             JOIN runs run ON run.id = snap.run_id
             WHERE snap.run_id = ?1 AND dispatch.delivery = 'submitted' AND run.status = 'running'",
                [&scope.run_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };
        let snapshot: ContextSnapshot = open(&snapshot)?;
        let prompt: String = open(&prompt)?;
        ensure!(
            snapshot.scope.mode == SessionMode::ReadOnly
                && matches!(snapshot.scope.provider, Provider::Codex | Provider::Claude)
                && matches!(snapshot.native_history, NativeHistory::Fresh)
                && matches!(snapshot.submitted_messages.as_slice(), [message]
                    if message.role == MessageRole::User && message.text == prompt),
            "This dispatch does not match the protected package request"
        );
    }
    Ok(Some(session))
}

fn apply(
    db: &Connection,
    session: &AuthoringSession,
    operations: &[DraftOperation],
) -> Result<AuthoringOutcome> {
    let (_, previous) = read_document(db, &session.draft, &session.working)?;
    let document = (|| {
        let mut package = previous.package.clone();
        for operation in operations {
            edits::apply(&mut package, operation)?;
        }
        edits::validate_visible(db, &session.project, &package)?;
        PackageDocument::capture(db, &session.project, package, Some(&previous))
    })();
    let document = match document {
        Ok(document) => document,
        Err(error) => return Ok(rejected(error)),
    };
    let revision = insert_revision(
        db,
        &session.draft,
        Some(&session.working),
        Some(&session.run),
        &document,
    )?;
    db.execute(
        "UPDATE workflow_package_mcp_sessions SET working_revision = ?2 WHERE run_id = ?1",
        params![session.run, revision],
    )?;
    Ok(AuthoringOutcome::Applied { revision })
}

fn submit(db: &Connection, session: &AuthoringSession) -> Result<AuthoringOutcome> {
    let (_, document) = read_document(db, &session.draft, &session.working)?;
    if let Err(error) = edits::validate_submission(db, &session.project, &document.package) {
        return Ok(rejected(error));
    }
    let revision = if session.working == session.base {
        insert_revision(
            db,
            &session.draft,
            Some(&session.working),
            Some(&session.run),
            &document,
        )?
    } else {
        session.working.clone()
    };
    promote_revision(db, &session.draft, Some(&session.base), &revision)?;
    db.execute(
        "UPDATE workflow_package_mcp_sessions SET working_revision = ?2, submitted_revision = ?2 WHERE run_id = ?1",
        params![session.run, revision],
    )?;
    ensure!(db.execute(
        "UPDATE workflow_package_requests SET result = 'saved' WHERE id = ?1 AND result IS NULL",
        [&session.authoring_request],
    )? == 1, "The authoring request has already settled");
    Ok(AuthoringOutcome::Submitted { revision })
}

fn rejected(error: anyhow::Error) -> AuthoringOutcome {
    AuthoringOutcome::Rejected {
        message: crate::diagnostics::preview(&error.to_string())
            .chars()
            .take(256)
            .collect(),
    }
}
