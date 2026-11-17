mod client;
mod delivery;
mod policy;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{diagnostics, protection, repository::github::GithubCli, store::Store};

use super::{
    ArtifactKind, ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, PinnedNode,
    WorkflowArtifact, WorkflowRun,
    artifacts::{insert_scoped_artifact, load_artifact},
    control::{refresh, require_stage},
    digest,
    inspection::record_workflow_input_ids,
    runtime::{load_definition, load_pinned_node, load_workflow_run},
};

pub use crate::repository::github::{
    CheckEvidence, CiSnapshot, CiState, GitHubRepository, PublicRepository, PullRequest,
    RepositorySlug, RequiredCheck,
};
pub(crate) use client::GithubDeliveryClient;
pub(crate) use delivery::source::validate_published_pull_source;
pub use policy::GithubSettings;
pub(crate) use policy::verified_destination;

pub(crate) fn workflow_github_merge_destination(
    db: &Connection,
    project: &str,
    run: &str,
) -> Result<Option<String>> {
    let Some(run) = load_workflow_run(db, project, run)? else {
        return Ok(None);
    };
    if let Some((_, destination)) =
        delivery_pin(db, &run.id, run.plan_revision.as_deref().unwrap_or(""))?
    {
        return Ok(Some(destination));
    }
    Ok(workflow_github_merge_settings(db, &run)?.map(|(_, settings)| settings.destination_branch))
}

fn workflow_github_merge_settings(
    db: &Connection,
    run: &WorkflowRun,
) -> Result<Option<(String, GithubSettings)>> {
    let definition = load_definition(db, &run.project_id, &run.version)?
        .context("The pinned workflow definition is unavailable")?;
    if !definition
        .nodes
        .iter()
        .any(|node| node.instance.kind == BuiltinNode::MergePullRequest)
    {
        return Ok(None);
    }
    Ok(Some(policy::load_settings(db, &run.project_id)?.context(
        "Configure the project's GitHub delivery destination before committing this workflow",
    )?))
}

