use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    attachments::{
        AttachmentContextUsage, AttachmentFormat, NativeAttachmentEncoding,
        QualifiedAttachmentLimits,
    },
    conversations::SessionScope,
    mcp_bridge::{INSPECT_SCOPE, Registration, SERVER_NAME, VERSION},
    protection,
    runner::ExecutionPlatform,
    store::Store,
};

use super::{
    Capability, Provider, ProviderEventData, SessionMode, Transport, configuration::mcp_identity,
};

pub const CAPABILITY_SCHEMA_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CapabilitySubject {
    Protocol {
        capability: Capability,
    },
    Tool {
        id: String,
        version: String,
        operation: String,
    },
    NativeAttachment {
        format: AttachmentFormat,
        encoding: NativeAttachmentEncoding,
    },
    AttachmentLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclaredSource {
    Adapter,
    Provider,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Unknown,
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyBlocker {
    ReadOnlyProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedResult {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDescriptor {
    pub subject: CapabilitySubject,
    pub source: DeclaredSource,
    pub availability: Availability,
    pub observed: Option<ObservedResult>,
    pub blocker: Option<PolicyBlocker>,
    pub boundary: EvidenceBoundary,
}

impl CapabilityDescriptor {
    pub fn native_attachment(format: AttachmentFormat, encoding: NativeAttachmentEncoding) -> Self {
        Self {
            subject: CapabilitySubject::NativeAttachment { format, encoding },
            source: DeclaredSource::Adapter,
            availability: Availability::Unknown,
            observed: None,
            blocker: None,
            boundary: EvidenceBoundary::ExactCheckout,
        }
    }

    pub fn attachment_limits() -> Self {
        Self {
            subject: CapabilitySubject::AttachmentLimits,
            source: DeclaredSource::Adapter,
            availability: Availability::Unknown,
            observed: None,
            blocker: None,
            boundary: EvidenceBoundary::ExactCheckout,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceBoundary {
    EquivalentConfiguration,
    ExactCheckout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOrigin {
    ProtocolFixture,
    AuthenticatedFixture,
}

impl EvidenceOrigin {
    fn as_sql(self) -> &'static str {
        match self {
            Self::ProtocolFixture => "protocol_fixture",
            Self::AuthenticatedFixture => "authenticated_fixture",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    pub id: String,
    pub checked_at: String,
    pub origin: EvidenceOrigin,
    pub result: ObservedResult,
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_limits: Option<QualifiedAttachmentLimits>,
}

impl Verification {
    pub fn verified(&self) -> bool {
        self.origin == EvidenceOrigin::AuthenticatedFixture
            && self.result == ObservedResult::Succeeded
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCapability {
    pub descriptor: CapabilityDescriptor,
    pub evidence: Option<Verification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    Blocked,
    Verified,
    Observed,
    Reported,
    Unverified,
    Unavailable,
}

impl ResolvedCapability {
    pub fn status(&self) -> CapabilityStatus {
        if self.descriptor.blocker.is_some() {
            CapabilityStatus::Blocked
        } else if self.descriptor.availability == Availability::Unavailable {
            CapabilityStatus::Unavailable
        } else if self.evidence.as_ref().is_some_and(Verification::verified) {
            CapabilityStatus::Verified
        } else if self.descriptor.observed.is_some() {
            CapabilityStatus::Observed
        } else {
            match (self.descriptor.source, self.descriptor.availability) {
                (DeclaredSource::Provider, Availability::Available) => CapabilityStatus::Reported,
                _ => CapabilityStatus::Unverified,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutIdentity {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationEvidence {
    pub detected: Vec<ConfigurationFile>,
    pub unresolved: Vec<PathBuf>,
    pub provider_revision: Option<String>,
    /// Provider configuration plus detected source revisions. An unknown
    /// provider configuration cannot qualify for evidence reuse.
    pub effective_revision: Option<String>,
    #[serde(
        default,
        alias = "attachmentRevision",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) scoped_launch_revision: Option<String>,
    #[serde(default)]
    pub environment_revision: Option<String>,
    #[serde(default)]
    pub tool_revisions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationFile {
    pub path: PathBuf,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) environment_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityScope {
    pub provider: Provider,
    pub transport: Transport,
    pub cli_version: String,
    pub executable_revision: Option<String>,
    pub profile_id: String,
    pub account_binding: Option<String>,
    pub requested_model: Option<String>,
    pub reported_model: Option<String>,
    pub requested_effort: Option<String>,
    pub reported_effort: Option<String>,
    pub runner: String,
    pub platform: Option<ExecutionPlatform>,
    pub mode: SessionMode,
    pub permission_revision: String,
    pub checkout: CheckoutIdentity,
    pub configuration: ConfigurationEvidence,
}

impl CapabilityScope {
    pub(crate) fn from_dispatch(
        scope: &SessionScope,
        checkout_id: String,
        executable_revision: Option<String>,
        platform: Option<ExecutionPlatform>,
        configuration: ConfigurationEvidence,
        reported_model: Option<String>,
        reported_effort: Option<String>,
    ) -> Self {
        Self {
            provider: scope.provider,
            transport: scope.provider.adapter().transport(),
            cli_version: scope.cli_version.clone(),
            executable_revision,
            profile_id: scope.profile_id.clone(),
            account_binding: scope.account_binding.clone(),
            requested_model: scope.model.clone(),
            reported_model,
            requested_effort: scope.effort.clone(),
            reported_effort,
            runner: scope.runner.clone(),
            platform,
            mode: scope.mode,
            permission_revision: scope.permission_revision.clone(),
            checkout: CheckoutIdentity {
                id: checkout_id,
                path: scope.checkout.clone(),
            },
            configuration,
        }
    }

    fn key(&self, descriptor: &CapabilityDescriptor) -> Option<QualificationKey<'_>> {
        let native_attachment = matches!(
            descriptor.subject,
            CapabilitySubject::NativeAttachment { .. }
        );
        let attachment_limits = descriptor.subject == CapabilitySubject::AttachmentLimits;
        let planning = matches!(descriptor.subject, CapabilitySubject::Protocol { capability } if capability.is_planning());
        if !native_attachment
            && !attachment_limits
            && !planning
            && !self.configuration.unresolved.is_empty()
        {
            return None;
        }
        let (configuration, tool_revision) = match &descriptor.subject {
            CapabilitySubject::NativeAttachment { format, encoding }
                if encoding.provider() == self.provider
                    && encoding.supports(*format)
                    && descriptor.boundary == EvidenceBoundary::ExactCheckout =>
            {
                ("native-attachment-v1", None)
            }
            CapabilitySubject::NativeAttachment { .. } => return None,
            CapabilitySubject::AttachmentLimits
                if descriptor.boundary == EvidenceBoundary::ExactCheckout =>
            {
                (self.configuration.scoped_launch_revision.as_deref()?, None)
            }
            CapabilitySubject::AttachmentLimits => return None,
            CapabilitySubject::Tool { id, .. } => (
                self.configuration.environment_revision.as_deref()?,
                Some(self.configuration.tool_revisions.get(id)?.as_str()),
            ),
            CapabilitySubject::Protocol {
                capability: Capability::RunScopedMcp,
            } => (
                self.configuration.environment_revision.as_deref()?,
                Some(
                    self.configuration
                        .tool_revisions
                        .get(&mcp_identity(SERVER_NAME))?
                        .as_str(),
                ),
            ),
            CapabilitySubject::Protocol {
                capability: Capability::WorkspaceWrite | Capability::ToolApprovals,
            } => (self.configuration.effective_revision.as_deref()?, None),
            CapabilitySubject::Protocol { capability } if capability.is_planning() => {
                if descriptor.boundary != EvidenceBoundary::ExactCheckout {
                    return None;
                }
                (self.configuration.scoped_launch_revision.as_deref()?, None)
            }
            CapabilitySubject::Protocol { .. } => {
                (self.configuration.environment_revision.as_deref()?, None)
            }
        };
        Some(QualificationKey {
            subject: descriptor.subject.clone(),
            provider: self.provider,
            transport: self.transport,
            cli_version: &self.cli_version,
            executable_revision: self.executable_revision.as_deref()?,
            profile_id: &self.profile_id,
            account: self.account_binding.as_deref()?,
            requested_model: self.requested_model.as_deref(),
            reported_model: self.reported_model.as_deref().or_else(|| {
                // Claude establishes the selected model when the first input arrives.
                if (native_attachment || attachment_limits || planning)
                    && self.provider == Provider::Claude
                {
                    self.requested_model.as_deref()
                } else {
                    None
                }
            })?,
            requested_effort: self.requested_effort.as_deref(),
            reported_effort: self.reported_effort.as_deref(),
            runner: &self.runner,
            platform: self.platform.as_ref()?,
            mode: self.mode,
            permission_revision: &self.permission_revision,
            configuration,
            tool_revision,
            boundary: descriptor.boundary,
            checkout: (descriptor.boundary == EvidenceBoundary::ExactCheckout)
                .then_some(&self.checkout),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QualificationKey<'a> {
    subject: CapabilitySubject,
    provider: Provider,
    transport: Transport,
    cli_version: &'a str,
    executable_revision: &'a str,
    profile_id: &'a str,
    account: &'a str,
    requested_model: Option<&'a str>,
    reported_model: &'a str,
    requested_effort: Option<&'a str>,
    reported_effort: Option<&'a str>,
    runner: &'a str,
    platform: &'a ExecutionPlatform,
    mode: SessionMode,
    permission_revision: &'a str,
    configuration: &'a str,
    tool_revision: Option<&'a str>,
    boundary: EvidenceBoundary,
    checkout: Option<&'a CheckoutIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityAssessment {
    pub version: u16,
    pub scope: CapabilityScope,
    pub entries: Vec<ResolvedCapability>,
}

#[derive(Default)]
pub(crate) struct ReportedCapabilities {
    available: BTreeSet<Capability>,
    observed: BTreeSet<Capability>,
    pub model: Option<String>,
    models: Vec<super::ReportedModel>,
    pub effort: Option<String>,
    pub configuration_reported: bool,
    pub configuration_revision: Option<String>,
    pub configuration_qualification: Option<super::configuration::ReportedConfiguration>,
    pub codex_instruction_settings: Option<super::configuration::CodexInstructionSettings>,
    pub bridge_registration: Option<Registration>,
    pub native_attachments: Vec<(AttachmentFormat, NativeAttachmentEncoding)>,
    pub context_usage: Option<AttachmentContextUsage>,
}

impl ReportedCapabilities {
    pub(crate) fn observe(&mut self, data: &ProviderEventData) -> bool {
        match data {
            ProviderEventData::BridgeRegistration { registration } => {
                let changed = self.bridge_registration.as_ref() != Some(registration);
                self.bridge_registration = Some(registration.clone());
                changed
            }
            ProviderEventData::Handshake { capabilities, .. } => {
                let before = self.available.len();
                self.available.extend(capabilities);
                before != self.available.len()
            }
            ProviderEventData::ConfigurationReported {
                revision,
                codex_instructions,
                qualification,
            } => {
                let changed = !self.configuration_reported
                    || self.configuration_revision != *revision
                    || self.configuration_qualification != *qualification;
                self.configuration_reported = true;
                self.configuration_revision = revision.clone();
                self.configuration_qualification = qualification.clone();
                self.codex_instruction_settings = codex_instructions.clone();
                changed
            }
            ProviderEventData::SessionReady { model, effort }
            | ProviderEventData::ModelObserved { model, effort } => {
                let changed = model
                    .as_ref()
                    .is_some_and(|model| Some(model) != self.model.as_ref())
                    || effort
                        .as_ref()
                        .is_some_and(|effort| Some(effort) != self.effort.as_ref());
                if let Some(model) = model {
                    self.model = Some(model.clone());
                }
                if let Some(effort) = effort {
                    self.effort = Some(effort.clone());
                }
                if changed {
                    self.observed.retain(|capability| {
                        matches!(
                            capability,
                            Capability::NativePlanMode
                                | Capability::FreshSession
                                | Capability::NativeTurnFork
                        )
                    });
                    self.context_usage = None;
                }
                changed
            }
            ProviderEventData::ModelInventory { models } => {
                let changed = self.models != *models;
                self.models.clone_from(models);
                self.observed.insert(Capability::ModelInventory) || changed
            }
            ProviderEventData::SessionOptions { .. } => {
                self.observed.insert(Capability::ModelInventory)
            }
            ProviderEventData::Reasoning { .. } => self.observed.insert(Capability::Reasoning),
            ProviderEventData::Approval { .. } => self.observed.insert(Capability::ToolApprovals),
            ProviderEventData::Question { .. } => {
                self.observed.insert(Capability::PlanningQuestions)
            }
            ProviderEventData::QuestionAnswerSubmitted { .. }
            | ProviderEventData::QuestionReplySettled {
                action: super::questions::QuestionAction::Accept,
                delivery: crate::dispatch::Delivery::Submitted,
                ..
            } => self.observed.insert(Capability::PlanningAnswers),
            ProviderEventData::PlanningModeConfirmed { .. } => {
                self.observed.insert(Capability::NativePlanMode)
            }
            ProviderEventData::PlanProposed { .. } => {
                self.observed.insert(Capability::NativePlanOutput)
            }
            ProviderEventData::FreshSessionOpened => self.observed.insert(Capability::FreshSession),
            ProviderEventData::NativeForkVerified { .. } => {
                self.observed.insert(Capability::NativeTurnFork)
            }
            ProviderEventData::DelegationControlsReported { .. } => {
                self.available.insert(Capability::NativeDelegationControls)
            }
            ProviderEventData::ChildActivity { child_id, .. } => {
                let changed = self.observed.insert(Capability::SubagentEvents);
                let identified = child_id.as_ref().is_some_and(|id| !id.trim().is_empty());
                (identified && self.observed.insert(Capability::PerChildEvents)) || changed
            }
            ProviderEventData::Compacted => self.observed.insert(Capability::NativeCompaction),
            ProviderEventData::NativeCompactionFinished {
                outcome: super::TurnOutcome::Completed,
            } => self.observed.insert(Capability::ManualNativeCompaction),
            ProviderEventData::ContextWindowUsage { used, size } => {
                self.context_usage = Some(AttachmentContextUsage {
                    used: *used,
                    size: *size,
                });
                false
            }
            ProviderEventData::Suggestion { .. } => {
                self.observed.insert(Capability::NativeSuggestions)
            }
            _ => false,
        }
    }

    pub(crate) fn image_inputs_reported(&self, model: Option<&str>) -> Option<bool> {
        let model = model?;
        let row = self
            .models
            .iter()
            .find(|row| row.id == model || row.canonical_id.as_deref() == Some(model))?;
        row.input_modalities
            .as_ref()
            .map(|items| items.iter().any(|item| item.eq_ignore_ascii_case("image")))
    }

    pub(crate) fn descriptors(&self, scope: &CapabilityScope) -> Vec<CapabilityDescriptor> {
        let bridge_availability = match self.bridge_registration {
            Some(Registration::ToolsListed | Registration::ScopeVerified) => {
                Availability::Available
            }
            Some(Registration::Unavailable { .. }) => Availability::Unavailable,
            _ => Availability::Unknown,
        };
        let mut descriptors: Vec<_> = Capability::ALL
            .into_iter()
            .map(|capability| {
                if capability == Capability::RunScopedMcp {
                    return CapabilityDescriptor {
                        subject: CapabilitySubject::Protocol { capability },
                        source: DeclaredSource::Adapter,
                        availability: bridge_availability,
                        observed: (bridge_availability == Availability::Available)
                            .then_some(ObservedResult::Succeeded),
                        blocker: None,
                        boundary: EvidenceBoundary::ExactCheckout,
                    };
                }
                let observed = self.observed.contains(&capability);
                let announced = self.available.contains(&capability);
                let unavailable = match scope.provider {
                    Provider::Codex => false,
                    Provider::Claude => matches!(
                        capability,
                        Capability::NativeTurnFork | Capability::ManualNativeCompaction
                    ),
                    Provider::Antigravity => matches!(
                        capability,
                        Capability::NativeTurnFork
                            | Capability::ManualNativeCompaction
                            | Capability::NativeDelegationControls
                    ),
                };
                let blocked = capability == Capability::WorkspaceWrite
                    && scope.mode != SessionMode::ApprovedWrite;
                CapabilityDescriptor {
                    subject: CapabilitySubject::Protocol { capability },
                    source: if blocked {
                        DeclaredSource::Policy
                    } else if announced || observed {
                        DeclaredSource::Provider
                    } else {
                        DeclaredSource::Adapter
                    },
                    availability: if announced || observed {
                        Availability::Available
                    } else if unavailable {
                        Availability::Unavailable
                    } else {
                        Availability::Unknown
                    },
                    observed: observed.then_some(ObservedResult::Succeeded),
                    blocker: blocked.then_some(PolicyBlocker::ReadOnlyProfile),
                    boundary: if capability == Capability::WorkspaceWrite
                        || capability.is_planning()
                        || matches!(
                            capability,
                            Capability::NativeTurnFork | Capability::NativeDelegationControls
                        ) {
                        EvidenceBoundary::ExactCheckout
                    } else {
                        EvidenceBoundary::EquivalentConfiguration
                    },
                }
            })
            .collect();
        if self.bridge_registration.is_some() {
            descriptors.push(CapabilityDescriptor {
                subject: CapabilitySubject::Tool {
                    id: mcp_identity(SERVER_NAME),
                    version: format!("{}:{VERSION}", env!("CARGO_PKG_VERSION")),
                    operation: INSPECT_SCOPE.into(),
                },
                source: DeclaredSource::Adapter,
                availability: bridge_availability,
                observed: (self.bridge_registration == Some(Registration::ScopeVerified))
                    .then_some(ObservedResult::Succeeded),
                blocker: None,
                boundary: EvidenceBoundary::ExactCheckout,
            });
        }
        for &(format, encoding) in &self.native_attachments {
            let mut descriptor = CapabilityDescriptor::native_attachment(format, encoding);
            let advertised = match scope.provider {
                Provider::Codex => self.image_inputs_reported(
                    scope
                        .reported_model
                        .as_deref()
                        .or(scope.requested_model.as_deref()),
                ),
                Provider::Antigravity => Some(true),
                Provider::Claude => None,
            };
            if let Some(available) = advertised {
                descriptor.source = DeclaredSource::Provider;
                descriptor.availability = if available {
                    Availability::Available
                } else {
                    Availability::Unavailable
                };
            }
            descriptors.push(descriptor);
        }
        descriptors.push(CapabilityDescriptor::attachment_limits());
        descriptors
    }
}

pub(crate) fn revision(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

impl Store {
    pub async fn record_capability_evidence(
        &self,
        scope: CapabilityScope,
        descriptor: CapabilityDescriptor,
        origin: EvidenceOrigin,
        result: ObservedResult,
        artifact_id: String,
    ) -> Result<String> {
        ensure!(
            descriptor.subject != CapabilitySubject::AttachmentLimits,
            "attachment limit evidence requires measured limits"
        );
        self.store_capability_evidence(scope, descriptor, origin, result, artifact_id, None)
            .await
    }

    pub async fn record_attachment_limit_evidence(
        &self,
        scope: CapabilityScope,
        artifact_id: String,
        limits: QualifiedAttachmentLimits,
    ) -> Result<String> {
        limits.validate(scope.provider)?;
        self.store_capability_evidence(
            scope,
            CapabilityDescriptor::attachment_limits(),
            EvidenceOrigin::AuthenticatedFixture,
            ObservedResult::Succeeded,
            artifact_id,
            Some(limits),
        )
        .await
    }

    async fn store_capability_evidence(
        &self,
        scope: CapabilityScope,
        descriptor: CapabilityDescriptor,
        origin: EvidenceOrigin,
        result: ObservedResult,
        artifact_id: String,
        attachment_limits: Option<QualifiedAttachmentLimits>,
    ) -> Result<String> {
        ensure!(
            !artifact_id.trim().is_empty(),
            "verification requires an evidence artifact"
        );
        ensure!(
            descriptor.blocker.is_none(),
            "blocked capability cannot be qualified"
        );
        ensure!(
            descriptor.availability != Availability::Unavailable
                || result == ObservedResult::Failed,
            "an unavailable capability cannot be qualified as successful"
        );
        if let CapabilitySubject::Tool {
            id,
            version,
            operation,
        } = &descriptor.subject
        {
            ensure!(
                !id.trim().is_empty() && !version.trim().is_empty() && !operation.trim().is_empty(),
                "tool qualification requires an identity, version, and operation"
            );
        }
        let key = scope
            .key(&descriptor)
            .context("qualification scope is incomplete")?;
        let hash = revision(&key)?;
        let evidence = if let Some(limits) = attachment_limits {
            serde_json::to_vec(&(key, result, artifact_id, limits))?
        } else {
            serde_json::to_vec(&(key, result, artifact_id))?
        };
        let evidence = protection::protect(&evidence)?;
        let id = Uuid::new_v4().to_string();
        self.execute(move |db| {
            db.execute(
                "INSERT INTO capability_evidence(id, version, qualification_key, origin, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    id,
                    CAPABILITY_SCHEMA_VERSION,
                    hash,
                    origin.as_sql(),
                    evidence
                ],
            )?;
            Ok(id)
        })
        .await
    }

    pub async fn resolve_capability_assessment(
        &self,
        scope: CapabilityScope,
        descriptors: Vec<CapabilityDescriptor>,
    ) -> Result<CapabilityAssessment> {
        self.execute(move |db| {
            let entries = descriptors
                .into_iter()
                .map(|descriptor| {
                    let evidence = if descriptor.blocker.is_none()
                        && descriptor.availability != Availability::Unavailable
                        && let Some(key) = scope.key(&descriptor)
                    {
                        latest_evidence(db, &revision(&key)?)?
                    } else {
                        None
                    };
                    Ok(ResolvedCapability {
                        descriptor,
                        evidence,
                    })
                })
                .collect::<Result<_>>()?;
            Ok(CapabilityAssessment {
                version: CAPABILITY_SCHEMA_VERSION,
                scope,
                entries,
            })
        })
        .await
    }
}

fn latest_evidence(db: &Connection, hash: &str) -> Result<Option<Verification>> {
    let row = db
        .query_row(
            "SELECT id, checked_at, origin, payload FROM capability_evidence
         WHERE version = ?1 AND qualification_key = ?2
         ORDER BY (origin = 'authenticated_fixture') DESC, sequence DESC LIMIT 1",
            params![CAPABILITY_SCHEMA_VERSION, hash],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(id, checked_at, origin, payload)| {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Evidence {
            AttachmentLimits(
                serde::de::IgnoredAny,
                ObservedResult,
                String,
                QualifiedAttachmentLimits,
            ),
            Capability(serde::de::IgnoredAny, ObservedResult, String),
        }
        let (result, artifact_id, attachment_limits) =
            match serde_json::from_slice(&protection::unprotect(&payload)?)? {
                Evidence::AttachmentLimits(_, result, artifact_id, limits) => {
                    (result, artifact_id, Some(limits))
                }
                Evidence::Capability(_, result, artifact_id) => (result, artifact_id, None),
            };
        Ok(Verification {
            id,
            checked_at,
            origin: serde_json::from_value(serde_json::Value::String(origin))?,
            result,
            artifact_id,
            attachment_limits,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::WindowsVersion;

    fn scope() -> CapabilityScope {
        CapabilityScope {
            provider: Provider::Codex,
            transport: Transport::CodexAppServer,
            cli_version: "1.2.3".into(),
            executable_revision: Some("cli-build-1".into()),
            profile_id: "profile".into(),
            account_binding: Some("account".into()),
            requested_model: Some("requested".into()),
            reported_model: Some("reported".into()),
            requested_effort: Some("low".into()),
            reported_effort: Some("medium".into()),
            runner: "windows-direct".into(),
            platform: Some(ExecutionPlatform {
                host: WindowsVersion {
                    major: 10,
                    minor: 0,
                    build: 19045,
                    revision: 6000,
                    native_architecture: 9,
                    server: false,
                },
                guest: None,
            }),
            mode: SessionMode::ReadOnly,
            permission_revision: "readonly-1".into(),
            checkout: CheckoutIdentity {
                id: "fixture".into(),
                path: PathBuf::from(r"C:\fixtures\project"),
            },
            configuration: ConfigurationEvidence {
                detected: Vec::new(),
                unresolved: Vec::new(),
                provider_revision: Some("provider-config".into()),
                effective_revision: Some("config-1".into()),
                scoped_launch_revision: Some("scoped-config-1".into()),
                environment_revision: Some("environment-1".into()),
                tool_revisions: BTreeMap::from([(
                    "fixture/read_file".into(),
                    "tool-config-1".into(),
                )]),
            },
        }
    }

    #[test]
    fn session_capabilities_distinguish_observation_and_unavailable_adapter_routes() {
        let status = |reported: &ReportedCapabilities, scope: &CapabilityScope, capability| {
            let descriptor = reported
                .descriptors(scope)
                .into_iter()
                .find(|entry| entry.subject == CapabilitySubject::Protocol { capability })
                .expect("protocol descriptor");
            ResolvedCapability {
                descriptor,
                evidence: None,
            }
            .status()
        };
        let mut scope = scope();
        let mut codex = ReportedCapabilities::default();
        assert_eq!(
            status(&codex, &scope, Capability::NativeTurnFork),
            CapabilityStatus::Unverified
        );
        codex.observe(&ProviderEventData::NativeForkVerified {
            source_session_id: "original".into(),
            last_turn_id: "boundary".into(),
        });
        assert_eq!(
            status(&codex, &scope, Capability::NativeTurnFork),
            CapabilityStatus::Observed
        );
        codex.observe(&ProviderEventData::Compacted);
        assert_eq!(
            status(&codex, &scope, Capability::NativeCompaction),
            CapabilityStatus::Observed
        );
        assert_eq!(
            status(&codex, &scope, Capability::ManualNativeCompaction),
            CapabilityStatus::Unverified
        );
        codex.observe(&ProviderEventData::NativeCompactionFinished {
            outcome: super::super::TurnOutcome::Completed,
        });
        assert_eq!(
            status(&codex, &scope, Capability::ManualNativeCompaction),
            CapabilityStatus::Observed
        );

        let mut claude = ReportedCapabilities::default();
        scope.provider = Provider::Claude;
        scope.transport = Transport::ClaudeStreamJson;
        claude.observe(&ProviderEventData::Compacted);
        assert_eq!(
            status(&claude, &scope, Capability::NativeCompaction),
            CapabilityStatus::Observed
        );
        assert_eq!(
            status(&claude, &scope, Capability::ManualNativeCompaction),
            CapabilityStatus::Unavailable
        );
        assert_eq!(
            status(&claude, &scope, Capability::NativeTurnFork),
            CapabilityStatus::Unavailable
        );

        let antigravity = ReportedCapabilities::default();
        scope.provider = Provider::Antigravity;
        scope.transport = Transport::AgentClientProtocol;
        assert_eq!(
            status(&antigravity, &scope, Capability::NativeCompaction),
            CapabilityStatus::Unverified
        );
        assert_eq!(
            status(&antigravity, &scope, Capability::ManualNativeCompaction),
            CapabilityStatus::Unavailable
        );
        assert_eq!(
            status(&antigravity, &scope, Capability::NativeTurnFork),
            CapabilityStatus::Unavailable
        );
    }

    fn descriptor() -> CapabilityDescriptor {
        CapabilityDescriptor {
            subject: CapabilitySubject::Tool {
                id: "fixture/read_file".into(),
                version: "tool-1".into(),
                operation: "read".into(),
            },
            source: DeclaredSource::Provider,
            availability: Availability::Available,
            observed: None,
            blocker: None,
            boundary: EvidenceBoundary::EquivalentConfiguration,
        }
    }

    #[tokio::test]
    async fn evidence_matches_only_the_qualified_profile_and_permission_boundary() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Store::open(temp.path().join("capabilities.sqlite"))?;
        let fixture = scope();
        let tool = descriptor();
        let evidence_id = store
            .record_capability_evidence(
                fixture.clone(),
                tool.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                "fixture-success".into(),
            )
            .await?;
        let mut other_checkout = fixture.clone();
        other_checkout.checkout.id = "task".into();
        other_checkout.checkout.path = PathBuf::from(r"C:\tasks\worktree");
        let assessment = store
            .resolve_capability_assessment(other_checkout.clone(), vec![tool.clone()])
            .await?;
        assert_eq!(
            assessment.entries[0].evidence.as_ref().unwrap().id,
            evidence_id
        );
        assert_eq!(assessment.entries[0].status(), CapabilityStatus::Verified);
        assert_eq!(assessment.scope.checkout.id, "task");
        for change in [
            |scope: &mut CapabilityScope| scope.account_binding = Some("other-account".into()),
            |scope: &mut CapabilityScope| scope.account_binding = None,
            |scope: &mut CapabilityScope| scope.profile_id = "other-profile".into(),
            |scope: &mut CapabilityScope| scope.reported_model = Some("other-model".into()),
            |scope: &mut CapabilityScope| scope.requested_model = Some("other-model".into()),
            |scope: &mut CapabilityScope| {
                scope.executable_revision = Some("different-build".into())
            },
            |scope: &mut CapabilityScope| scope.cli_version = "1.2.4".into(),
            |scope: &mut CapabilityScope| scope.transport = Transport::AgentClientProtocol,
            |scope: &mut CapabilityScope| scope.runner = "windows-guest".into(),
            |scope: &mut CapabilityScope| scope.platform.as_mut().unwrap().host.revision += 1,
            |scope: &mut CapabilityScope| scope.permission_revision = "readonly-2".into(),
            |scope: &mut CapabilityScope| {
                scope.configuration.environment_revision = Some("environment-2".into())
            },
            |scope: &mut CapabilityScope| {
                scope
                    .configuration
                    .tool_revisions
                    .insert("fixture/read_file".into(), "modified tool".into());
            },
            |scope: &mut CapabilityScope| {
                scope
                    .configuration
                    .unresolved
                    .push(PathBuf::from("settings.json"))
            },
        ] {
            let mut changed = other_checkout.clone();
            change(&mut changed);
            let resolved = store
                .resolve_capability_assessment(changed, vec![tool.clone()])
                .await?;
            assert!(resolved.entries[0].evidence.is_none());
        }
        let mut other_tool = tool.clone();
        if let CapabilitySubject::Tool { version, .. } = &mut other_tool.subject {
            *version = "tool-2".into();
        }
        let path_sensitive = CapabilityDescriptor {
            boundary: EvidenceBoundary::ExactCheckout,
            ..tool.clone()
        };
        let narrow_id = store
            .record_capability_evidence(
                fixture.clone(),
                path_sensitive.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                "same-path-only".into(),
            )
            .await?;
        let resolved = store
            .resolve_capability_assessment(other_checkout, vec![other_tool, path_sensitive.clone()])
            .await?;
        assert!(
            resolved
                .entries
                .iter()
                .all(|entry| entry.evidence.is_none())
        );
        let resolved = store
            .resolve_capability_assessment(fixture, vec![path_sensitive])
            .await?;
        assert_eq!(resolved.entries[0].evidence.as_ref().unwrap().id, narrow_id);
        Ok(())
    }

    #[tokio::test]
    async fn unrelated_tool_changes_do_not_discard_matching_evidence() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Store::open(temp.path().join("capabilities.sqlite"))?;
        let read = descriptor();
        let browser = CapabilityDescriptor {
            subject: CapabilitySubject::Tool {
                id: "fixture/browser".into(),
                version: "1".into(),
                operation: "open".into(),
            },
            ..read.clone()
        };
        let mut profile = scope();
        profile
            .configuration
            .tool_revisions
            .insert("fixture/browser".into(), "browser-config-1".into());
        let shared_read = store
            .record_capability_evidence(
                profile.clone(),
                read.clone(),
                EvidenceOrigin::ProtocolFixture,
                ObservedResult::Succeeded,
                "read-fixture".into(),
            )
            .await?;
        let old_browser = store
            .record_capability_evidence(
                profile.clone(),
                browser.clone(),
                EvidenceOrigin::ProtocolFixture,
                ObservedResult::Succeeded,
                "browser-fixture".into(),
            )
            .await?;
        let saved = store
            .resolve_capability_assessment(profile.clone(), vec![read.clone(), browser.clone()])
            .await?;
        assert_eq!(
            saved.entries[1].evidence.as_ref().map(|entry| &entry.id),
            Some(&old_browser)
        );
        profile.configuration.effective_revision = Some("changed full configuration".into());
        profile
            .configuration
            .tool_revisions
            .insert("fixture/browser".into(), "browser-config-2".into());
        let updated = store
            .resolve_capability_assessment(profile.clone(), vec![read.clone(), browser])
            .await?;
        assert_eq!(
            updated.entries[0].evidence.as_ref().map(|entry| &entry.id),
            Some(&shared_read)
        );
        assert!(updated.entries[1].evidence.is_none());
        let unavailable = CapabilityDescriptor {
            availability: Availability::Unavailable,
            ..read
        };
        let current = store
            .resolve_capability_assessment(profile, vec![unavailable])
            .await?;
        assert_eq!(current.entries[0].status(), CapabilityStatus::Unavailable);
        assert!(current.entries[0].evidence.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn fixtures_and_newer_failures_keep_the_original_assessment_inspectable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Store::open(temp.path().join("capabilities.sqlite"))?;
        let scope = scope();
        let tool = descriptor();
        let protocol = store
            .record_capability_evidence(
                scope.clone(),
                tool.clone(),
                EvidenceOrigin::ProtocolFixture,
                ObservedResult::Succeeded,
                "scripted".into(),
            )
            .await?;
        let initial = store
            .resolve_capability_assessment(scope.clone(), vec![tool.clone()])
            .await?;
        assert_eq!(initial.entries[0].status(), CapabilityStatus::Reported);
        assert_eq!(initial.entries[0].evidence.as_ref().unwrap().id, protocol);
        let qualified = store
            .record_capability_evidence(
                scope.clone(),
                tool.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Succeeded,
                "authenticated-pass".into(),
            )
            .await?;
        let passing = store
            .resolve_capability_assessment(scope.clone(), vec![tool.clone()])
            .await?;
        assert_eq!(passing.entries[0].evidence.as_ref().unwrap().id, qualified);
        assert_eq!(passing.entries[0].status(), CapabilityStatus::Verified);
        let failed = store
            .record_capability_evidence(
                scope.clone(),
                tool.clone(),
                EvidenceOrigin::AuthenticatedFixture,
                ObservedResult::Failed,
                "authenticated-failure".into(),
            )
            .await?;
        let latest = store
            .resolve_capability_assessment(scope.clone(), vec![tool])
            .await?;
        assert_eq!(latest.entries[0].evidence.as_ref().unwrap().id, failed);
        assert_eq!(latest.entries[0].status(), CapabilityStatus::Reported);
        assert_eq!(passing.entries[0].status(), CapabilityStatus::Verified);
        let blocked = ReportedCapabilities::default()
            .descriptors(&scope)
            .into_iter()
            .find(|descriptor| {
                descriptor.subject
                    == CapabilitySubject::Protocol {
                        capability: Capability::WorkspaceWrite,
                    }
            })
            .unwrap();
        let blocked = store
            .resolve_capability_assessment(scope, vec![blocked])
            .await?;
        assert_eq!(blocked.entries[0].status(), CapabilityStatus::Blocked);
        Ok(())
    }
}
