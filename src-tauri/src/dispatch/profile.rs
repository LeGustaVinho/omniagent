use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{
    conversations::SessionScope,
    protection,
    providers::{
        Provider, ProviderEvent, ProviderEventData, SessionMode, Transport,
        capabilities::{
            CapabilityAssessment, CapabilityDescriptor, CapabilityScope, CapabilityStatus,
            Verification,
        },
    },
    store::Store,
};

use super::{ContextSnapshot, Delivery, EventSource};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchProfileEvidence {
    pub dispatch_id: String,
    pub snapshot_id: String,
    pub turn_id: String,
    pub delivery: Delivery,
    pub profile: DispatchProfile,
    pub assessment: Option<HistoricalAssessment>,
    pub model_observations: Vec<ModelObservation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchProfile {
    pub provider: Provider,
    pub profile_id: String,
    pub cli_version: String,
    pub runner: String,
    pub checkout_id: String,
    pub checkout_path: PathBuf,
    pub mode: SessionMode,
    pub permission_revision: String,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub reported_at_dispatch: ObservedModel,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalAssessment {
    pub version: u16,
    pub transport: Transport,
    pub executable_revision: Option<String>,
    pub configuration_revision: Option<String>,
    pub provider_revision: Option<String>,
    pub capabilities: Vec<HistoricalCapability>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalCapability {
    pub descriptor: CapabilityDescriptor,
    pub status: CapabilityStatus,
    pub evidence: Option<Verification>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ObservedModel {
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelObservation {
    pub event_sequence: String,
    pub reported: ObservedModel,
    pub reassessment: Option<ModelReassessment>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReassessment {
    pub event_sequence: String,
    pub assessment: HistoricalAssessment,
}

impl Store {
    pub async fn dispatch_profile_evidence(
        &self,
        project_id: String,
        conversation_id: String,
        run_id: String,
    ) -> Result<Option<DispatchProfileEvidence>> {
        let record = self
            .execute({
                let run_id = run_id.clone();
                move |db| {
                    Ok(db
                        .query_row(
                            "SELECT d.id, s.id, d.turn_id, d.delivery, s.payload, r.checkout_id,
                                coalesce((SELECT max(seq) FROM agent_events WHERE run_id = r.id), 0)
                         FROM runs r
                         JOIN checkouts c ON c.id = r.checkout_id
                         JOIN agent_attempts a ON a.run_id = r.id
                         JOIN context_snapshots s ON s.run_id = r.id
                         JOIN dispatches d ON d.snapshot_id = s.id AND d.ordinal = 1
                         WHERE c.project_id = ?1 AND a.conversation_id = ?2 AND r.id = ?3",
                            params![project_id, conversation_id, run_id],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?,
                                    row.get::<_, String>(3)?,
                                    row.get::<_, Vec<u8>>(4)?,
                                    row.get::<_, String>(5)?,
                                    row.get::<_, i64>(6)?,
                                ))
                            },
                        )
                        .optional()?)
                }
            })
            .await?;
        let Some((dispatch_id, snapshot_id, turn_id, delivery, payload, checkout_id, through)) =
            record
        else {
            return Ok(None);
        };
        let delivery = serde_json::from_value(serde_json::Value::String(delivery))?;
        let snapshot: ContextSnapshot = serde_json::from_slice(&protection::unprotect(&payload)?)?;
        let reported_at_dispatch = snapshot
            .capabilities
            .as_ref()
            .map(|assessment| reported_model(&assessment.scope))
            .unwrap_or_default();
        let model_observations = if delivery == Delivery::Submitted {
            self.dispatch_model_observations(
                run_id,
                through,
                &dispatch_id,
                &snapshot.scope,
                &checkout_id,
                reported_at_dispatch.clone(),
            )
            .await?
        } else {
            Vec::new()
        };
        let scope = snapshot.scope;
        Ok(Some(DispatchProfileEvidence {
            dispatch_id,
            snapshot_id,
            turn_id,
            delivery,
            profile: DispatchProfile {
                provider: scope.provider,
                profile_id: scope.profile_id,
                cli_version: scope.cli_version,
                runner: scope.runner,
                checkout_id,
                checkout_path: scope.checkout,
                mode: scope.mode,
                permission_revision: scope.permission_revision,
                requested_model: scope.model,
                requested_effort: scope.effort,
                reported_at_dispatch,
            },
            assessment: snapshot.capabilities.map(historical_assessment),
            model_observations,
        }))
    }

    async fn dispatch_model_observations(
        &self,
        run_id: String,
        through: i64,
        dispatch_id: &str,
        scope: &SessionScope,
        checkout_id: &str,
        mut reported: ObservedModel,
    ) -> Result<Vec<ModelObservation>> {
        let mut cursor = 0;
        let mut submitted = false;
        let mut observations = Vec::<ModelObservation>::new();
        // Read a bounded page at a time so decrypting a long run does not monopolize the store.
        while cursor < through {
            let events = self
                .execute({
                    let run_id = run_id.clone();
                    move |db| {
                        let mut query = db.prepare(
                            "SELECT seq, source, payload FROM agent_events
                         WHERE run_id = ?1 AND seq > ?2 AND seq <= ?3
                             AND source IN ('application', 'provider')
                         ORDER BY seq LIMIT 128",
                        )?;
                        let events = query.query_map(params![run_id, cursor, through], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Vec<u8>>(2)?,
                            ))
                        })?;
                        Ok(events.collect::<rusqlite::Result<Vec<_>>>()?)
                    }
                })
                .await?;
            if events.is_empty() {
                break;
            }
            for (seq, source, payload) in events {
                cursor = seq;
                let application = source == EventSource::Application.as_sql();
                if !submitted && !application {
                    continue;
                }
                let event: ProviderEvent =
                    serde_json::from_slice(&protection::unprotect(&payload)?)?;
                if !submitted {
                    submitted = matches!(event.data, ProviderEventData::DispatchSubmitted { dispatch_id: id } if id == dispatch_id);
                    continue;
                }
                if event.location.parent_item_id.is_some() {
                    continue;
                }
                match event.data {
                    ProviderEventData::SessionReady { model, effort }
                    | ProviderEventData::ModelObserved { model, effort }
                        if !application =>
                    {
                        let next = ObservedModel {
                            model: model.or_else(|| reported.model.clone()),
                            effort: effort.or_else(|| reported.effort.clone()),
                        };
                        if next != reported {
                            reported = next.clone();
                            observations.push(ModelObservation {
                                event_sequence: seq.to_string(),
                                reported: next,
                                reassessment: None,
                            });
                        }
                    }
                    ProviderEventData::CapabilityAssessmentUpdated { assessment }
                        if application =>
                    {
                        if let Some(observation) = observations.last_mut()
                            && observation.reassessment.is_none()
                            && reported_model(&assessment.scope) == observation.reported
                            && same_profile(&assessment.scope, scope, checkout_id)
                        {
                            observation.reassessment = Some(ModelReassessment {
                                event_sequence: seq.to_string(),
                                assessment: historical_assessment(*assessment),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(observations)
    }
}

fn reported_model(scope: &CapabilityScope) -> ObservedModel {
    ObservedModel {
        model: scope.reported_model.clone(),
        effort: scope.reported_effort.clone(),
    }
}

fn same_profile(assessment: &CapabilityScope, selected: &SessionScope, checkout_id: &str) -> bool {
    assessment.provider == selected.provider
        && assessment.profile_id == selected.profile_id
        && assessment.cli_version == selected.cli_version
        && assessment.account_binding == selected.account_binding
        && assessment.requested_model == selected.model
        && assessment.requested_effort == selected.effort
        && assessment.runner == selected.runner
        && assessment.mode == selected.mode
        && assessment.permission_revision == selected.permission_revision
        && assessment.checkout.id == checkout_id
        && assessment.checkout.path == selected.checkout
}

fn historical_assessment(assessment: CapabilityAssessment) -> HistoricalAssessment {
    HistoricalAssessment {
        version: assessment.version,
        transport: assessment.scope.transport,
        executable_revision: assessment.scope.executable_revision,
        configuration_revision: assessment.scope.configuration.effective_revision,
        provider_revision: assessment.scope.configuration.provider_revision,
        capabilities: assessment
            .entries
            .into_iter()
            .map(|entry| HistoricalCapability {
                status: entry.status(),
                descriptor: entry.descriptor,
                evidence: entry.evidence,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, process::Command};

    use anyhow::ensure;

    use super::*;
    use crate::{
        dispatch::NativeHistory,
        providers::{
            Capability, ProviderLocation,
            capabilities::{
                Availability, CAPABILITY_SCHEMA_VERSION, CapabilitySubject, ConfigurationEvidence,
                DeclaredSource, EvidenceBoundary, EvidenceOrigin, ObservedResult, PolicyBlocker,
                ResolvedCapability,
            },
            questions::QuestionRoute,
        },
        runs::RunStatus,
    };

    #[tokio::test]
    async fn reads_frozen_profile_and_separates_later_provider_reports_after_restart() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let checkout = dunce::canonicalize(directory.path())?;
        ensure!(
            Command::new("git.exe")
                .args(["init", "--quiet"])
                .current_dir(&checkout)
                .status()?
                .success()
        );
        let path = checkout.join("profile-evidence.sqlite");
        let store = Store::open(path.clone())?;
        let registered = crate::repository::register(&store, checkout.clone()).await?;
        let conversation = store
            .create_conversation(registered.checkout_id.clone(), "Review".into())
            .await?;
        let other_conversation = store
            .create_conversation(registered.checkout_id.clone(), "Other".into())
            .await?;
        let scope = SessionScope {
            provider: Provider::Codex,
            profile_id: "review-profile".into(),
            account_binding: Some("internal-account-binding".into()),
            executable: checkout.join("codex.exe"),
            checkout,
            cli_version: "test-cli".into(),
            runner: "test-runner".into(),
            permission_revision: "policy-original".into(),
            mode: SessionMode::ReadOnly,
            model: Some("requested-model".into()),
            effort: Some("medium".into()),
        };
        let attempt = store
            .prepare_agent_attempt(
                conversation.id.clone(),
                conversation.context_id,
                scope.clone(),
                "protected private prompt".into(),
            )
            .await?;
        let run_id = attempt.run.id.clone();
        store.mark_agent_running(run_id.clone()).await?;

        let at_dispatch = CapabilityAssessment {
            version: CAPABILITY_SCHEMA_VERSION,
            scope: CapabilityScope::from_dispatch(
                &scope,
                attempt.run.checkout_id.clone(),
                Some("original-executable".into()),
                None,
                ConfigurationEvidence {
                    detected: Vec::new(),
                    unresolved: Vec::new(),
                    provider_revision: Some("original-provider".into()),
                    effective_revision: Some("original-configuration".into()),
                    scoped_launch_revision: None,
                    environment_revision: None,
                    tool_revisions: BTreeMap::new(),
                },
                Some("effective-at-dispatch".into()),
                Some("low".into()),
            ),
            entries: vec![
                ResolvedCapability {
                    descriptor: CapabilityDescriptor {
                        subject: CapabilitySubject::Protocol {
                            capability: Capability::Reasoning,
                        },
                        source: DeclaredSource::Provider,
                        availability: Availability::Available,
                        observed: None,
                        blocker: None,
                        boundary: EvidenceBoundary::ExactCheckout,
                    },
                    evidence: Some(Verification {
                        id: "saved-verification".into(),
                        checked_at: "2026-01-01T00:00:00Z".into(),
                        origin: EvidenceOrigin::AuthenticatedFixture,
                        result: ObservedResult::Succeeded,
                        artifact_id: "saved-artifact".into(),
                        attachment_limits: None,
                    }),
                },
                ResolvedCapability {
                    descriptor: CapabilityDescriptor {
                        subject: CapabilitySubject::Protocol {
                            capability: Capability::WorkspaceWrite,
                        },
                        source: DeclaredSource::Policy,
                        availability: Availability::Available,
                        observed: None,
                        blocker: Some(PolicyBlocker::ReadOnlyProfile),
                        boundary: EvidenceBoundary::ExactCheckout,
                    },
                    evidence: None,
                },
            ],
        };
        let dispatch_id = store
            .prepare_dispatch(
                run_id.clone(),
                ContextSnapshot {
                    version: 13,
                    context_id: attempt.context_id,
                    scope,
                    session_id: None,
                    job_policy_revision: None,
                    role_context: None,
                    base_commit: None,
                    submitted_messages: attempt.input,
                    native_history: NativeHistory::Fresh,
                    capabilities: Some(at_dispatch.clone()),
                    filesystem_skills: Vec::new(),
                    reported_skills: None,
                    instructions: None,
                    scoped_launch: None,
                    requested_skill: None,
                    plugin_inventory: None,
                    attachments: Vec::new(),
                    task_context: None,
                    repository_markdown: None,
                    question_route: QuestionRoute::Native,
                    answered_question: None,
                },
                vec![b"private wire bytes".to_vec()],
            )
            .await?;
        store
            .settle_dispatch(dispatch_id.clone(), Delivery::Submitted)
            .await?;

        let event = |source, data| {
            store.record_agent_observation(
                run_id.clone(),
                source,
                ProviderEvent {
                    location: ProviderLocation::default(),
                    data,
                },
            )
        };
        let marker = |id: String| ProviderEventData::DispatchSubmitted { dispatch_id: id };
        let report = |model: &str| ProviderEventData::ModelObserved {
            model: Some(model.into()),
            effort: None,
        };
        event(EventSource::Provider, marker(dispatch_id.clone())).await?;
        event(EventSource::Application, marker("another-dispatch".into())).await?;
        event(EventSource::Provider, report("pre-marker")).await?;
        event(EventSource::Application, marker(dispatch_id.clone())).await?;
        event(EventSource::Application, report("application-metadata")).await?;
        store
            .record_agent_observation(
                run_id.clone(),
                EventSource::Provider,
                ProviderEvent {
                    location: ProviderLocation {
                        parent_item_id: Some("delegated".into()),
                        ..ProviderLocation::default()
                    },
                    data: report("delegated-model"),
                },
            )
            .await?;
        event(EventSource::Provider, report("effective-later")).await?;

        let updated = |assessment| ProviderEventData::CapabilityAssessmentUpdated {
            assessment: Box::new(assessment),
        };
        let mut later = at_dispatch.clone();
        later.scope.reported_model = Some("effective-later".into());
        later.scope.configuration.effective_revision = Some("later-configuration".into());
        later.entries[0].evidence = None;
        let mut unrelated = later.clone();
        unrelated.scope.checkout.id = "another-checkout".into();
        event(EventSource::Application, updated(unrelated)).await?;
        event(EventSource::Provider, updated(at_dispatch.clone())).await?;
        event(EventSource::Application, updated(later.clone())).await?;
        event(EventSource::Provider, report("effective-later")).await?;
        event(EventSource::Provider, report("effective-at-dispatch")).await?;
        store
            .finish_agent_attempt(run_id.clone(), RunStatus::Interrupted, None)
            .await?;
        drop(store);

        let store = Store::open(path)?;
        assert!(
            store
                .dispatch_profile_evidence(
                    "other-project".into(),
                    conversation.id.clone(),
                    run_id.clone()
                )
                .await?
                .is_none()
        );
        assert!(
            store
                .dispatch_profile_evidence(
                    attempt.run.project_id.clone(),
                    other_conversation.id,
                    run_id.clone()
                )
                .await?
                .is_none()
        );
        let saved = store
            .dispatch_profile_evidence(attempt.run.project_id, conversation.id, run_id)
            .await?
            .expect("the authorized dispatch profile");
        assert_eq!(saved.dispatch_id, dispatch_id);
        assert_eq!(saved.turn_id, attempt.run.turn_id);
        assert_eq!(
            saved.profile.requested_model.as_deref(),
            Some("requested-model")
        );
        assert_eq!(
            saved.profile.reported_at_dispatch.model.as_deref(),
            Some("effective-at-dispatch")
        );
        assert_eq!(saved.assessment, Some(historical_assessment(at_dispatch)));
        assert_eq!(saved.model_observations.len(), 2);
        assert_eq!(
            saved.model_observations[0].reported.model.as_deref(),
            Some("effective-later")
        );
        assert_eq!(
            saved.model_observations[0]
                .reassessment
                .as_ref()
                .map(|item| &item.assessment),
            Some(&historical_assessment(later))
        );
        assert_eq!(
            saved.model_observations[1].reported.model.as_deref(),
            Some("effective-at-dispatch")
        );
        assert!(saved.model_observations[1].reassessment.is_none());
        let serialized = serde_json::to_string(&saved)?;
        for input in [
            "internal-account-binding",
            "protected private prompt",
            "private wire bytes",
        ] {
            assert!(!serialized.contains(input));
        }
        Ok(())
    }
}
