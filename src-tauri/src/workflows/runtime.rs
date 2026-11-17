use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    conversations::SessionScope,
    jobs::{Job, JobRequest, PipelineAgentInput, PipelineDispatch},
    protection,
    providers::{SessionMode, questions::QuestionRoute},
    runs::RunStatus,
    store::Store,
};

use super::{
    ArtifactKind, ArtifactValue, Executor, NodeConfiguration, NodeInputs, PinnedNode, PortType,
    WorkflowDefinition, WorkflowGraph, WorkflowJobInput, WorkflowRevision, compile,
    authoring::package::roles,
    units::PLAN_UNITS_INSTRUCTIONS,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: String,
    pub project_id: String,
    pub version: WorkflowRevision,
    pub plan_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodePin {
    pub workflow_run_id: String,
    pub workflow: WorkflowRevision,
    pub node_instance_id: String,
    pub instance_revision: String,
    pub definition_revision: u32,
    pub plan_revision: Option<String>,
    pub release_id: Option<String>,
    pub unit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeAttempt {
    pub pin: NodePin,
    pub job_id: String,
    pub run_id: String,
    pub previous_run_id: Option<String>,
    pub ordinal: u32,
    pub status: RunStatus,
}

pub struct WorkflowAgentSubmission {
    pub project_id: String,
    pub dispatch: PipelineDispatch,
    pub scope: SessionScope,
    pub request: JobRequest,
    pub input: PipelineAgentInput,
    pub artifacts: NodeInputs,
    pub unit_id: Option<String>,
}

pub(super) fn insert_workflow_run(
    db: &Connection,
    project: &str,
    workflow: &WorkflowDefinition,
) -> Result<WorkflowRun> {
    super::authoring::ensure_package_runnable(db, project, &workflow.version)?;
    roles::validate_role_pins(db, project, workflow)?;
    ensure!(workflow.nodes.iter().all(|node| !matches!(node.instance.kind, super::BuiltinNode::Subflow | super::BuiltinNode::RepeatUntil | super::BuiltinNode::ForEach)),
        "An unbound workflow invocation cannot launch");
    let id = Uuid::new_v4().to_string();
    db.execute("INSERT INTO job_pipeline_runs(id, project_id) VALUES (?1, ?2)", params![id, project])?;
    db.execute(
        "INSERT INTO workflow_run_pins VALUES (?1, ?2, ?3, ?4)",
        params![id, project, workflow.version.id, workflow.version.revision],
    )?;
    for subflow in &workflow.subflows {
        let scope = match subflow.version.destination {
            super::authoring::package::PackageDestination::Project => format!("project:{project}"),
            super::authoring::package::PackageDestination::Library => "library".into(),
        };
        db.execute(
            "INSERT INTO workflow_subflow_pins VALUES (?1, ?2, ?3, 'subflow', ?4, ?5)",
            params![id, subflow.path, scope, subflow.version.id, subflow.version.revision],
        )?;
    }
    for node in &workflow.nodes {
        let scheduler_kind = if node.definition.executor == Executor::RecordImplementationRelease {
            "implementation_approval"
        } else {
            "execution"
        };
        db.execute(
            "INSERT INTO job_pipeline_nodes (pipeline_run_id, instance_id, kind) VALUES (?1, ?2, ?3)",
            params![id, node.instance.id, scheduler_kind],
        )?;
        let requires_plan = node.definition.inputs.iter().any(|port| port.value == PortType::Data(ArtifactKind::Plan));
        db.execute(
            "INSERT INTO workflow_node_pins VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, node.instance.id, node.revision, node.definition.revision,
                node.definition.executor.scheduler_profile(), requires_plan],
        )?;
    }
    Ok(WorkflowRun { id, project_id: project.into(), version: workflow.version.clone(), plan_revision: None })
}

pub(super) fn load_workflow_run(db: &Connection, project: &str, run: &str) -> Result<Option<WorkflowRun>> {
    Ok(db.query_row(
        "SELECT p.workflow_id, p.workflow_revision, j.plan_revision
         FROM workflow_run_pins p JOIN job_pipeline_runs j ON j.id = p.pipeline_run_id
         WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2",
        params![project, run],
        |row| Ok(WorkflowRun {
            id: run.into(),
            project_id: project.into(),
            version: WorkflowRevision { id: row.get(0)?, revision: row.get(1)? },
            plan_revision: row.get(2)?,
        }),
    ).optional()?)
}

impl Store {
    pub async fn save_workflow_revision(
        &self,
        project: String,
        id: String,
        graph: WorkflowGraph,
    ) -> Result<WorkflowRevision> {
        let workflow = compile(id, graph)?;
        let definition = protection::protect(&serde_json::to_vec(&workflow)?)?;
        self.execute(move |db| {
            for node in &workflow.nodes {
                if let NodeConfiguration::McpTool(configuration) = &node.instance.configuration {
                    super::mcp_tools::validate_mcp_binding(db, &project, configuration)
                        .with_context(|| format!("MCP tool binding for node {}", node.instance.id))?;
                }
            }
            db.execute(
                "INSERT INTO workflow_revisions VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(project_id, workflow_id, revision) DO NOTHING",
                params![
                    project,
                    workflow.version.id,
                    workflow.version.revision,
                    definition
                ],
            )?;
            Ok(workflow.version)
        })
        .await
    }

    pub async fn workflow_definition(
        &self,
        project: String,
        revision: WorkflowRevision,
    ) -> Result<Option<WorkflowDefinition>> {
        self.execute(move |db| load_definition(db, &project, &revision))
            .await
    }

    pub async fn start_workflow_run(
        &self,
        project: String,
        version: WorkflowRevision,
    ) -> Result<WorkflowRun> {
        self.execute(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let workflow = load_definition(&tx, &project, &version)?
                .context("workflow revision is outside this project")?;
            let run = insert_workflow_run(&tx, &project, &workflow)?;
            tx.commit()?;
            Ok(run)
        }).await
    }

    pub async fn workflow_run(&self, project: String, run: String) -> Result<Option<WorkflowRun>> {
        self.execute(move |db| load_workflow_run(db, &project, &run)).await
    }

    pub async fn submit_workflow_agent(&self, submission: WorkflowAgentSubmission) -> Result<Job> {
        self.submit_workflow_agent_with_revision(submission, None)
            .await
    }

    pub async fn submit_workflow_agent_with_comparison(
        &self,
        submission: WorkflowAgentSubmission,
        comparison: Option<super::comparison::ComparisonSelection>,
    ) -> Result<Job> {
        self.submit_workflow_agent_inner(submission, None, false, comparison).await
    }

    pub(super) async fn submit_workflow_agent_with_revision(
        &self,
        submission: WorkflowAgentSubmission,
        plan_revision_request: Option<super::WorkflowPlanRevisionRequest>,
    ) -> Result<Job> {
        self.submit_workflow_agent_inner(submission, plan_revision_request, false, None).await
    }

    pub(crate) async fn submit_workflow_checklist_agent(&self, submission: WorkflowAgentSubmission) -> Result<Job> {
        ensure!(submission.input.target.is_some(), "checklist execution requires a task run target");
        self.submit_workflow_agent_inner(submission, None, true, None).await
    }

    async fn submit_workflow_agent_inner(
        &self,
        submission: WorkflowAgentSubmission,
        plan_revision_request: Option<super::WorkflowPlanRevisionRequest>,
        checklist_execution: bool,
        comparison: Option<super::comparison::ComparisonSelection>,
    ) -> Result<Job> {
        let WorkflowAgentSubmission {
            project_id,
            dispatch,
            scope,
            mut request,
            mut input,
            artifacts,
            unit_id,
        } = submission;
        let (node, resolved) = self
            .workflow_execution_inputs(
                &project_id,
                &dispatch.pipeline_run_id,
                &dispatch.node_instance_id,
                unit_id.as_deref(),
                &artifacts,
            )
            .await?;
        let planning_route = input.pinned.as_ref().map(|(_, route)| *route);
        ensure!(comparison.is_none() || (node.definition.kind == super::BuiltinNode::ReviewPlan && scope.mode == SessionMode::ReadOnly),
            "Comparison responses require a read-only plan review stage");
        let qualified = if let Some(plan) = super::stage_plan::StagePlanConfiguration::for_stage(&node)? {
            scope.mode == plan.mode()
        } else {
            match (node.definition.executor, scope.mode) {
                (Executor::PlanAgent, SessionMode::Plan) => true,
                (Executor::PlanAgent, SessionMode::ReadOnly) => {
                    planning_route == Some(QuestionRoute::StructuredTurn)
                }
                (Executor::ReadOnlyAgent, SessionMode::ReadOnly) => true,
                (Executor::WriteAgent, SessionMode::ApprovedWrite) => true,
                _ => false,
            }
        };
        ensure!(
            qualified,
            "the CLI profile does not match the pinned agent executor"
        );
        let plan = resolved.get("plan");
        let unit = if let Some(id) = &unit_id {
            let task = request
                .task_id
                .clone()
                .context("the logical unit needs an assigned task")?;
            let project = project_id.clone();
            let id = id.clone();
            Some(
                self.execute(move |db| super::load_delivery_unit(db, &project, &task, &id))
                    .await?
                    .context("logical unit is outside the active delivery")?,
            )
        } else {
            None
        };
        ensure!(
            dispatch.plan_revision.as_ref()
                == plan
                    .map(|artifact| &artifact.id)
                    .or(unit.as_ref().map(|unit| &unit.plan_revision)),
            "the scheduled plan must be the revision received on the pinned input"
        );
        if let Some(ArtifactValue::Plan {
            run_id,
            event_sequence,
        }) = plan.map(|artifact| &artifact.value)
        {
            if node.definition.kind == super::BuiltinNode::ReviewPlan {
                let origin = self
                    .workflow_profile_of_attempt(&project_id, run_id)
                    .await?;
                ensure!(
                    origin.provider != scope.provider || origin.profile_id != scope.profile_id,
                    "a cross-review requires a different selected provider profile"
                );
                let checkout = self.workflow_plan_checkout(&project_id, run_id).await?;
                ensure!(
                    request.checkout_id.as_ref() == Some(&checkout),
                    "plan review must use the plan's checkout"
                );
            }
            let proposal = self
                .workflow_plan_text(run_id.clone(), *event_sequence)
                .await?;
            request
                .instructions
                .push_str("\n\nWorkflow plan evidence\n");
            request
                .instructions
                .push_str(&serde_json::to_string(&proposal)?);
        }
        let context = resolved.get("context").map(|artifact| &artifact.value);
        if let Some(ArtifactValue::Markdown(markdown)) = context {
            ensure!(
                request.checkout_id.as_ref() == Some(&markdown.checkout_id),
                "planning must execute in the checkout of its saved document"
            );
            ensure!(
                input
                    .markdown_revision
                    .as_ref()
                    .is_none_or(|revision| revision == &markdown.id),
                "the scheduled Markdown revision differs from the pinned input"
            );
            input.markdown_revision = Some(markdown.id.clone());
            request.instructions.push_str("\n\n");
            request.instructions.push_str(PLAN_UNITS_INSTRUCTIONS);
        } else {
            ensure!(
                input.markdown_revision.is_none(),
                "this node has no Markdown input"
            );
        }
        if let Some(ArtifactValue::Release(release)) =
            resolved.get("release").map(|artifact| &artifact.value)
        {
            let prepared = resolved.get("worktree").map(|artifact| &artifact.value);
            ensure!(
                matches!(prepared, Some(ArtifactValue::Worktree { checkout_id, task })
                if checkout_id == &release.release.checkout_id && task.task_id == release.release.task_id
                    && task.project_id == project_id),
                "the prepared worktree differs from the approved scope"
            );
            ensure!(
                dispatch.release_id.as_ref() == Some(&release.release.id)
                    && dispatch.plan_revision.as_ref() == Some(&release.release.plan_revision)
                    && request.checkout_id.as_ref() == Some(&release.release.checkout_id)
                    && request.task_id.as_ref() == Some(&release.release.task_id),
                "the scheduled implementation must match the checkout, task and plan approved for release"
            );
        } else {
            ensure!(
                dispatch.release_id.is_none(),
                "this node has no release input"
            );
        }
        let NodeConfiguration::AgentRole { role, model, requirements } = &node.instance.configuration else {
            anyhow::bail!("the pinned node has no agent role");
        };
        if !requirements.is_empty() {
            super::stage_requirements::validate_stage_requirements(self, project_id.clone(), requirements.clone()).await?;
        }
        if let Some(version) = &requirements.bound_role {
            let (project, version) = (project_id.clone(), version.clone());
            let template = self.execute(move |db| roles::load_role(db, &project, &version)).await?
                .context("The pinned workflow role is unavailable")?;
            roles::qualify_bound_role(&template, model.as_ref(), &scope)?;
            request.instructions = roles::role_instructions(&template, &request.instructions);
        }
        ensure!(
            model.as_ref().is_none_or(|selection| selection.provider == scope.provider
                && selection.profile_id == scope.profile_id
                && scope.model.as_deref() == Some(selection.model_id.as_str())),
            "the scheduled CLI profile and model must match the pinned stage override"
        );
        ensure!(
            input
                .role_context
                .as_ref()
                .is_none_or(|policy| policy.role == *role)
                && input
                    .target
                    .as_ref()
                    .is_none_or(|target| target.role == *role),
            "submission cannot change the pinned node role"
        );
        if let Some(unit) = &unit {
            request.instructions.push_str("\n\nPinned logical unit\n");
            request
                .instructions
                .push_str(&serde_json::to_string(&unit.unit)?);
            input
                .target
                .get_or_insert_with(|| crate::tasks::TaskRunScope {
                    role: role.clone(),
                    checklist_item_id: None,
                });
        }
        if node.definition.kind == super::BuiltinNode::ReviewCandidate {
            ensure!(
                !request.requires_bridge && input.pinned.is_none(),
                "candidate review must start in its clean checkout without an MCP bridge"
            );
            let Some(ArtifactValue::Candidate(candidate)) =
                resolved.get("candidate").map(|artifact| &artifact.value)
            else {
                anyhow::bail!("candidate review needs a selected candidate");
            };
            ensure!(
                request.checkout_id.as_ref() == Some(&candidate.checkout_id)
                    && request.task_id.as_ref() == Some(&candidate.target.task_id),
                "candidate review cannot switch its task or registered source checkout"
            );
        }
        self.submit_workflow_pipeline_agent_job(
            dispatch,
            scope,
            request,
            input,
            checklist_execution,
            WorkflowJobInput {
                unit_id,
                artifacts,
                plan_revision_request,
                comparison,
            },
        )
        .await
    }

    pub async fn pinned_workflow_node(
        &self,
        project: String,
        run: String,
        node: String,
    ) -> Result<Option<PinnedNode>> {
        self.execute(move |db| load_pinned_node(db, &project, &run, &node))
            .await
    }

    async fn workflow_profile_of_attempt(
        &self,
        project: &str,
        attempt: &str,
    ) -> Result<SessionScope> {
        let (project, attempt) = (project.to_owned(), attempt.to_owned());
        self.execute(move |db| {
            let payload: Vec<u8> = db.query_row(
                "SELECT j.operator_scope FROM job_attempts a JOIN jobs j ON j.id = a.job_id
                 JOIN workflow_job_inputs i ON i.job_id = j.id WHERE j.project_id = ?1 AND a.run_id = ?2",
                params![project, attempt], |row| row.get(0),
            ).optional()?.context("the workflow attempt's selected profile is unavailable")?;
            Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?)
        }).await
    }

    pub async fn workflow_attempt(
        &self,
        project: String,
        attempt: String,
    ) -> Result<Option<NodeAttempt>> {
        self.execute(move |db| load_node_attempt(db, &project, &attempt)).await
    }
}

