use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    protection,
    repository::verification::{
        CandidateGateOutcome, SelectedCandidate, load_scoped_candidate_acceptance,
        load_scoped_candidate_verification, load_selected_candidate,
    },
    store::Store,
};

use super::GoalDefinition;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum GoalCriterionEvidence {
    CandidateGate {
        verification_id: String,
        gate_index: usize,
    },
    CandidateAcceptance {
        acceptance_id: String,
    },
    OperatorReview {
        approved: bool,
        note: Option<String>,
    },
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalCriterionStatus {
    Pending,
    Failed,
    Passed,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerificationStatus {
    Verifying,
    Failed,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCriterionVerification {
    pub ordinal: usize,
    pub text: String,
    pub status: GoalCriterionStatus,
    pub evidence: Option<GoalCriterionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalVerification {
    pub revision_id: String,
    pub run_id: String,
    pub context_id: String,
    pub provider_result: Option<String>,
    pub candidate_id: Option<String>,
    pub status: GoalVerificationStatus,
    pub criteria: Vec<GoalCriterionVerification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalEvidenceChoice {
    pub label: String,
    pub candidate_id: String,
    pub status: GoalCriterionStatus,
    pub evidence: GoalCriterionEvidence,
}

struct ResultScope {
    revision_id: String,
    run_id: String,
    context_id: String,
    project: String,
    task: Option<String>,
    checkout_id: String,
    provider_result: Option<String>,
    definition: GoalDefinition,
}

impl Store {
    pub async fn goal_verification(
        &self,
        project: String,
        run: String,
    ) -> Result<Option<GoalVerification>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            load_goal_verification(&tx, &project, &run)
        })
        .await
    }

    pub async fn list_goal_criterion_evidence(
        &self,
        project: String,
        run: String,
    ) -> Result<Vec<GoalEvidenceChoice>> {
        self.execute(move |db| {
            let tx = db.transaction()?;
            let Some(scope) = result_scope(&tx, &project, &run)? else {
                return Ok(Vec::new());
            };
            let Some(task) = &scope.task else {
                return Ok(Vec::new());
            };
            let saved = evaluate(&tx, &scope)?;
            let mut candidates = tx.prepare(
                "SELECT id FROM selected_candidate_revisions WHERE project_id = ?1 AND task_id = ?2
                 AND capture_run_id = ?3 AND (?4 IS NULL OR id = ?4) ORDER BY id"
            )?;
            let ids = candidates.query_map(params![project, task, run, saved.candidate_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut choices = Vec::new();
            for id in ids {
                let candidate = load_selected_candidate(&tx, &project, task, &id)?;
                require_result_candidate(&scope, &candidate)?;
                let mut query = tx.prepare(
                    "SELECT id FROM candidate_verifications WHERE candidate_id = ?1 ORDER BY sequence DESC"
                )?;
                for verification_id in query.query_map([&id], |row| row.get::<_, String>(0))? {
                    let verification_id = verification_id?;
                    let verification = load_scoped_candidate_verification(&tx, &project, task, &verification_id)?
                        .context("Recorded candidate verification is unavailable")?;
                    for (gate_index, gate) in verification.gates.iter().enumerate() {
                        choices.push(GoalEvidenceChoice {
                            label: gate.gate.name.clone(),
                            candidate_id: id.clone(),
                            status: gate_status(gate.outcome.as_ref()),
                            evidence: GoalCriterionEvidence::CandidateGate { verification_id: verification_id.clone(), gate_index },
                        });
                    }
                }
                let mut query = tx.prepare(
                    "SELECT id FROM candidate_acceptances WHERE candidate_id = ?1 ORDER BY accepted_at DESC, id"
                )?;
                for acceptance_id in query.query_map([&id], |row| row.get::<_, String>(0))? {
                    let evidence = GoalCriterionEvidence::CandidateAcceptance { acceptance_id: acceptance_id? };
                    choices.push(GoalEvidenceChoice {
                        label: "Candidate acceptance".into(),
                        candidate_id: id.clone(),
                        status: evaluate_evidence(&tx, &scope, &evidence)?.1,
                        evidence,
                    });
                }
            }
            Ok(choices)
        }).await
    }

    /// Associates evidence chosen by the operator with one criterion of this
    /// sealed result. Provider statements alone cannot create these links.
    pub async fn record_goal_criterion_evidence(
        &self,
        project: String,
        run: String,
        ordinal: usize,
        evidence: GoalCriterionEvidence,
    ) -> Result<GoalVerification> {
        let updated = self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let scope = result_scope(&tx, &project, &run)?
                .context("No sealed goal result needs verification for this run")?;
            crate::workflows::stage_goal::require_stage_criterion(&tx, &run, ordinal, &evidence)?;
            let current = evaluate(&tx, &scope)?;
            let criterion = ordinal.checked_sub(1).and_then(|index| current.criteria.get(index))
                .context("The criterion is outside the saved goal revision")?;
            if criterion.evidence.as_ref() == Some(&evidence) {
                return Ok(current);
            }
            ensure!(current.status != GoalVerificationStatus::Completed, "This goal result has already been verified");
            if let GoalCriterionEvidence::OperatorReview { note: Some(note), .. } = &evidence {
                ensure!(note.len() <= 8192, "The review note exceeds its size limit");
            }
            let (candidate, _) = evaluate_evidence(&tx, &scope, &evidence)?;
            ensure!(
                candidate.as_ref().is_none_or(|id| current.candidate_id.as_ref().is_none_or(|saved| saved == id)),
                "Goal criteria must reference the same recorded candidate"
            );
            tx.execute(
                "INSERT INTO goal_criterion_evidence(run_id, criterion_ordinal, candidate_id, payload) VALUES (?1, ?2, ?3, ?4)",
                 params![run, i64::try_from(ordinal)?, candidate, protection::protect(&serde_json::to_vec(&evidence)?)?],
            )?;
            let updated = evaluate(&tx, &scope)?;
            tx.commit()?;
            Ok(updated)
        }).await?;
        self.job_signal.send_replace(());
        Ok(updated)
    }
}

pub(crate) fn load_goal_verification(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<Option<GoalVerification>> {
    result_scope(db, project, run)?
        .map(|scope| evaluate(db, &scope))
        .transpose()
}

fn result_scope(db: &Connection, project: &str, run: &str) -> Result<Option<ResultScope>> {
    let row = db.query_row(
        "SELECT c.revision_id, c.context_id, r.definition, coalesce(task.scope_task_id, j.task_id),
                j.checkout_id, executor.provider_result
         FROM goal_checkpoints c JOIN goal_revisions r ON r.id = c.revision_id
         JOIN goals g ON g.id = r.goal_id JOIN job_attempts attempt ON attempt.run_id = c.run_id
         JOIN jobs j ON j.id = attempt.job_id
         LEFT JOIN job_worktree_scopes task ON task.job_id = j.id
         LEFT JOIN native_goal_executors executor ON executor.run_id = c.run_id
         WHERE g.project_id = ?1 AND c.run_id = ?2 AND c.next_action = 'review'",
        params![project, run],
        |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Vec<u8>>(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).optional()?;
    row.map(
        |(revision_id, context_id, definition, task, checkout_id, provider_result)| {
            Ok(ResultScope {
                revision_id,
                run_id: run.to_owned(),
                context_id,
                project: project.to_owned(),
                task,
                checkout_id,
                provider_result,
                definition: serde_json::from_slice(&protection::unprotect(&definition)?)?,
            })
        },
    )
    .transpose()
}

fn evaluate(db: &Connection, scope: &ResultScope) -> Result<GoalVerification> {
    let candidate_id: Option<String> = db
        .query_row(
            "SELECT candidate_id FROM goal_criterion_evidence
         WHERE run_id = ?1 AND candidate_id IS NOT NULL ORDER BY sequence LIMIT 1",
            [&scope.run_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut read = db.prepare_cached(
        "SELECT payload FROM goal_criterion_evidence
         WHERE run_id = ?1 AND criterion_ordinal = ?2 ORDER BY sequence DESC LIMIT 1",
    )?;
    let mut criteria = Vec::with_capacity(scope.definition.criteria.len());
    for (index, text) in scope.definition.criteria.iter().enumerate() {
        let ordinal = index + 1;
        let payload: Option<Vec<u8>> = read
            .query_row(params![scope.run_id, i64::try_from(ordinal)?], |row| {
                row.get(0)
            })
            .optional()?;
        let evidence = payload
            .map(|payload| -> Result<GoalCriterionEvidence> {
                Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?)
            })
            .transpose()?;
        let status = match &evidence {
            Some(evidence) => evaluate_evidence(db, scope, evidence)?.1,
            None => GoalCriterionStatus::Pending,
        };
        criteria.push(GoalCriterionVerification {
            ordinal,
            text: text.clone(),
            status,
            evidence,
        });
    }
    let status = if criteria
        .iter()
        .any(|criterion| criterion.status == GoalCriterionStatus::Failed)
    {
        GoalVerificationStatus::Failed
    } else if criteria
        .iter()
        .all(|criterion| criterion.status == GoalCriterionStatus::Passed)
    {
        GoalVerificationStatus::Completed
    } else {
        GoalVerificationStatus::Verifying
    };
    Ok(GoalVerification {
        revision_id: scope.revision_id.clone(),
        run_id: scope.run_id.clone(),
        context_id: scope.context_id.clone(),
        provider_result: scope.provider_result.clone(),
        candidate_id,
        status,
        criteria,
    })
}

fn evaluate_evidence(
    db: &Connection,
    scope: &ResultScope,
    evidence: &GoalCriterionEvidence,
) -> Result<(Option<String>, GoalCriterionStatus)> {
    use GoalCriterionStatus::{Failed, Passed};
    match evidence {
        GoalCriterionEvidence::OperatorReview { approved, .. } => {
            Ok((None, if *approved { Passed } else { Failed }))
        }
        GoalCriterionEvidence::CandidateGate {
            verification_id,
            gate_index,
        } => {
            let task = scope
                .task
                .as_deref()
                .context("Candidate evidence requires a linked task")?;
            let verification =
                load_scoped_candidate_verification(db, &scope.project, task, verification_id)?
                    .context("The test evidence is outside the goal result's project or task")?;
            let candidate =
                load_selected_candidate(db, &scope.project, task, &verification.candidate_id)?;
            require_result_candidate(scope, &candidate)?;
            let gate = verification
                .gates
                .get(*gate_index)
                .context("The gate does not exist in this verification")?;
            let status = gate_status(gate.outcome.as_ref());
            Ok((Some(candidate.id), status))
        }
        GoalCriterionEvidence::CandidateAcceptance { acceptance_id } => {
            let task = scope
                .task
                .as_deref()
                .context("Candidate evidence requires a linked task")?;
            let acceptance =
                load_scoped_candidate_acceptance(db, &scope.project, task, acceptance_id)?
                    .context(
                        "The approval evidence is outside the goal result's project or task",
                    )?;
            require_result_candidate(scope, &acceptance.candidate)?;
            ensure!(
                acceptance.verification.allows_acceptance(),
                "The selected result was not approved"
            );
            Ok((Some(acceptance.candidate.id), Passed))
        }
    }
}

fn gate_status(outcome: Option<&CandidateGateOutcome>) -> GoalCriterionStatus {
    match outcome {
        Some(CandidateGateOutcome::Passed { .. }) => GoalCriterionStatus::Passed,
        Some(_) => GoalCriterionStatus::Failed,
        None => GoalCriterionStatus::Pending,
    }
}

fn require_result_candidate(scope: &ResultScope, candidate: &SelectedCandidate) -> Result<()> {
    ensure!(
        candidate.target.run_id == scope.run_id && candidate.checkout_id == scope.checkout_id,
        "The candidate belongs to a different recorded goal result"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::repository::verification::{CandidateSelection, CandidateTarget};

    use super::*;

    #[test]
    fn goal_verification_reads_the_typed_result_and_rejects_a_different_candidate_run() -> Result<()>
    {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE selected_candidate_revisions(id TEXT PRIMARY KEY, project_id TEXT, task_id TEXT, payload BLOB);
             CREATE TABLE candidate_verifications(id TEXT PRIMARY KEY, candidate_id TEXT, policy_revision TEXT,
                 started_at TEXT, finished_at TEXT, gates BLOB);
             CREATE TABLE candidate_gate_results(verification_id TEXT, gate_index INTEGER, payload BLOB);
             CREATE TABLE candidate_acceptances(id TEXT PRIMARY KEY, candidate_id TEXT, verification_id TEXT, accepted_at TEXT);",
        )?;
        let mut scope = ResultScope {
            project: "project".into(),
            task: Some("task".into()),
            checkout_id: "checkout".into(),
            revision_id: "goal-revision".into(),
            run_id: "run".into(),
            context_id: "context".into(),
            provider_result: Some("completed".into()),
            definition: GoalDefinition {
                objective: "Goal".into(),
                criteria: vec!["Test the result".into()],
                max_turns: None,
                max_seconds: None,
                token_budget: None,
            },
        };
        let candidate = SelectedCandidate {
            id: "candidate".into(),
            target: CandidateTarget {
                project_id: scope.project.clone(),
                task_id: "task".into(),
                unit_id: "unit".into(),
                run_id: scope.run_id.clone(),
                capture_revision: "frozen-capture".into(),
            },
            checkout_id: scope.checkout_id.clone(),
            branch: "work".into(),
            base_commit: "base".into(),
            tree: "frozen-tree".into(),
            selection: CandidateSelection::default(),
        };
        let protected =
            |value: serde_json::Value| protection::protect(&serde_json::to_vec(&value)?);
        db.execute(
            "INSERT INTO selected_candidate_revisions VALUES (?1,?2,?3,?4)",
            params![
                candidate.id,
                scope.project,
                scope.task,
                protected(serde_json::to_value(&candidate)?)?
            ],
        )?;
        db.execute(
            "INSERT INTO candidate_verifications VALUES ('test','candidate','policy','time',NULL,?1)",
            [protected(json!([{
                "gate": { "name":"Build","required":true,
                    "action":{"type":"command","executable":"cmd.exe","arguments":[],"timeoutSeconds":1}},
                "executable_revision":"fixture-binary"
            }]))?],
        )?;
        let gate = GoalCriterionEvidence::CandidateGate {
            verification_id: "test".into(),
            gate_index: 0,
        };
        assert_eq!(
            evaluate_evidence(&db, &scope, &gate)?.1,
            GoalCriterionStatus::Pending
        );
        db.execute(
            "INSERT INTO candidate_gate_results VALUES ('test',0,?1)",
            [protected(
                json!({"status":"failed","exitCode":2,"stdout":"","stderr":"","truncated":false}),
            )?],
        )?;
        assert_eq!(
            evaluate_evidence(&db, &scope, &gate)?.1,
            GoalCriterionStatus::Failed
        );
        db.execute(
            "UPDATE candidate_gate_results SET payload=?1",
            [protected(
                json!({"status":"passed","stdout":"","stderr":"","truncated":false}),
            )?],
        )?;
        assert_eq!(
            evaluate_evidence(&db, &scope, &gate)?.1,
            GoalCriterionStatus::Passed
        );

        let approval = GoalCriterionEvidence::CandidateAcceptance {
            acceptance_id: "review".into(),
        };
        assert!(evaluate_evidence(&db, &scope, &approval).is_err());
        db.execute(
            "UPDATE candidate_verifications SET finished_at='sealed'",
            [],
        )?;
        db.execute(
            "INSERT INTO candidate_acceptances VALUES ('review','candidate','test','accepted')",
            [],
        )?;
        assert_eq!(
            evaluate_evidence(&db, &scope, &approval)?,
            (Some(candidate.id), GoalCriterionStatus::Passed)
        );
        scope.run_id = "different checkpoint".into();
        assert!(evaluate_evidence(&db, &scope, &gate).is_err());
        assert!(evaluate_evidence(&db, &scope, &approval).is_err());
        Ok(())
    }
}
