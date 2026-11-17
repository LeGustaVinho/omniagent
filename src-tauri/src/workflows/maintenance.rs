use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{protection, runtime_inspection::BuildIdentity, store::Store};

use super::{
    ApprovalDecisionSource, BuiltinNode, Executor, NodeInputs, PinnedNode, Wire, WorkflowRevision,
    artifacts::insert_scoped_artifact, builtin::BUILTIN_DELIVERY_ID, control,
    inspection::record_workflow_input_ids,
};

mod evidence;
mod package;
mod signed_activation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceActivationMode {
    Disabled,
    ManualFixture,
    SignedUpdater,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceBuild {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub timeout_seconds: Option<u64>,
    pub package_path: String,
    pub version_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceConfiguration {
    pub source_checkout_id: String,
    pub workflow_id: String,
    pub build: MaintenanceBuild,
    pub required_checks: Vec<String>,
    pub activation: MaintenanceActivationMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceSettings {
    pub revision: String,
    pub configuration: MaintenanceConfiguration,
    pub build_executable_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceFixtureActivation {
    pub id: String,
    pub recorded_at: String,
    pub workflow_artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceSignedActivation {
    pub id: String,
    pub version: String,
    pub source_revision: String,
    pub sha256: String,
    pub workflow_artifact_id: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaintenanceHandoff {
    pub id: String,
    pub project_id: String,
    pub workflow_run_id: String,
    pub workflow: WorkflowRevision,
    pub node_id: String,
    pub git_artifact_id: String,
    pub task_id: String,
    pub unit_id: String,
    pub plan_revision: String,
    pub settings_revision: String,
    pub version: String,
    pub source_revision: String,
    pub source_tree: String,
    pub candidate_revision: String,
    pub sha256: String,
    pub size: u64,
    pub package_name: String,
    pub commit_evidence_id: String,
    pub verification_id: String,
    pub checks: Vec<String>,
    pub approval_artifact_id: String,
    pub approval_source: ApprovalDecisionSource,
    pub acceptance_artifact_id: String,
    pub review_artifact_id: String,
    pub review_run_id: String,
    pub build_executable_revision: String,
    pub recorded_at: String,
    pub activation: Option<MaintenanceFixtureActivation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_activation: Option<MaintenanceSignedActivation>,
}

impl MaintenanceHandoff {
    fn request(&self) -> MaintenanceHandoffRequest {
        MaintenanceHandoffRequest {
            project_id: self.project_id.clone(),
            workflow_run_id: self.workflow_run_id.clone(),
            node_id: self.node_id.clone(),
            git_artifact_id: self.git_artifact_id.clone(),
            task_id: self.task_id.clone(),
            commit_evidence_id: self.commit_evidence_id.clone(),
            expected_settings_revision: self.settings_revision.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMaintenance {
    running_build: BuildIdentity,
    pub settings: Option<MaintenanceSettings>,
    pub handoffs: Vec<MaintenanceHandoff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceHandoffRequest {
    pub project_id: String,
    pub workflow_run_id: String,
    pub node_id: String,
    pub git_artifact_id: String,
    pub task_id: String,
    pub commit_evidence_id: String,
    pub expected_settings_revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceActivationRequest {
    pub project_id: String,
    pub handoff_id: String,
    pub expected_settings_revision: String,
    pub expected_sha256: String,
}

impl Store {
    pub async fn workflow_maintenance(&self, project: String) -> Result<WorkflowMaintenance> {
        self.execute(move |db| {
            let mut statement = db.prepare(&format!(
                "{HANDOFF_SELECT} WHERE h.project_id = ?1 ORDER BY h.recorded_at DESC, h.id DESC"
            ))?;
            let handoffs = statement
                .query_map([&project], decode_handoff)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(WorkflowMaintenance {
                running_build: BuildIdentity::running(),
                settings: load_settings(db, &project)?,
                handoffs,
            })
        })
        .await
    }

    pub async fn set_workflow_maintenance(
        &self,
        project: String,
        expected_revision: Option<String>,
        mut configuration: MaintenanceConfiguration,
    ) -> Result<MaintenanceSettings> {
        let (build, build_executable_revision) = tokio::task::spawn_blocking({
            let mut build = configuration.build;
            move || {
                let revision = package::pin_build(&mut build)?;
                Ok::<_, anyhow::Error>((build, revision))
            }
        })
        .await??;
        configuration.build = build;
        for check in &mut configuration.required_checks {
            *check = check.trim().to_owned();
        }
        configuration.required_checks.sort();
        configuration.required_checks.dedup();
        ensure!(
            !configuration.required_checks.is_empty()
                && configuration
                    .required_checks
                    .iter()
                    .all(|name| !name.is_empty()),
            "Select at least one required candidate test check"
        );
        ensure!(
            !configuration.workflow_id.trim().is_empty(),
            "Select a maintenance workflow"
        );
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let previous_revision = load_settings(&tx, &project)?.map(|settings| settings.revision);
            ensure!(
                previous_revision == expected_revision,
                "Maintenance settings changed"
            );
            let source_registered: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2)",
                params![project, configuration.source_checkout_id],
                |row| row.get(0),
            )?;
            ensure!(
                source_registered,
                "Select the project's registered source checkout"
            );
            let settings = MaintenanceSettings {
                revision: Uuid::new_v4().to_string(),
                configuration,
                build_executable_revision,
            };
            tx.execute(
                "INSERT INTO workflow_maintenance_settings VALUES (?1, ?2, ?3)
                 ON CONFLICT(project_id) DO UPDATE SET revision = excluded.revision,
                    configuration = excluded.configuration",
                params![project, settings.revision, protect(&settings)?],
            )?;
            tx.commit()?;
            Ok(settings)
        })
        .await
    }

    pub async fn create_maintenance_handoff(
        &self,
        request: MaintenanceHandoffRequest,
        package_root: PathBuf,
    ) -> Result<MaintenanceHandoff> {
        let initial = self
            .execute({
                let request = request.clone();
                move |db| {
                    let candidate = evidence::resolve(db, &request)?;
                    let existing = existing_handoff(db, &request)?;
                    if existing.is_none() {
                        require_unactivated_stage(db, &request)?;
                    }
                    Ok((candidate, existing))
                }
            })
            .await?;
        let (candidate, existing) = initial;
        if let Some(existing) = existing {
            return Ok(existing);
        }

        let id = Uuid::new_v4().to_string();
        let built = tokio::task::spawn_blocking({
            let id = id.clone();
            let candidate = candidate.clone();
            move || package::build(&package_root, &id, &candidate)
        })
        .await??;

        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure!(
                evidence::resolve(&tx, &request)? == candidate,
                "Candidate evidence changed during the build"
            );
            if let Some(existing) = existing_handoff(&tx, &request)? {
                return Ok(existing);
            }
            require_unactivated_stage(&tx, &request)?;
            let recorded_at =
                tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                    row.get(0)
                })?;
            let handoff = candidate.handoff(id, recorded_at, &built);
            tx.execute(
                "INSERT INTO workflow_maintenance_handoffs VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    handoff.id,
                    handoff.project_id,
                    handoff.workflow_run_id,
                    handoff.node_id,
                    handoff.git_artifact_id,
                    handoff.commit_evidence_id,
                    handoff.settings_revision,
                    handoff.recorded_at,
                    protect(&handoff)?
                ],
            )?;
            let published = built.persist()?;
            if let Err(error) = tx.commit() {
                let _ = std::fs::remove_file(published);
                return Err(error.into());
            }
            Ok(handoff)
        })
        .await
    }

    pub async fn activate_maintenance_fixture(
        &self,
        request: MaintenanceActivationRequest,
        package_root: PathBuf,
    ) -> Result<MaintenanceHandoff> {
        let initial = self
            .execute({
                let request = request.clone();
                move |db| {
                    require_activation(db, &request, MaintenanceActivationMode::ManualFixture)
                        .map(|(handoff, _)| handoff)
                }
            })
            .await?;
        if initial.activation.is_some() {
            return Ok(initial);
        }
        let package_lock =
            tokio::task::spawn_blocking(move || package::check_bytes(&package_root, &initial))
                .await??;
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (mut handoff, candidate) = require_activation(&tx, &request, MaintenanceActivationMode::ManualFixture)?;
            if handoff.activation.is_none() {
                let id = Uuid::new_v4().to_string();
                let output = insert_scoped_artifact(
                    &tx, &handoff.project_id, handoff.workflow_run_id.clone(),
                    handoff.node_id.clone(), "source".into(), Some(handoff.unit_id.clone()),
                    candidate.git_artifact.value,
                )?;
                record_workflow_input_ids(
                    &tx, &output, &handoff.plan_revision,
                    &NodeInputs::from([("git".into(), handoff.git_artifact_id.clone())]),
                )?;
                let recorded_at = tx.query_row(
                    "INSERT INTO workflow_maintenance_fixture_activations (handoff_id, id, output_artifact_id)
                     VALUES (?1, ?2, ?3) RETURNING recorded_at",
                    params![handoff.id, id, output.id],
                    |row| row.get(0),
                )?;
                handoff.activation = Some(MaintenanceFixtureActivation {
                    id, recorded_at, workflow_artifact_id: output.id,
                });
                control::refresh(&tx, &handoff.project_id, &handoff.workflow_run_id)?;
            }
            tx.commit()?;
            drop(package_lock);
            Ok(handoff)
        })
        .await
    }