pub(crate) fn load_node_attempt(
    db: &Connection,
    project: &str,
    attempt: &str,
) -> Result<Option<NodeAttempt>> {
    let mut statement = db.prepare(
        "SELECT o.pipeline_run_id, w.workflow_id, w.workflow_revision, o.node_instance_id,
                n.instance_revision, n.definition_revision, o.plan_revision, o.release_id,
                a.job_id, a.previous_run_id, a.ordinal, r.status, i.unit_id
         FROM job_attempts a JOIN runs r ON r.id = a.run_id
         JOIN checkouts c ON c.id = r.checkout_id
         JOIN job_pipeline_origins o ON o.job_id = a.job_id
         JOIN workflow_run_pins w ON w.pipeline_run_id = o.pipeline_run_id
         JOIN workflow_node_pins n ON n.pipeline_run_id = o.pipeline_run_id
            AND n.node_instance_id = o.node_instance_id
         LEFT JOIN workflow_job_inputs i ON i.job_id = a.job_id
         WHERE w.project_id = ?1 AND c.project_id = ?1 AND a.run_id = ?2",
    )?;
    let mut rows = statement.query(params![project, attempt])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(NodeAttempt {
        pin: NodePin {
            workflow_run_id: row.get(0)?,
            workflow: WorkflowRevision {
                id: row.get(1)?,
                revision: row.get(2)?,
            },
            node_instance_id: row.get(3)?,
            instance_revision: row.get(4)?,
            definition_revision: row.get(5)?,
            plan_revision: row.get(6)?,
            release_id: row.get(7)?,
            unit_id: row.get(12)?,
        },
        job_id: row.get(8)?,
        run_id: attempt.into(),
        previous_run_id: row.get(9)?,
        ordinal: row.get(10)?,
        status: RunStatus::from_sql(&row.get::<_, String>(11)?)?,
    }))
}

