use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{
    jobs::side_requests::can_validate_recorded_side_source,
    model_catalog::ModelSelection,
    protection,
    providers::{
        Provider, SessionMode,
        capabilities::{
            CapabilityStatus, CapabilitySubject, EvidenceBoundary, EvidenceOrigin, ObservedResult,
            ResolvedCapability,
        },
    },
    runner::ExecutionPlatform,
    store::Store,
};

use super::{ContextSnapshot, profile::same_profile};

const PAGE_SIZE: usize = 12;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationCursor {
    recorded_at: i64,
    run_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationPage {
    records: Vec<RecordedProfile>,
    next_cursor: Option<QualificationCursor>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedProfile {
    run_id: String,
    recorded_at: i64,
    provider: Provider,
    cli_version: String,
    requested_model: Option<String>,
    reported_model: Option<String>,
    platform: Option<ExecutionPlatform>,
    runner: String,
    mode: SessionMode,
    capabilities: Vec<RecordedCapability>,
    validation: Option<ValidationTarget>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidationTarget {
    conversation_id: String,
    source_run_id: String,
    selection: ModelSelection,
    effort: Option<String>,
    mode: SessionMode,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedCapability {
    subject: CapabilitySubject,
    boundary: EvidenceBoundary,
    status: RecordedStatus,
    observed: Option<ObservedResult>,
    evidence: Option<EvidenceSummary>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordedStatus {
    Blocked,
    Verified,
    FixturePassed,
    Observed,
    Reported,
    Unverified,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceSummary {
    origin: EvidenceOrigin,
    checked_at: String,
    result: ObservedResult,
}

impl From<ResolvedCapability> for RecordedCapability {
    fn from(entry: ResolvedCapability) -> Self {
        let status = match entry.status() {
            CapabilityStatus::Blocked => RecordedStatus::Blocked,
            CapabilityStatus::Verified
                if entry.evidence.as_ref().is_some_and(|evidence| {
                    evidence.origin == EvidenceOrigin::AuthenticatedProviderRun
                }) =>
            {
                RecordedStatus::Verified
            }
            CapabilityStatus::Verified => RecordedStatus::FixturePassed,
            CapabilityStatus::Observed => RecordedStatus::Observed,
            CapabilityStatus::Reported => RecordedStatus::Reported,
            CapabilityStatus::Unverified => RecordedStatus::Unverified,
            CapabilityStatus::Unavailable => RecordedStatus::Unavailable,
        };
        Self {
            subject: entry.descriptor.subject,
            boundary: entry.descriptor.boundary,
            observed: entry.descriptor.observed,
            status,
            evidence: entry.evidence.map(|evidence| EvidenceSummary {
                origin: evidence.origin,
                checked_at: evidence.checked_at,
                result: evidence.result,
            }),
        }
    }
}

impl Store {
    pub async fn desktop_capability_qualifications(
        &self,
        cursor: Option<QualificationCursor>,
        validation_checkout_id: Option<String>,
    ) -> Result<QualificationPage> {
        self.capability_qualifications(cursor, validation_checkout_id, None)
            .await
    }

    pub async fn provider_capability_qualifications(
        &self,
        provider: Provider,
        cursor: Option<QualificationCursor>,
        checkout_id: Option<String>,
    ) -> Result<QualificationPage> {
        self.capability_qualifications(cursor, checkout_id, Some(provider))
            .await
    }

    async fn capability_qualifications(
        &self,
        cursor: Option<QualificationCursor>,
        validation_checkout_id: Option<String>,
        provider: Option<Provider>,
    ) -> Result<QualificationPage> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "WITH page AS MATERIALIZED (
                    SELECT r.id, r.created_at, r.checkout_id, s.id AS snapshot_id
                    FROM runs r
                    JOIN context_snapshots s ON s.run_id = r.id
                    JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                    WHERE d.delivery = 'submitted'
                      AND (?1 IS NULL OR (r.created_at, r.id) < (?1, ?2))
                      AND (?4 IS NULL OR ((?5 IS NULL OR r.checkout_id = ?5)
                      AND EXISTS (SELECT 1 FROM agent_attempts a
                         JOIN agent_sessions s ON s.id = a.agent_session_id
                         WHERE a.run_id = r.id AND json_extract(s.scope, '$.provider') = ?4)))
                    ORDER BY r.created_at DESC, r.id DESC LIMIT ?3
                 )
                 SELECT page.id, page.created_at, page.checkout_id, a.conversation_id, s.payload
                 FROM page JOIN context_snapshots s ON s.id = page.snapshot_id
                 JOIN agent_attempts a ON a.run_id = page.id
                 ORDER BY page.created_at DESC, page.id DESC",
            )?;
            let (stamp, id) = cursor.as_ref().map_or((None, None), |cursor| {
                (Some(cursor.recorded_at), Some(cursor.run_id.as_str()))
            });
            let mut rows = query.query(params![
                stamp,
                id,
                PAGE_SIZE as u32 + 1,
                provider.map(Provider::as_str),
                validation_checkout_id
            ])?;
            let mut records = Vec::with_capacity(PAGE_SIZE);
            while records.len() < PAGE_SIZE
                && let Some(row) = rows.next()?
            {
                let run_id: String = row.get(0)?;
                let recorded_at = row.get(1)?;
                let checkout_id: String = row.get(2)?;
                let conversation_id: String = row.get(3)?;
                let snapshot: ContextSnapshot =
                    serde_json::from_slice(&protection::unprotect(row.get_ref(4)?.as_blob()?)?)?;
                let scope = &snapshot.scope;
                let validation = if validation_checkout_id.as_deref() == Some(checkout_id.as_str())
                    && let Some(model_id) = &scope.model
                    && can_validate_recorded_side_source(db, &conversation_id, &run_id, &snapshot)?
                {
                    Some(ValidationTarget {
                        conversation_id,
                        source_run_id: run_id.clone(),
                        selection: ModelSelection {
                            provider: scope.provider,
                            profile_id: scope.profile_id.clone(),
                            model_id: model_id.clone(),
                        },
                        effort: scope.effort.clone(),
                        mode: scope.mode,
                    })
                } else {
                    None
                };
                let scope = snapshot.scope;
                let assessment = snapshot
                    .capabilities
                    .filter(|assessment| same_profile(&assessment.scope, &scope, &checkout_id));
                let (reported_model, platform, capabilities) = assessment.map_or_else(
                    || (None, None, Vec::new()),
                    |assessment| {
                        (
                            assessment.scope.reported_model,
                            assessment.scope.platform,
                            assessment.entries.into_iter().map(Into::into).collect(),
                        )
                    },
                );
                records.push(RecordedProfile {
                    run_id,
                    recorded_at,
                    provider: scope.provider,
                    cli_version: scope.cli_version,
                    requested_model: scope.model,
                    reported_model,
                    platform,
                    runner: scope.runner,
                    mode: scope.mode,
                    capabilities,
                    validation,
                });
            }
            let next_cursor = if rows.next()?.is_some() {
                records.last().map(|record| QualificationCursor {
                    recorded_at: record.recorded_at,
                    run_id: record.run_id.clone(),
                })
            } else {
                None
            };
            Ok(QualificationPage {
                records,
                next_cursor,
            })
        })
        .await
    }
}