    pub(crate) async fn require_signed_update_handoff(
        &self,
        request: MaintenanceActivationRequest,
    ) -> Result<MaintenanceHandoff> {
        self.execute(move |db| validate_signed_update_handoff(db, &request))
            .await
    }
}

pub(crate) fn validate_signed_update_handoff(
    db: &Connection,
    request: &MaintenanceActivationRequest,
) -> Result<MaintenanceHandoff> {
    let (handoff, candidate) =
        require_activation(db, request, MaintenanceActivationMode::SignedUpdater)?;
    ensure!(
        candidate.matches_handoff(&handoff),
        "The accepted maintenance evidence changed"
    );
    Ok(handoff)
}

pub(crate) fn load_settings(db: &Connection, project: &str) -> Result<Option<MaintenanceSettings>> {
    db.query_row(
        "SELECT configuration FROM workflow_maintenance_settings WHERE project_id = ?1",
        [project],
        |row| decode(row, 0),
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn builtin_enabled(db: &Connection, project: &str) -> Result<bool> {
    Ok(load_settings(db, project)?
        .is_some_and(|settings| settings.configuration.workflow_id == BUILTIN_DELIVERY_ID))
}

pub(super) fn validate_activation_wires(nodes: &[PinnedNode], wires: &[Wire]) -> Result<()> {
    let mut activation_nodes = nodes
        .iter()
        .filter(|node| node.definition.executor == Executor::ActivateMaintenanceFixture);
    let Some(activation) = activation_nodes.next() else {
        return Ok(());
    };
    ensure!(
        activation_nodes.next().is_none(),
        "Use one maintenance activation stage per workflow"
    );
    let source = |port: &str| {
        wires
            .iter()
            .find(|wire| wire.to.node == activation.instance.id && wire.to.port == port)
            .map(|wire| &wire.from)
    };
    let ready =
        source("ready").context("Maintenance activation needs a verified commit control input")?;
    let git = source("git").context("Maintenance activation needs a verified commit artifact")?;
    ensure!(
        ready.node == git.node
            && ready.port == "verified"
            && git.port == "git"
            && nodes.iter().any(|node| node.instance.id == git.node
                && node.instance.kind == BuiltinNode::VerifyCommit),
        "Wire maintenance activation to both outputs of the same Verify commit node"
    );
    Ok(())
}

fn require_activation(
    db: &Connection,
    request: &MaintenanceActivationRequest,
    mode: MaintenanceActivationMode,
) -> Result<(MaintenanceHandoff, evidence::CandidateEvidence)> {
    let handoff = db
        .query_row(
            &format!("{HANDOFF_SELECT} WHERE h.project_id = ?1 AND h.id = ?2"),
            params![request.project_id, request.handoff_id],
            decode_handoff,
        )
        .optional()?
        .context("The candidate handoff is unavailable in this project")?;
    ensure!(
        handoff.signed_activation.is_none()
            && (mode != MaintenanceActivationMode::SignedUpdater || handoff.activation.is_none()),
        "This maintenance workflow stage already recorded an activation"
    );
    ensure!(
        handoff.sha256 == request.expected_sha256,
        "The candidate package digest changed"
    );
    ensure!(
        handoff.settings_revision == request.expected_settings_revision,
        "Review a handoff prepared for the current maintenance settings"
    );
    let source = handoff.request();
    let candidate = evidence::resolve(db, &source)?;
    ensure!(
        candidate.settings.configuration.activation == mode,
        "{} activation is disabled for this maintenance workflow",
        if mode == MaintenanceActivationMode::ManualFixture {
            "Fixture"
        } else {
            "Signed update"
        },
    );
    if handoff.activation.is_none() {
        require_unactivated_stage(db, &source)?;
    }
    Ok((handoff, candidate))
}

fn existing_handoff(
    db: &Connection,
    request: &MaintenanceHandoffRequest,
) -> Result<Option<MaintenanceHandoff>> {
    db.query_row(
        &format!(
            "{HANDOFF_SELECT}
         WHERE h.project_id = ?1 AND h.pipeline_run_id = ?2 AND h.commit_evidence_id = ?3
           AND h.settings_revision = ?4 AND h.node_instance_id = ?5 AND h.git_artifact_id = ?6"
        ),
        params![
            request.project_id,
            request.workflow_run_id,
            request.commit_evidence_id,
            request.expected_settings_revision,
            request.node_id,
            request.git_artifact_id,
        ],
        decode_handoff,
    )
    .optional()
    .map_err(Into::into)
}

fn require_unactivated_stage(db: &Connection, request: &MaintenanceHandoffRequest) -> Result<()> {
    let activated: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_maintenance_handoffs h
         LEFT JOIN workflow_maintenance_fixture_activations a ON a.handoff_id = h.id
         LEFT JOIN workflow_maintenance_signed_activations s ON s.handoff_id = h.id
         WHERE h.project_id = ?1 AND h.pipeline_run_id = ?2 AND h.node_instance_id = ?3
           AND h.commit_evidence_id = ?4 AND (a.id IS NOT NULL OR s.id IS NOT NULL))",
        params![
            request.project_id,
            request.workflow_run_id,
            request.node_id,
            request.commit_evidence_id
        ],
        |row| row.get(0),
    )?;
    ensure!(
        !activated,
        "This workflow stage already recorded its maintenance activation"
    );
    Ok(())
}