pub(super) fn load_pinned_node(
    db: &Connection,
    project: &str,
    run: &str,
    node: &str,
) -> Result<Option<PinnedNode>> {
    let pin = db
        .query_row(
            "SELECT p.workflow_id, p.workflow_revision, n.instance_revision
         FROM workflow_run_pins p JOIN workflow_node_pins n ON n.pipeline_run_id = p.pipeline_run_id
         WHERE p.project_id = ?1 AND p.pipeline_run_id = ?2 AND n.node_instance_id = ?3",
            params![project, run, node],
            |row| {
                Ok((
                    WorkflowRevision {
                        id: row.get(0)?,
                        revision: row.get(1)?,
                    },
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((version, instance_revision)) = pin else {
        return Ok(None);
    };
    let workflow = load_definition(db, project, &version)?
        .context("protected workflow revision disappeared")?;
    Ok(Some(
        workflow
            .nodes
            .into_iter()
            .find(|saved| saved.instance.id == node && saved.revision == instance_revision)
            .context("protected node revision does not match its run pin")?,
    ))
}

pub(crate) fn load_definition(
    db: &Connection,
    project: &str,
    version: &WorkflowRevision,
) -> Result<Option<WorkflowDefinition>> {
    let bytes: Option<Vec<u8>> = db.query_row(
        "SELECT definition FROM workflow_revisions WHERE project_id = ?1 AND workflow_id = ?2 AND revision = ?3",
        params![project, version.id, version.revision], |row| row.get(0),
    ).optional()?;
    bytes
        .map(|payload| Ok(serde_json::from_slice(&protection::unprotect(&payload)?)?))
        .transpose()
}
