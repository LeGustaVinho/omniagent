use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{
    protection,
    providers::{
        ProviderEvent, ProviderEventData, ProviderLocation,
        generated_image::{GeneratedImageCapture, GeneratedImageMime, ImageCaptureIssue},
    },
    store::Store,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImageEvidence {
    pub event_sequence: String,
    pub item_id: String,
    pub tool: String,
    pub producing_model: Option<String>,
    pub status: Option<String>,
    pub mime: Option<GeneratedImageMime>,
    pub byte_len: Option<usize>,
    pub sha256: Option<String>,
    pub unavailable_reason: Option<ImageCaptureIssue>,
}

pub(super) fn generated_image_metadata(
    sequence: i64,
    location: ProviderLocation,
    data: ProviderEventData,
) -> Option<GeneratedImageEvidence> {
    let (tool, producing_model, status, image) = codex_image(data)?;
    let (mime, byte_len, sha256, unavailable_reason) = match image {
        GeneratedImageCapture::Saved(image) => (
            Some(image.mime),
            Some(image.byte_len),
            Some(image.sha256),
            None,
        ),
        GeneratedImageCapture::Unavailable { reason } => (None, None, None, Some(reason)),
    };
    Some(GeneratedImageEvidence {
        event_sequence: sequence.to_string(),
        item_id: location.item_id?,
        tool,
        producing_model,
        status,
        mime,
        byte_len,
        sha256,
        unavailable_reason,
    })
}

fn codex_image(
    data: ProviderEventData,
) -> Option<(
    String,
    Option<String>,
    Option<String>,
    GeneratedImageCapture,
)> {
    match data {
        ProviderEventData::MediaGeneration {
            media_type,
            tool,
            producing_model,
            status,
            image: Some(image),
        } if media_type == "image" && tool == "imageGeneration" => {
            Some((tool, producing_model, status, image))
        }
        _ => None,
    }
}

pub(super) fn owns_image_location(
    location: &ProviderLocation,
    session: Option<&str>,
    turn: Option<&str>,
) -> bool {
    location.parent_item_id.is_none()
        && location.item_id.as_ref().is_some_and(|id| !id.is_empty())
        && session.is_some_and(|id| !id.is_empty() && location.session_id.as_deref() == Some(id))
        && turn.is_some_and(|id| !id.is_empty() && location.turn_id.as_deref() == Some(id))
}

impl Store {
    pub async fn dispatch_generated_image(
        &self,
        project_id: String,
        conversation_id: String,
        run_id: String,
        sequence: i64,
    ) -> Result<Option<String>> {
        self.execute(move |db| {
            let record = db.query_row(
                "SELECT d.id, s.provider_session_id, a.provider_turn_id, e.payload
                 FROM agent_events e JOIN runs r ON r.id = e.run_id
                 JOIN checkouts c ON c.id = r.checkout_id
                 JOIN agent_attempts a ON a.run_id = r.id
                 JOIN agent_sessions s ON s.id = a.agent_session_id
                 JOIN context_snapshots i ON i.run_id = r.id
                 JOIN dispatches d ON d.snapshot_id = i.id AND d.ordinal = 1
                 WHERE c.project_id = ?1 AND a.conversation_id = ?2 AND r.id = ?3
                     AND e.seq = ?4 AND json_extract(s.scope, '$.provider') = 'codex'
                     AND e.source = 'provider' AND d.delivery = 'submitted'",
                params![project_id, conversation_id, run_id, sequence],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?, row.get::<_, Vec<u8>>(3)?)),
            ).optional()?;
            let Some((dispatch_id, session, turn, payload)) = record else { return Ok(None); };
            let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
            if !owns_image_location(&event.location, session.as_deref(), turn.as_deref()) {
                return Ok(None);
            }
            let Some((_, _, _, GeneratedImageCapture::Saved(_))) = codex_image(event.data) else { return Ok(None); };
            let mut statement = db.prepare(
                "SELECT payload FROM agent_events WHERE run_id = ?1 AND seq < ?2 AND source = 'application' ORDER BY seq"
            )?;
            let mut rows = statement.query(params![run_id, sequence])?;
            while let Some(row) = rows.next()? {
                let payload: Vec<u8> = row.get(0)?;
                let event: ProviderEvent = serde_json::from_slice(&protection::unprotect(&payload)?)?;
                if matches!(event.data, ProviderEventData::DispatchSubmitted { dispatch_id: id } if id == dispatch_id) {
                    let image: Option<Vec<u8>> = db.query_row(
                        "SELECT payload FROM generated_image_evidence_payloads WHERE event_seq = ?1",
                        [sequence], |row| row.get(0),
                    ).optional()?;
                    return image.map(|payload| Ok(STANDARD.encode(protection::unprotect(&payload)?))).transpose();
                }
            }
            Ok(None)
        }).await
    }
}
