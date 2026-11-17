use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::repository::{
    IntegratedWorktreeBase,
    verified_git::load_verified_delivery_commit,
    verified_merge::{
        DeliveryMergeRequest, GithubSyncOutcome, SourceSync, create_github_reattachment,
        create_github_sync, validate_github_source_sync,
    },
};

use super::{Delivery, PullEvidence, open, seal};

const MAX_SOURCE_SYNCS: usize = 32;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SourcePublication {
    project: String,
    repository: u64,
    branch: String,
    task: String,
    run: String,
    plan: String,
    checkout: String,
    policy: String,
    accepted: String,
    commit_evidence: String,
    previous: Option<String>,
    pub(super) target: String,
    pub(super) source_syncs: Vec<SourceSync>,
}

impl SourcePublication {
    fn verify_scope(&self, db: &Connection, delivery: &Delivery) -> Result<()> {
        let request = &delivery.merge;
        ensure!(
            self.project == request.project_id
                && self.repository == delivery.settings.repository_id
                && self.branch == request.expected_source_branch
                && self.task == request.task_id
                && self.run == request.workflow_run_id
                && self.checkout == delivery.checkout
                && self.policy == delivery.revision
                && self.source_syncs.len() <= MAX_SOURCE_SYNCS,
            "The prior GitHub source belongs to a different delivery scope"
        );
        require_origin(db, self)
    }
}

fn require_origin(db: &Connection, source: &SourcePublication) -> Result<()> {
    let commit =
        load_verified_delivery_commit(db, &source.project, &source.task, &source.commit_evidence)?
            .context("The prior GitHub source lost its verified commit")?;
    ensure!(
        commit.sha == source.accepted
            && commit.branch == source.branch
            && commit.checkout_id == source.checkout
            && commit_plan(
                db,
                &source.project,
                &source.task,
                &source.run,
                &source.checkout,
                &source.commit_evidence
            )?
            .as_ref()
                == Some(&source.plan),
        "The prior GitHub source does not match its approved delivery lineage"
    );
    Ok(())
}

fn commit_plan(
    db: &Connection,
    project: &str,
    task: &str,
    run: &str,
    checkout: &str,
    commit: &str,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT plan_revision FROM workflow_delivery_units
         WHERE project_id = ?1 AND task_id = ?2 AND pipeline_run_id = ?3 AND checkout_id = ?4
           AND commit_evidence_id = ?5",
            params![project, task, run, checkout, commit],
            |row| row.get(0),
        )
        .optional()?)
}

#[derive(Clone, PartialEq, Eq)]
struct BranchIntent {
    task: String,
    previous: Option<String>,
    target: String,
    published: bool,
}

