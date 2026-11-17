use anyhow::{Context, ensure};
use rusqlite::{Connection, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::{
    dispatch::ContextSnapshot,
    providers::{
        Provider,
        capabilities::{
            EvidenceOrigin, ObservedResult, Verification, native_image_generation_scope_key,
        },
        profiles::DESKTOP_PERMISSION_REVISION,
    },
    runner::{DirectWindowsRunner, Runner},
};

use super::*;

fn receipt(db: &Connection, key: &str, sequence: Option<i64>) -> Result<Option<Verification>> {
    Ok(db
        .query_row(
            "SELECT id, checked_at, event_seq FROM generated_image_qualifications
         WHERE qualification_key = ?1 AND (?2 IS NULL OR event_seq = ?2)
         ORDER BY checked_at DESC, event_seq DESC LIMIT 1",
            params![key, sequence],
            |row| {
                Ok(Verification {
                    id: row.get(0)?,
                    checked_at: row.get(1)?,
                    origin: EvidenceOrigin::AuthenticatedProviderRun,
                    result: ObservedResult::Succeeded,
                    artifact_id: format!("generated-image:{}", row.get::<_, i64>(2)?),
                    attachment_limits: None,
                })
            },
        )
        .optional()?)
}

pub(crate) fn latest_native_image_qualification(
    db: &Connection,
    key: &str,
) -> Result<Option<Verification>> {
    receipt(db, key, None)
}

pub(crate) fn is_current_native_image_qualification(db: &Connection, id: &str) -> Result<bool> {
    let key: Option<String> = db
        .query_row(
            "SELECT qualification_key FROM generated_image_qualifications WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(key) = key else {
        return Ok(false);
    };
    Ok(latest_native_image_qualification(db, &key)?.is_some_and(|evidence| evidence.id == id))
}

impl Store {
    pub async fn qualify_conversation_generated_image(
        &self,
        project: String,
        conversation: String,
        run: String,
        sequence: i64,
    ) -> Result<Verification> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let key = qualification_key(&tx, &project, &conversation, &run, sequence)?;
            tx.execute(
                "INSERT INTO generated_image_qualifications (id, event_seq, qualification_key)
                 VALUES (?1, ?2, ?3) ON CONFLICT(event_seq) DO NOTHING",
                params![uuid::Uuid::new_v4().to_string(), sequence, key],
            )?;
            let evidence = receipt(&tx, &key, Some(sequence))?
                .context("The saved image belongs to a different qualification scope")?;
            tx.commit()?;
            Ok(evidence)
        })
        .await
    }
}

fn qualification_key(
    db: &Connection,
    project: &str,
    conversation: &str,
    run: &str,
    sequence: i64,
) -> Result<String> {
    let image = db.query_row(
        "SELECT d.id, c.id, s.provider_session_id, a.provider_turn_id, i.payload, e.payload, p.payload
         FROM agent_events e JOIN runs r ON r.id = e.run_id
         JOIN checkouts c ON c.id = r.checkout_id
         JOIN agent_attempts a ON a.run_id = r.id
         JOIN agent_sessions s ON s.id = a.agent_session_id
         JOIN context_snapshots i ON i.run_id = r.id
         JOIN dispatches d ON d.snapshot_id = i.id AND d.ordinal = 1
         JOIN generated_image_evidence_payloads p ON p.event_seq = e.seq
         WHERE c.project_id = ?1 AND a.conversation_id = ?2 AND r.id = ?3 AND e.seq = ?4
           AND r.status = 'succeeded' AND e.source = 'provider' AND d.delivery = 'submitted'",
        params![project, conversation, run, sequence],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, Vec<u8>>(5)?,
            row.get::<_, Vec<u8>>(6)?)),
    ).optional()?.context("The selected image is unavailable from a successful submitted attempt in this project")?;
    let (dispatch, checkout, session, turn, snapshot, event, bytes) = image;
    let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&snapshot)?)?;
    let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&event)?)?;
    ensure!(
        owns_image_location(&event.location, session.as_deref(), turn.as_deref()),
        "The provider image does not belong to the submitted root turn"
    );
    let Some((_, _, status, GeneratedImageCapture::Saved(image))) = codex_image(event.data) else {
        anyhow::bail!("The provider did not save a recognized native image result")
    };
    let bytes = protection::unprotect(&bytes)?;
    ensure!(
        status.as_deref() == Some("completed")
            && bytes.len() == image.byte_len
            && format!("{:x}", Sha256::digest(&bytes)) == image.sha256,
        "The protected image does not match its completed provider receipt"
    );

    let assessed = snapshot
        .capabilities
        .as_ref()
        .context("No effective capability scope was recorded")?;
    let scope = &assessed.scope;
    let admitted = &snapshot.scope;
    ensure!(
        admitted.provider == Provider::Codex
            && scope.provider == Provider::Codex
            && admitted.runner == DirectWindowsRunner.identity()
            && admitted.permission_revision == DESKTOP_PERMISSION_REVISION
            && admitted
                .account_binding
                .as_deref()
                .is_some_and(|binding| !binding.is_empty())
            && admitted.account_binding == scope.account_binding
            && scope.profile_id == admitted.profile_id
            && scope.runner == admitted.runner
            && scope.cli_version == admitted.cli_version
            && scope.mode == admitted.mode
            && scope.permission_revision == admitted.permission_revision
            && scope.requested_model == admitted.model
            && scope.requested_effort == admitted.effort
            && scope.checkout.id == checkout
            && scope.checkout.path == admitted.checkout,
        "The image requires a verified desktop Codex profile and its recorded checkout scope"
    );
    validate_dispatch_boundary(db, run, sequence, &dispatch, session.as_deref(), scope)?;
    native_image_generation_scope_key(scope)?
        .context("The exact native image generation scope could not be qualified")
}

fn validate_dispatch_boundary(
    db: &Connection,
    run: &str,
    sequence: i64,
    dispatch: &str,
    session: Option<&str>,
    scope: &crate::providers::capabilities::CapabilityScope,
) -> Result<()> {
    let mut statements = db.prepare(
        "SELECT source, payload FROM agent_events WHERE run_id = ?1 AND seq < ?2
         AND source IN ('application', 'provider') ORDER BY seq",
    )?;
    let mut rows = statements.query(params![run, sequence])?;
    let mut submitted = false;
    while let Some(row) = rows.next()? {
        let source = row.get::<_, String>(0)?;
        let payload = row.get_ref(1)?.as_blob()?;
        let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(payload)?)?;
        if source == "application" {
            submitted |= matches!(event.data, ProviderEventData::DispatchSubmitted { dispatch_id } if dispatch_id == dispatch);
        } else if submitted
            && event.location.session_id.as_deref() == session
            && event.location.parent_item_id.is_none()
            && let ProviderEventData::SessionReady { model, effort }
            | ProviderEventData::ModelObserved { model, effort } = event.data
        {
            ensure!(
                model.is_none_or(|model| Some(model) == scope.reported_model)
                    && effort.is_none_or(|effort| Some(effort) == scope.reported_effort),
                "The provider changed the effective model or effort before generating this image"
            );
        }
    }
    ensure!(
        submitted,
        "The image was recorded before the verified prompt submission"
    );
    Ok(())
}
