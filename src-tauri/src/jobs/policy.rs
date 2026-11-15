use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    providers::{Capability, SessionMode},
    store::Store,
};

use super::{Job, JobOrigin, ReleaseBlocker, load_job, pipeline::release_blocker};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum JobRole {
    Operator,
    Agent,
    Pipeline,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/")]
pub enum JobWorkspace {
    Project,
    Checkout { checkout_id: String },
    Task { task_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/")]
pub enum JobPermission {
    Allow,
    Review,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobRule {
    pub role: JobRole,
    pub operation: SessionMode,
    pub workspace: JobWorkspace,
    pub permission: JobPermission,
    #[serde(default)]
    pub requirements: JobRequirements,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "../../src/bindings/")]
pub struct JobRequirements {
    pub capabilities: Vec<Capability>,
    pub customizations: Vec<RequiredCustomization>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/")]
pub enum RequiredCustomization {
    Instruction { path: String },
    Skill { id: String },
    Plugin { id: String },
    ExecutableEntry { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/bindings/")]
pub enum DispatchRejection {
    PolicyChanged,
    PipelineRelease {
        reason: ReleaseBlocker,
    },
    ProfileUnavailable,
    WorktreeUnavailable,
    Requirements {
        capabilities: Vec<Capability>,
        customizations: Vec<RequiredCustomization>,
    },
}

pub(super) struct EffectivePolicy {
    pub permission: JobPermission,
    pub key: String,
    pub requirements: JobRequirements,
}

fn rules(db: &Connection, project: &str) -> Result<Vec<JobRule>> {
    let serialized: Option<String> = db
        .query_row(
            "SELECT rules FROM project_job_policies WHERE project_id = ?1",
            [project],
            |row| row.get(0),
        )
        .optional()?;
    serialized
        .map(|rules| serde_json::from_str(&rules).context("Invalid saved project job policy"))
        .unwrap_or_else(|| Ok(Vec::new()))
}

pub(super) fn effective_policy(db: &Connection, job: &Job) -> Result<EffectivePolicy> {
    let role = match job.origin {
        JobOrigin::Operator => JobRole::Operator,
        JobOrigin::Agent { .. } => JobRole::Agent,
        JobOrigin::Pipeline => JobRole::Pipeline,
    };
    let matched = rules(db, &job.project_id)?.into_iter().find(|rule| {
        rule.role == role
            && rule.operation == job.mode
            && match &rule.workspace {
                JobWorkspace::Project => true,
                JobWorkspace::Checkout { checkout_id } => checkout_id == &job.checkout_id,
                JobWorkspace::Task { task_id } => job.task_id.as_ref() == Some(task_id),
            }
    });
    if let Some(rule) = matched {
        return Ok(EffectivePolicy {
            permission: rule.permission,
            key: format!("{:x}", Sha256::digest(serde_json::to_vec(&rule)?)),
            requirements: rule.requirements,
        });
    }
    Ok(EffectivePolicy {
        permission: if role == JobRole::Agent || job.mode == SessionMode::ApprovedWrite {
            JobPermission::Review
        } else {
            JobPermission::Allow
        },
        key: "default".into(),
        requirements: JobRequirements::default(),
    })
}

pub(super) fn bind_attempt_policy(db: &Connection, job: &Job, run: &str) -> Result<()> {
    db.execute(
        "INSERT INTO job_attempt_policies (run_id, policy_key) VALUES (?1, ?2)",
        params![run, effective_policy(db, job)?.key],
    )?;
    Ok(())
}

fn attempt_requirements(
    db: &Connection,
    run: &str,
) -> Result<std::result::Result<JobRequirements, DispatchRejection>> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT p.policy_key, j.project_id, j.id FROM job_attempt_policies p
         JOIN job_attempts a ON a.run_id = p.run_id JOIN jobs j ON j.id = a.job_id
         WHERE p.run_id = ?1",
            [run],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((accepted, project, id)) = row else {
        return Ok(Ok(JobRequirements::default()));
    };
    let job = load_job(db, &project, &id)?.context("Job is unavailable")?;
    if let Some(reason) = release_blocker(db, &job)? {
        return Ok(Err(DispatchRejection::PipelineRelease { reason }));
    }
    let policy = effective_policy(db, &job)?;
    if accepted != policy.key {
        return Ok(Err(DispatchRejection::PolicyChanged));
    }
    Ok(Ok(policy.requirements))
}

pub(crate) fn validate_attempt_policy(db: &Connection, run: &str) -> Result<bool> {
    if let Err(rejection) = attempt_requirements(db, run)? {
        save_rejection(db, run, &rejection)?;
        return Ok(false);
    }
    Ok(true)
}

pub(super) fn dispatch_rejection(db: &Connection, run: &str) -> Result<Option<DispatchRejection>> {
    let row: Option<String> = db.query_row(
        "SELECT rejection FROM job_attempt_policies WHERE run_id = ?1 AND rejection IS NOT NULL",
        [run],
        |row| row.get(0),
    ).optional()?;
    row.map(|json| Ok(serde_json::from_str(&json)?)).transpose()
}

fn save_rejection(db: &Connection, run: &str, rejection: &DispatchRejection) -> Result<()> {
    db.execute(
        "UPDATE job_attempt_policies SET rejection = ?2 WHERE run_id = ?1 AND rejection IS NULL",
        params![run, serde_json::to_string(rejection)?],
    )?;
    Ok(())
}

pub(super) fn reconcile_approval(db: &Connection, job: &Job) -> Result<()> {
    let effective = effective_policy(db, job)?;
    let prior: Option<(String, String)> = db
        .query_row(
            "SELECT policy_key, decision FROM job_approvals WHERE job_id = ?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if prior.as_ref().is_some_and(|(key, decision)| {
        decision == "denied"
            || (key == &effective.key && effective.permission == JobPermission::Review)
    }) {
        return Ok(());
    }
    if prior.is_some() {
        db.execute("DELETE FROM job_approvals WHERE job_id = ?1", [&job.id])?;
    }
    if effective.permission == JobPermission::Review {
        // The notice points to the pending job rather than a particular approval version.
        db.execute(
            "DELETE FROM job_notices WHERE job_id = ?1 AND kind = 'job_approval'",
            [&job.id],
        )?;
        db.execute(
            "INSERT INTO job_approvals (id, job_id, decision, policy_key)
             VALUES (?1, ?2, 'pending', ?3)",
            params![Uuid::new_v4().to_string(), job.id, effective.key],
        )?;
    }
    Ok(())
}

fn check_project(db: &Connection, project: &str) -> Result<()> {
    let exists = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
        [project],
        |row| row.get::<_, bool>(0),
    )?;
    ensure!(exists, "Project is unavailable");
    Ok(())
}

impl Store {
    pub(crate) async fn job_dispatch_requirements(&self, run: String) -> Result<JobRequirements> {
        let requirements = self
            .execute(move |db| match attempt_requirements(db, &run)? {
                Ok(requirements) => Ok(Some(requirements)),
                Err(rejection) => {
                    save_rejection(db, &run, &rejection)?;
                    Ok(None)
                }
            })
            .await?;
        requirements.context("Job authorization changed before dispatch")
    }

    pub(crate) async fn record_dispatch_rejection(
        &self,
        run: String,
        rejection: DispatchRejection,
    ) -> Result<()> {
        self.execute(move |db| save_rejection(db, &run, &rejection))
            .await
    }

    pub async fn project_job_rules(&self, project: String) -> Result<Vec<JobRule>> {
        self.execute(move |db| {
            check_project(db, &project)?;
            rules(db, &project)
        })
        .await
    }

    pub async fn set_project_job_rules(
        &self,
        project: String,
        expected: Vec<JobRule>,
        updated: Vec<JobRule>,
    ) -> Result<Vec<JobRule>> {
        let updated = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                check_project(&tx, &project)?;
                ensure!(
                    rules(&tx, &project)? == expected,
                    "The project rules changed. Reload them before saving."
                );
                for rule in &updated {
                    let valid_scope = match &rule.workspace {
                        JobWorkspace::Project => true,
                        JobWorkspace::Checkout { checkout_id } => tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM checkouts WHERE project_id = ?1 AND id = ?2)",
                            params![project, checkout_id],
                            |row| row.get(0),
                        )?,
                        JobWorkspace::Task { task_id } => tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE project_id = ?1 AND id = ?2)",
                            params![project, task_id],
                            |row| row.get(0),
                        )?,
                    };
                    ensure!(valid_scope, "A job rule targets a workspace outside the project");
                    for requirement in &rule.requirements.customizations {
                        match requirement {
                            RequiredCustomization::Instruction { path } => ensure!(
                                !path.is_empty() && std::path::Path::new(path).components().all(|part| matches!(part, std::path::Component::Normal(_))),
                                "An instruction requirement must name a file relative to the worktree"
                            ),
                            RequiredCustomization::Skill { id }
                            | RequiredCustomization::Plugin { id }
                            | RequiredCustomization::ExecutableEntry { id } => ensure!(
                                !id.trim().is_empty(),
                                "A customization requirement needs an identifier"
                            ),
                        }
                    }
                }
                tx.execute(
                    "INSERT INTO project_job_policies (project_id, rules) VALUES (?1, ?2)
                     ON CONFLICT(project_id) DO UPDATE SET rules = excluded.rules",
                    params![project, serde_json::to_string(&updated)?],
                )?;
                let pending = tx.prepare(
                    "SELECT id FROM jobs j WHERE project_id = ?1 AND queue_state <> 'cancelled'
                     AND (NOT EXISTS(SELECT 1 FROM job_attempts a WHERE a.job_id = j.id)
                          OR EXISTS(SELECT 1 FROM job_recoveries r WHERE r.job_id = j.id))",
                )?.query_map([&project], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for id in pending {
                    let job = load_job(&tx, &project, &id)?.context("Pending job disappeared")?;
                    reconcile_approval(&tx, &job)?;
                }
                tx.commit()?;
                Ok(updated)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(updated)
    }
}
