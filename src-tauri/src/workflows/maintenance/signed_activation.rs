use crate::{
    maintenance_update::{ActivationOutcome, UpdateActivationReceipt},
    workflows::{ArtifactKind, ArtifactValue, artifacts::load_artifact},
};

use super::*;

impl Store {
    pub(crate) async fn record_verified_signed_activation(
        &self,
        receipt: UpdateActivationReceipt,
    ) -> Result<()> {
        ensure!(
            receipt.outcome == ActivationOutcome::Verified,
            "The signed update has not passed startup verification"
        );
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let Some(handoff) = tx
                .query_row(
                    &format!("{HANDOFF_SELECT} WHERE h.project_id = ?1 AND h.id = ?2"),
                    params![receipt.project_id, receipt.update.handoff_id],
                    decode_handoff,
                )
                .optional()?
            else {
                // A user-selected historical restore may predate this handoff.
                // Keep the installation receipt available without repopulating that generation.
                return Ok(());
            };
            ensure!(
                handoff
                    .version
                    .strip_prefix('v')
                    .unwrap_or(&handoff.version)
                    == receipt.update.version
                    && handoff.source_revision == receipt.update.source_revision
                    && handoff.sha256 == receipt.update.sha256
                    && handoff.size == receipt.update.size
                    && handoff.checks == receipt.update.checks
                    && handoff.verification_id == receipt.update.verification_id
                    && handoff.approval_artifact_id == receipt.update.approval_artifact_id,
                "The verified installation receipt does not match the accepted maintenance handoff"
            );
            if let Some(existing) = &handoff.signed_activation {
                ensure!(
                    existing.id == receipt.id,
                    "This maintenance stage already recorded another signed activation"
                );
                return Ok(());
            }
            require_unactivated_stage(&tx, &handoff.request())?;
            let artifact = load_artifact(
                &tx,
                &handoff.project_id,
                &handoff.workflow_run_id,
                &handoff.git_artifact_id,
                ArtifactKind::Git,
            )?
            .context("The accepted maintenance source is missing from the workflow history")?;
            ensure!(
                artifact.unit_id.as_ref() == Some(&handoff.unit_id)
                    && matches!(&artifact.value, ArtifactValue::Git(source)
                        if source.id == handoff.commit_evidence_id
                            && source.task.project_id == handoff.project_id
                            && source.task.task_id == handoff.task_id),
                "The saved maintenance source belongs to a different unit or commit"
            );
            let output = insert_scoped_artifact(
                &tx,
                &handoff.project_id,
                handoff.workflow_run_id.clone(),
                handoff.node_id.clone(),
                "source".into(),
                Some(handoff.unit_id.clone()),
                artifact.value,
            )?;
            record_workflow_input_ids(
                &tx,
                &output,
                &handoff.plan_revision,
                &NodeInputs::from([("git".into(), handoff.git_artifact_id)]),
            )?;
            let recorded_at =
                tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                    row.get(0)
                })?;
            let activation = MaintenanceSignedActivation {
                id: receipt.id,
                version: receipt.update.version,
                source_revision: receipt.update.source_revision,
                sha256: receipt.update.sha256,
                workflow_artifact_id: output.id,
                recorded_at,
            };
            tx.execute(
                "INSERT INTO workflow_maintenance_signed_activations VALUES (?1, ?2, ?3, ?4)",
                params![
                    handoff.id,
                    activation.id,
                    activation.workflow_artifact_id,
                    protect(&activation)?
                ],
            )?;
            control::refresh(&tx, &handoff.project_id, &handoff.workflow_run_id)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
}