fn read_branch(
    db: &Connection,
    project: &str,
    repository: u64,
    branch: &str,
) -> Result<Option<BranchIntent>> {
    Ok(db
        .query_row(
            "SELECT task_id, previous_head, target_head, published FROM workflow_github_branches
         WHERE project_id = ?1 AND repository_id = ?2 AND branch = ?3",
            params![project, repository.to_string(), branch],
            |row| {
                Ok(BranchIntent {
                    task: row.get(0)?,
                    previous: row.get(1)?,
                    target: row.get(2)?,
                    published: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn check_observed(intent: Option<&BranchIntent>, task: &str, observed: Option<&str>) -> Result<()> {
    if let Some(intent) = intent {
        ensure!(
            intent.task == task,
            "The GitHub branch is reserved for a different task"
        );
        ensure!(
            observed == Some(intent.target.as_str())
                || (!intent.published && observed == intent.previous.as_deref()),
            "The GitHub branch differs from its authorized publication history"
        );
    } else {
        ensure!(
            observed.is_none(),
            "An unrecognized remote GitHub branch already exists"
        );
    }
    Ok(())
}

fn read_proof(
    db: &Connection,
    project: &str,
    repository: u64,
    branch: &str,
    target: &str,
) -> Result<Option<SourcePublication>> {
    let row = db.query_row(
        "SELECT task_id, previous_head, accepted_sha, policy_revision, proof FROM workflow_github_source_syncs
         WHERE project_id = ?1 AND repository_id = ?2 AND branch = ?3 AND target_head = ?4",
        params![project, repository.to_string(), branch, target],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, Vec<u8>>(4)?)),
    ).optional()?;
    row.map(|(task, previous, accepted, policy, bytes)| {
        let proof: SourcePublication = open(&bytes)?;
        ensure!(
            proof.project == project
                && proof.repository == repository
                && proof.branch == branch
                && proof.target == target
                && proof.task == task
                && proof.previous == previous
                && proof.accepted == accepted
                && proof.policy == policy,
            "The protected GitHub source proof does not match its branch record"
        );
        Ok(proof)
    })
    .transpose()
}

fn has_legacy_head(db: &Connection, delivery: &Delivery, head: &str) -> Result<()> {
    let request = &delivery.merge;
    let id: String = db.query_row(
        "SELECT id FROM verified_delivery_commits WHERE project_id = ?1 AND task_id = ?2 AND sha = ?3",
        params![request.project_id, request.task_id, head], |row| row.get(0),
    ).context("The preexisting GitHub source lacks a native verified commit")?;
    let commit = load_verified_delivery_commit(db, &request.project_id, &request.task_id, &id)?
        .context("The preexisting GitHub source lost its verified commit")?;
    ensure!(
        commit.sha == head
            && commit.branch == request.expected_source_branch
            && commit.checkout_id == delivery.checkout
            && commit_plan(
                db,
                &request.project_id,
                &request.task_id,
                &request.workflow_run_id,
                &delivery.checkout,
                &id
            )?
            .is_some(),
        "The preexisting GitHub source is outside the approved task"
    );
    Ok(())
}

pub(super) fn prepare(
    db: &Connection,
    delivery: &Delivery,
    plan: &str,
    observed: Option<&str>,
) -> Result<Option<SourcePublication>> {
    let request = &delivery.merge;
    let repository = delivery.settings.repository_id;
    let branch = &request.expected_source_branch;
    let current = read_branch(db, &request.project_id, repository, branch)?;
    check_observed(current.as_ref(), &request.task_id, observed)?;
    let prior = match observed {
        Some(head) => {
            let proof = read_proof(db, &request.project_id, repository, branch, head)?;
            if let Some(proof) = &proof {
                proof.verify_scope(db, delivery)?;
            } else {
                has_legacy_head(db, delivery, head)?;
            }
            proof
        }
        None => None,
    };
    let mut syncs = prior
        .as_ref()
        .map_or_else(Vec::new, |proof| proof.source_syncs.clone());
    if let Some(prior) = &prior
        && !syncs.is_empty()
        && prior.commit_evidence != request.commit_evidence_id
    {
        match create_github_reattachment(db, request, &syncs)? {
            GithubSyncOutcome::Conflict => return Ok(None),
            GithubSyncOutcome::Ready(step) => syncs.push(step),
            GithubSyncOutcome::Unchanged => {}
        }
    }
    match create_github_sync(db, request, &syncs, &request.expected_destination_sha)? {
        GithubSyncOutcome::Conflict => return Ok(None),
        GithubSyncOutcome::Ready(step) => syncs.push(step),
        GithubSyncOutcome::Unchanged => {}
    }
    ensure!(
        syncs.len() <= MAX_SOURCE_SYNCS,
        "The GitHub source history reached its synchronization limit"
    );
    let target = validate_github_source_sync(db, request, &syncs)?;
    if let Some(remote_head) = observed {
        IntegratedWorktreeBase {
            branch: branch.clone(),
            prerequisite_commits: vec![remote_head.to_owned()],
            verified_tip: Some(target.clone()),
        }
        .registered_tip(db, &request.project_id, &delivery.checkout)
        .context("The synchronized GitHub branch would not retain the authorized remote head")?;
    }
    let mut publication = SourcePublication {
        project: request.project_id.clone(),
        repository,
        branch: branch.clone(),
        task: request.task_id.clone(),
        run: request.workflow_run_id.clone(),
        plan: plan.to_owned(),
        checkout: delivery.checkout.clone(),
        policy: delivery.revision.clone(),
        accepted: request.expected_source_sha.clone(),
        commit_evidence: request.commit_evidence_id.clone(),
        previous: observed.map(str::to_owned),
        target,
        source_syncs: syncs,
    };
    if let Some(intent) = &current
        && intent.target == publication.target
    {
        publication.previous.clone_from(&intent.previous);
    }
    if let Some(previous) = read_proof(
        db,
        &publication.project,
        repository,
        branch,
        &publication.target,
    )? {
        if observed == Some(publication.target.as_str())
            && current
                .as_ref()
                .is_some_and(|intent| intent.target != publication.target)
        {
            publication.previous.clone_from(&previous.previous);
        }
        ensure!(
            publication == previous,
            "The selected GitHub head has a different recorded publication"
        );
    } else {
        publication.persist(db)?;
    }
    if current
        .as_ref()
        .is_none_or(|intent| intent.target != publication.target)
    {
        db.execute(
            "INSERT INTO workflow_github_branches VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
             ON CONFLICT(project_id, repository_id, branch) DO UPDATE
             SET previous_head = excluded.previous_head, target_head = excluded.target_head, published = 0",
            params![publication.project, repository.to_string(), publication.branch, publication.task,
                publication.previous, publication.target],
        )?;
    }
    Ok(Some(publication))
}

impl SourcePublication {
    fn persist(&self, db: &Connection) -> Result<()> {
        db.execute(
            "INSERT INTO workflow_github_source_syncs
             (project_id, repository_id, branch, target_head, task_id, previous_head, accepted_sha, policy_revision, proof)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![self.project, self.repository.to_string(), self.branch, self.target,
                self.task, self.previous, self.accepted, self.policy, seal(self)?],
        )?;
        Ok(())
    }

    pub(super) fn previous_head(&self) -> Option<&str> {
        self.previous.as_deref()
    }

    pub(super) fn acknowledge(&self, db: &Connection) -> Result<()> {
        let count = db.execute(
            "UPDATE workflow_github_branches SET published = 1 WHERE project_id = ?1 AND repository_id = ?2
             AND branch = ?3 AND task_id = ?4 AND target_head = ?5 AND previous_head IS ?6",
            params![self.project, self.repository.to_string(), self.branch, self.task, self.target, self.previous],
        )?;
        ensure!(
            count == 1,
            "The GitHub branch intent changed during publication"
        );
        Ok(())
    }
}

pub(crate) fn validate_published_pull_source(
    db: &Connection,
    project: &str,
    run: &str,
    plan: &str,
    evidence: &PullEvidence,
    request: &DeliveryMergeRequest,
) -> Result<()> {
    let pull = &evidence.pull;
    ensure!(
        request.project_id == project
            && request.workflow_run_id == run
            && request.task_id == evidence.task_id
            && request.commit_evidence_id == evidence.commit_evidence_id
            && request.expected_source_branch == pull.head_branch
            && request.destination_branch == pull.base_branch
            && request.expected_destination_sha == pull.base_sha
            && evidence.source_syncs.len() <= MAX_SOURCE_SYNCS,
        "The pull request source does not match the approved delivery"
    );
    let intent = read_branch(db, project, pull.repository_id, &pull.head_branch)?
        .context("The pull request lost its GitHub branch publication")?;
    ensure!(
        intent.published && intent.task == evidence.task_id && intent.target == pull.head_sha,
        "The pull request source does not match the published branch"
    );
    if let Some(proof) = read_proof(
        db,
        project,
        pull.repository_id,
        &pull.head_branch,
        &pull.head_sha,
    )? {
        ensure!(
            proof.task == evidence.task_id
                && proof.previous == intent.previous
                && proof.run == run
                && proof.plan == plan
                && proof.checkout == evidence.checkout_id
                && proof.commit_evidence == evidence.commit_evidence_id
                && proof.accepted == request.expected_source_sha
                && proof.policy == evidence.policy_revision
                && proof.source_syncs == evidence.source_syncs,
            "The pull request differs from its protected GitHub source proof"
        );
        require_origin(db, &proof)?;
    } else {
        ensure!(
            evidence.source_syncs.is_empty() && pull.head_sha == request.expected_source_sha,
            "The synchronized GitHub source lost its protected proof"
        );
    }
    ensure!(
        validate_github_source_sync(db, request, &evidence.source_syncs)? == pull.head_sha,
        "The pull request head does not match the verified source history"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_source_transitions_require_a_recorded_replay_or_previous_head() -> Result<()> {
        assert!(check_observed(None, "task", Some("external")).is_err());
        check_observed(None, "task", None)?;
        let mut intent = BranchIntent {
            task: "task".into(),
            previous: Some("first".into()),
            target: "second".into(),
            published: false,
        };
        check_observed(Some(&intent), "task", Some("first"))?;
        check_observed(Some(&intent), "task", Some("second"))?;
        assert!(check_observed(Some(&intent), "other", Some("second")).is_err());
        assert!(check_observed(Some(&intent), "task", Some("third")).is_err());
        intent.published = true;
        check_observed(Some(&intent), "task", Some("second"))?;
        assert!(check_observed(Some(&intent), "task", Some("first")).is_err());
        assert!(check_observed(Some(&intent), "task", None).is_err());
        Ok(())
    }

    #[test]
    fn github_source_proof_remains_bound_to_the_original_branch_intent_after_restart() -> Result<()>
    {
        let fixture = tempfile::tempdir()?;
        let file = fixture.path().join("github.sqlite");
        let proof = SourcePublication {
            project: "project".into(),
            repository: 7,
            branch: "delivery".into(),
            task: "task".into(),
            run: "run".into(),
            plan: "plan".into(),
            checkout: "checkout".into(),
            policy: "policy".into(),
            accepted: "head".into(),
            commit_evidence: "commit".into(),
            previous: Some("head".into()),
            target: "sync".into(),
            source_syncs: vec![SourceSync {
                kind: crate::repository::verified_merge::SourceSyncKind::Destination,
                accepted_commit_evidence_id: "commit".into(),
                previous_head: "head".into(),
                second_parent_sha: "destination".into(),
                head_sha: "sync".into(),
                tree_sha: "tree".into(),
            }],
        };
        {
            let db = Connection::open(&file)?;
            db.execute_batch(
                "PRAGMA foreign_keys=OFF; CREATE TABLE projects(id TEXT PRIMARY KEY);
                 CREATE TABLE workflow_automatic_runs(pipeline_run_id TEXT PRIMARY KEY);
                 CREATE TABLE workflow_node_pins(pipeline_run_id TEXT, node_instance_id TEXT,
                   PRIMARY KEY (pipeline_run_id, node_instance_id));",
            )?;
            db.execute_batch(include_str!(
                "../../../../migrations/123-workflow-github.sql"
            ))?;
            db.execute_batch(include_str!(
                "../../../../migrations/127-workflow-github-source-sync.sql"
            ))?;
            proof.persist(&db)?;
            db.execute(
                "INSERT INTO workflow_github_branches VALUES ('project', '7', 'delivery', 'task', 'head', 'sync', 0)", [],
            )?;
        }
        let db = Connection::open(file)?;
        let restored = read_proof(&db, "project", 7, "delivery", "sync")?
            .context("No restored publication authorization")?;
        assert!(restored == proof);
        restored.acknowledge(&db)?;
        let branch =
            read_branch(&db, "project", 7, "delivery")?.context("No acknowledged branch intent")?;
        assert!(branch.published);
        assert!(check_observed(Some(&branch), "task", Some("unrecorded")).is_err());
        db.execute(
            "UPDATE workflow_github_source_syncs SET previous_head = 'unrecorded'",
            [],
        )?;
        assert!(read_proof(&db, "project", 7, "delivery", "sync").is_err());
        Ok(())
    }
}