const HANDOFF_SELECT: &str =
    "SELECT h.receipt, a.id, a.recorded_at, a.output_artifact_id, s.receipt
     FROM workflow_maintenance_handoffs h
     LEFT JOIN workflow_maintenance_fixture_activations a ON a.handoff_id = h.id
     LEFT JOIN workflow_maintenance_signed_activations s ON s.handoff_id = h.id";

fn decode_handoff(row: &rusqlite::Row<'_>) -> rusqlite::Result<MaintenanceHandoff> {
    let mut handoff: MaintenanceHandoff = decode(row, 0)?;
    if let Some(id) = row.get::<_, Option<String>>(1)? {
        handoff.activation = Some(MaintenanceFixtureActivation {
            id,
            recorded_at: row.get(2)?,
            workflow_artifact_id: row.get(3)?,
        });
    }
    if !matches!(row.get_ref(4)?, rusqlite::types::ValueRef::Null) {
        handoff.signed_activation = Some(decode(row, 4)?);
    }
    Ok(handoff)
}

fn protect<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn decode<T: DeserializeOwned>(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<T> {
    let payload: Vec<u8> = row.get(column)?;
    let invalid = |error| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Blob, error)
    };
    let bytes = protection::unprotect(&payload).map_err(|error| invalid(Box::new(error)))?;
    serde_json::from_slice(&bytes).map_err(|error| invalid(Box::new(error)))
}