fn delivery_pin(db: &Connection, run: &str, plan: &str) -> Result<Option<(String, String)>> {
    Ok(db
        .query_row(
            "SELECT policy_revision, destination_branch FROM workflow_github_delivery_pins
         WHERE pipeline_run_id = ?1 AND plan_key = ?2",
            params![run, plan],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

pub(crate) fn pin_before_workflow_commit(
    db: &Connection,
    unit: &super::WorkflowDeliveryUnit,
) -> Result<()> {
    let run = load_workflow_run(db, &unit.project_id, &unit.workflow_run_id)?
        .context("The commit belongs to an unavailable workflow")?;
    ensure!(
        run.plan_revision.as_ref() == Some(&unit.plan_revision),
        "The delivery plan changed before committing"
    );
    if let Some((revision, _)) = delivery_pin(db, &run.id, &unit.plan_revision)? {
        policy::require_settings(db, &unit.project_id, &revision)?;
        return Ok(());
    }
    let Some((revision, settings)) = workflow_github_merge_settings(db, &run)? else {
        return Ok(());
    };
    let milestone = crate::tasks::load_workflow_milestone_policy(db, &unit.project_id, &run.id)?;
    ensure!(
        milestone.destination_branch.as_deref() == Some(&settings.destination_branch),
        "The GitHub delivery policy and planned destination differ"
    );
    db.execute(
        "INSERT INTO workflow_github_delivery_pins VALUES (?1, ?2, ?3, ?4)",
        params![
            run.id,
            unit.plan_revision,
            revision,
            settings.destination_branch
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchConfiguration {
    pub limit: u16,
}

impl SearchConfiguration {
    pub(super) fn valid(&self) -> bool {
        (1..=30).contains(&self.limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchEvidence {
    pub query: String,
    pub repositories: Vec<PublicRepository>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullEvidence {
    pub policy_revision: String,
    pub task_id: String,
    pub checkout_id: String,
    pub commit_evidence_id: String,
    pub git_artifact_id: String,
    pub requires_human: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_syncs: Vec<crate::repository::verified_merge::SourceSync>,
    pub pull: PullRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiEvidence {
    pub pull_artifact_id: String,
    pub pull: PullEvidence,
    pub snapshot: CiSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubWait {
    node_id: String,
    unit_id: Option<String>,
    reason: Option<String>,
}

pub(super) fn waits(db: &Connection, run: &WorkflowRun) -> Result<Vec<GithubWait>> {
    let mut query = db.prepare(
        "SELECT node_instance_id, NULLIF(unit_key, ''), reason FROM workflow_github_actions
         WHERE pipeline_run_id = ?1 AND plan_key = ?2 AND port IS NULL
         ORDER BY node_instance_id, unit_key",
    )?;
    let rows = query
        .query_map(
            params![run.id, run.plan_revision.as_deref().unwrap_or("")],
            |row| {
                Ok(GithubWait {
                    node_id: row.get(0)?,
                    unit_id: row.get(1)?,
                    reason: row.get(2)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(super) fn stage_port(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<&'static str>> {
    let plan = run.plan_revision.as_deref().unwrap_or("");
    let unit = unit.unwrap_or("");
    let key = request_key(node, inputs)?;
    let recorded: Option<Option<String>> = db.query_row(
        "SELECT port FROM workflow_github_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4 AND request_key = ?5",
        params![run.id, node.instance.id, unit, plan, key], |row| row.get(0),
    ).optional()?;
    if let Some(port) = recorded {
        return port.as_deref().map(terminal_port).transpose();
    }
    let automatic: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_automatic_runs WHERE pipeline_run_id = ?1)",
        [&run.id],
        |row| row.get(0),
    )?;
    if !automatic {
        return Ok(Some("failed"));
    }
    db.execute(
        "INSERT INTO workflow_github_actions
         (pipeline_run_id, node_instance_id, unit_key, plan_key, request_key, inputs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![run.id, node.instance.id, unit, plan, key, seal(inputs)?],
    )?;
    Ok(None)
}

pub(super) fn stage_reason(
    db: &Connection,
    run: &WorkflowRun,
    node: &PinnedNode,
    unit: Option<&str>,
    inputs: &NodeInputs,
) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT reason FROM workflow_github_actions
         WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3
           AND plan_key = ?4 AND request_key = ?5",
            params![
                run.id,
                node.instance.id,
                unit.unwrap_or(""),
                run.plan_revision.as_deref().unwrap_or(""),
                request_key(node, inputs)?
            ],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

fn terminal_port(port: &str) -> Result<&'static str> {
    Ok(match port {
        "completed" => "completed",
        "passed" => "passed",
        "approval_required" => "approval_required",
        "merged" => "merged",
        "failed" => "failed",
        "stale" => "stale",
        _ => bail!("Unknown recorded GitHub outcome"),
    })
}

fn request_key(node: &PinnedNode, inputs: &NodeInputs) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&(&node.revision, inputs))?))
}

fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn open<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}

#[derive(Clone)]
pub(crate) struct PendingGithubAction {
    project: String,
    run: String,
    node: String,
    unit: String,
    plan: String,
    key: String,
    inputs: NodeInputs,
    remote_request: Option<String>,
}

impl Store {
    pub(crate) async fn next_workflow_github_action(&self) -> Result<Option<PendingGithubAction>> {
        self.execute(|db| {
            let pending = db.query_row(
                "SELECT r.project_id, a.pipeline_run_id, a.node_instance_id, a.unit_key, a.plan_key,
                        a.request_key, a.inputs, a.remote_request_id
                 FROM workflow_github_actions a JOIN job_pipeline_runs r
                   ON r.id = a.pipeline_run_id AND coalesce(r.plan_revision, '') = a.plan_key
                 WHERE a.port IS NULL AND a.retry_after <= unixepoch()
                   AND NOT (SELECT paused FROM job_queue_settings WHERE id = 1)
                 ORDER BY a.retry_after, a.pipeline_run_id, a.node_instance_id, a.unit_key LIMIT 1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get::<_, Vec<u8>>(6)?, row.get(7)?)),
            ).optional()?;
            pending
                .map(
                    |(project, run, node, unit, plan, key, bytes, remote_request)| {
                        Ok(PendingGithubAction {
                            project,
                            run,
                            node,
                            unit,
                            plan,
                            key,
                            inputs: open(&bytes)?,
                            remote_request,
                        })
                    },
                )
                .transpose()
        })
        .await
    }
}

impl PendingGithubAction {
    fn unit(&self) -> Option<&str> {
        (!self.unit.is_empty()).then_some(self.unit.as_str())
    }

    fn input(&self, db: &Connection, name: &str, kind: ArtifactKind) -> Result<WorkflowArtifact> {
        let id = self
            .inputs
            .get(name)
            .with_context(|| format!("GitHub requires input {name}"))?;
        load_artifact(db, &self.project, &self.run, id, kind)?
            .with_context(|| format!("The selected {name} artifact is unavailable"))
    }

    fn validate(&self, db: &Connection) -> Result<PinnedNode> {
        let run = load_workflow_run(db, &self.project, &self.run)?
            .context("The GitHub workflow is unavailable")?;
        ensure!(
            run.plan_revision.as_deref().unwrap_or("") == self.plan,
            "The GitHub action plan changed"
        );
        require_stage(
            db,
            &self.project,
            &self.run,
            &self.node,
            self.unit(),
            &self.inputs,
        )?;
        let node = load_pinned_node(db, &self.project, &self.run, &self.node)?
            .context("The GitHub node is unavailable")?;
        ensure!(
            request_key(&node, &self.inputs)? == self.key,
            "The GitHub input pins changed"
        );
        let pending: bool = db.query_row(
            "SELECT port IS NULL FROM workflow_github_actions
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4 AND request_key = ?5",
            params![self.run, self.node, self.unit, self.plan, self.key], |row| row.get(0),
        )?;
        ensure!(pending, "The GitHub action already has an outcome");
        Ok(node)
    }

    pub(crate) async fn execute(&self, store: &Store) -> Result<()> {
        self.execute_with_client(store, &GithubCli::discover()?)
            .await
    }

    pub(crate) async fn execute_with_client(
        &self,
        store: &Store,
        gh: &impl GithubDeliveryClient,
    ) -> Result<()> {
        let action = self.clone();
        let node = store.execute(move |db| action.validate(db)).await?;
        match node.instance.kind {
            BuiltinNode::SearchPublicRepositories => {
                let NodeConfiguration::SearchPublicRepositories(config) =
                    node.instance.configuration
                else {
                    bail!("GitHub repository search configuration is unavailable");
                };
                let action = self.clone();
                let query = store
                    .execute(move |db| {
                        let ArtifactValue::RenderedText(query) =
                            action.input(db, "query", ArtifactKind::RenderedText)?.value
                        else {
                            unreachable!()
                        };
                        Ok(query)
                    })
                    .await?;
                if query.trim().is_empty()
                    || query.len() > 256
                    || query.contains(['\r', '\n', '\0'])
                {
                    return self
                        .complete(store, "failed", None, Some("GitHub public repository search needs a single-line query of up to 256 bytes".into()))
                        .await;
                }
                let repositories = gh.search_public_repositories(&query, config.limit).await?;
                self.complete(
                    store,
                    "completed",
                    Some((
                        "research",
                        ArtifactValue::GithubResearch(ResearchEvidence {
                            query,
                            repositories,
                        }),
                    )),
                    None,
                )
                .await
            }
            BuiltinNode::OpenOrUpdatePullRequest => self.publish_pull(store, gh).await,
            BuiltinNode::WaitForCi => self.wait_for_ci(store, gh).await,
            BuiltinNode::MergePullRequest => self.merge_pull(store, gh).await,
            _ => bail!("The pinned node is not a GitHub action"),
        }
    }

    async fn complete(
        &self,
        store: &Store,
        port: &'static str,
        output: Option<(&'static str, ArtifactValue)>,
        reason: Option<String>,
    ) -> Result<()> {
        let action = self.clone();
        store
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                action.validate(&tx)?;
                if let Some((_, value)) = &output {
                    let policy = match value {
                        ArtifactValue::GithubPullRequest(pull) => Some(&pull.policy_revision),
                        ArtifactValue::GithubCi(ci) if port != "stale" => {
                            Some(&ci.pull.policy_revision)
                        }
                        _ => None,
                    };
                    if let Some(revision) = policy {
                        policy::require_settings(&tx, &action.project, revision)?;
                    }
                }
                action.write_result(&tx, port, output, reason)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    fn write_result(
        &self,
        db: &Connection,
        port: &'static str,
        output: Option<(&'static str, ArtifactValue)>,
        reason: Option<String>,
    ) -> Result<()> {
        let failed_ci = port == "failed"
            && matches!(&output, Some((_, ArtifactValue::GithubCi(ci))) if ci.snapshot.state == CiState::Failed);
        let output = output
            .map(|(output_port, value)| {
                let artifact = insert_scoped_artifact(
                    db,
                    &self.project,
                    self.run.clone(),
                    self.node.clone(),
                    output_port.into(),
                    self.unit().map(str::to_owned),
                    value,
                )?;
                record_workflow_input_ids(db, &artifact, &self.plan, &self.inputs)?;
                Ok::<_, anyhow::Error>(artifact.id)
            })
            .transpose()?;
        let count = db.execute(
            "UPDATE workflow_github_actions SET port = ?6, output_id = ?7, reason = ?8
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4
               AND request_key = ?5 AND port IS NULL",
            params![self.run, self.node, self.unit, self.plan, self.key, port, output, reason],
        )?;
        ensure!(
            count == 1,
            "The GitHub action already has a recorded outcome"
        );
        if failed_ci {
            self.retry_ci_failure(
                db,
                output
                    .as_deref()
                    .context("The failed CI result lost its evidence")?,
                reason.as_deref(),
            )?;
        }
        refresh(db, &self.project, &self.run)?;
        Ok(())
    }

    fn retry_ci_failure(
        &self,
        db: &Connection,
        artifact_id: &str,
        ci_reason: Option<&str>,
    ) -> Result<()> {
        db.execute_batch("SAVEPOINT github_ci_retry")?;
        let reason = match crate::workflows::backlog::retry::retry_backlog_ci_failure_in_tx(
            db,
            &self.project,
            &self.run,
            artifact_id,
        ) {
            Ok(Some(_)) => {
                db.execute_batch("RELEASE github_ci_retry")?;
                return Ok(());
            }
            Ok(None) => "No automatic CI retry is available for this delivery".to_owned(),
            Err(error) => format!("Could not start the automatic CI retry. {error}"),
        };
        db.execute_batch("ROLLBACK TO github_ci_retry; RELEASE github_ci_retry")?;
        let reason = match ci_reason {
            Some(ci_reason) => format!("{ci_reason}. {reason}"),
            None => reason,
        };
        db.execute(
            "UPDATE workflow_github_actions SET reason = ?6
             WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4
               AND request_key = ?5",
            params![self.run, self.node, self.unit, self.plan, self.key, diagnostics::preview(&reason)],
        )?;
        Ok(())
    }

    pub(crate) async fn defer(&self, store: &Store, error: &anyhow::Error) -> Result<()> {
        self.pending(store, &error.to_string(), None).await
    }

    async fn pending(&self, store: &Store, reason: &str, request: Option<String>) -> Result<()> {
        let action = self.clone();
        let reason = diagnostics::preview(reason);
        store.execute(move |db| {
            db.execute(
                "UPDATE workflow_github_actions SET reason = ?6, retry_after = unixepoch() + 30,
                        remote_request_id = coalesce(?7, remote_request_id)
                 WHERE pipeline_run_id = ?1 AND node_instance_id = ?2 AND unit_key = ?3 AND plan_key = ?4
                   AND request_key = ?5 AND port IS NULL",
                params![action.run, action.node, action.unit, action.plan, action.key, reason, request],
            )?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tasks::{DeliveryCompletionMilestone, load_workflow_milestone_policy},
        workflows::{
            DeliveryRoles, Endpoint, NodeInstance, Wire, WorkflowDeliveryUnit, WorkflowPlanUnit,
            builtin_delivery_graph, tests::approved_delivery_repository,
        },
    };

    #[tokio::test]
    async fn github_standalone_workflow_pins_the_destination_before_the_first_commit() -> Result<()>
    {
        let temporary = tempfile::tempdir()?;
        let store = Store::open(temporary.path().join("github.sqlite"))?;
        let repo = approved_delivery_repository(&store, temporary.path()).await?;
        let project = repo.project_id.clone();
        let settings = GithubSettings {
            account_id: 1,
            repository_id: 2,
            owner: "example".into(),
            repository: "fixture".into(),
            remote: "origin".into(),
            destination_branch: "integration".into(),
            required_checks: vec![RequiredCheck {
                context: "build".into(),
                app_id: Some(3),
            }],
            allow_automatic_merge: false,
            high_risk_paths: None,
        };
        store
            .set_workflow_github_settings(project.clone(), settings.clone())
            .await?;
        let mut graph = builtin_delivery_graph(DeliveryRoles::default());
        graph
            .nodes
            .retain(|node| !matches!(node.id.as_str(), "merge" | "result"));
        graph
            .wires
            .retain(|wire| !matches!(wire.to.node.as_str(), "merge" | "result"));
        graph.nodes.extend(
            [
                ("pull", BuiltinNode::OpenOrUpdatePullRequest),
                ("ci", BuiltinNode::WaitForCi),
                ("remote-merge", BuiltinNode::MergePullRequest),
            ]
            .map(|(id, kind)| NodeInstance {
                id: id.into(),
                kind,
                configuration: NodeConfiguration::None,
            }),
        );
        graph.wires.extend(
            [
                ("commit", "verified", "pull", "ready"),
                ("commit", "git", "pull", "git"),
                ("pull", "completed", "ci", "ready"),
                ("pull", "pull", "ci", "pull"),
                ("ci", "passed", "remote-merge", "ready"),
                ("ci", "ci", "remote-merge", "ci"),
            ]
            .map(|(from, port, to, input)| Wire {
                from: Endpoint {
                    node: from.into(),
                    port: port.into(),
                },
                to: Endpoint {
                    node: to.into(),
                    port: input.into(),
                },
            }),
        );
        let revision = store
            .save_workflow_revision(project.clone(), "github-delivery".into(), graph)
            .await?;
        let run = store.start_workflow_run(project.clone(), revision).await?;
        let unit = WorkflowDeliveryUnit {
            workflow_run_id: run.id,
            plan_revision: "approved-plan".into(),
            project_id: project.clone(),
            task_id: "fixture-task".into(),
            checkout_id: repo.checkout_id,
            unit: WorkflowPlanUnit {
                id: "fixture-unit".into(),
                ordinal: 1,
                purpose: "Ship".into(),
                completion_criteria: vec![],
            },
            commit_evidence_id: None,
        };
        let unit = store
            .execute(move |db| {
                let mut first = unit;
                let plan = crate::workflows::artifacts::insert_artifact(
                    db,
                    &first.project_id,
                    first.workflow_run_id.clone(),
                    "plan".into(),
                    "plan".into(),
                    ArtifactValue::Plan {
                        run_id: "provider-fixture".into(),
                        event_sequence: 1,
                    },
                )?;
                first.plan_revision = plan.id;
                db.execute(
                    "UPDATE job_pipeline_runs SET plan_revision = ?2 WHERE id = ?1",
                    params![first.workflow_run_id, first.plan_revision],
                )?;
                pin_before_workflow_commit(db, &first)?;
                let milestone =
                    load_workflow_milestone_policy(db, &first.project_id, &first.workflow_run_id)?;
                assert_eq!(
                    milestone.milestone,
                    DeliveryCompletionMilestone::VerifiedMerge
                );
                assert_eq!(milestone.destination_branch.as_deref(), Some("integration"));
                Ok(first)
            })
            .await?;
        store
            .set_workflow_github_settings(
                project,
                GithubSettings {
                    destination_branch: "another-destination".into(),
                    ..settings
                },
            )
            .await?;
        store
            .execute(move |db| {
                let milestone =
                    load_workflow_milestone_policy(db, &unit.project_id, &unit.workflow_run_id)?;
                assert_eq!(milestone.destination_branch.as_deref(), Some("integration"));
                assert!(pin_before_workflow_commit(db, &unit).is_err());
                Ok(())
            })
            .await
    }
}
