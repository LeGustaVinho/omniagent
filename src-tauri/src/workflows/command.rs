use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    jobs::{
        CommandExchange, Job, PinnedCommandScript, PipelineDispatch, PreparedCommand,
        queue_pipeline_command,
    },
    protection,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
};

use super::{
    ArtifactValue, BuiltinNode, NodeConfiguration, NodeInputs, WorkflowArtifact, WorkflowJobInput,
    digest, runtime::load_pinned_node,
};

#[cfg(test)]
mod tests;

const ARGUMENT_LIMIT: usize = 16 * 1024;
const INPUT_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutputFormat {
    Json,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", content = "value", rename_all = "snake_case")]
pub enum CommandData {
    Json(Value),
    Text(String),
}

impl CommandData {
    pub(crate) fn parse(format: CommandOutputFormat, stdout: &str) -> Result<Self> {
        match format {
            CommandOutputFormat::Json => Ok(Self::Json(
                serde_json::from_str(stdout)
                    .context("Command stdout is not one complete JSON value")?,
            )),
            CommandOutputFormat::Text => Ok(Self::Text(stdout.into())),
        }
    }

    fn format(&self) -> CommandOutputFormat {
        match self {
            Self::Json(_) => CommandOutputFormat::Json,
            Self::Text(_) => CommandOutputFormat::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandConfiguration {
    pub registration_id: String,
    pub registration_revision: String,
    pub arguments: Vec<String>,
    pub input: Map<String, Value>,
    pub output: CommandOutputFormat,
}

impl CommandConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        !self.registration_id.trim().is_empty()
            && self.registration_revision.len() == 64
            && self
                .registration_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            && valid_arguments(&self.arguments)
            && serde_json::to_vec(&self.input).is_ok_and(|bytes| bytes.len() <= INPUT_LIMIT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CommandTarget {
    Executable {
        path: PathBuf,
    },
    Script {
        executable: PathBuf,
        path: PathBuf,
        interpreter_arguments: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandRegistrationRequest {
    pub name: String,
    pub target: CommandTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRegistration {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub target: CommandTarget,
}

#[derive(Deserialize, Serialize)]
struct SavedRegistration {
    summary: CommandRegistration,
    executable_revision: String,
    script_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowCommandSubmission {
    pub project_id: String,
    pub run_id: String,
    pub node_id: String,
    pub inputs: NodeInputs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandResult {
    pub run_id: String,
    pub output: CommandData,
}

impl Store {
    pub async fn register_workflow_command(
        &self,
        project: String,
        request: CommandRegistrationRequest,
    ) -> Result<CommandRegistration> {
        let saved = tokio::task::spawn_blocking(move || prepare_registration(request)).await??;
        self.execute(move |db| {
            db.execute(
                "INSERT INTO workflow_command_registrations VALUES (?1, ?2, ?3, ?4)",
                params![
                    project,
                    saved.summary.id,
                    saved.summary.revision,
                    seal(&saved)?
                ],
            )?;
            Ok(saved.summary)
        })
        .await
    }

    pub async fn list_workflow_command_registrations(
        &self,
        project: String,
    ) -> Result<Vec<CommandRegistration>> {
        self.execute(move |db| {
            let mut query = db.prepare(
                "SELECT payload FROM workflow_command_registrations WHERE project_id = ?1",
            )?;
            let mut rows = query.query([project])?;
            let mut values = Vec::new();
            while let Some(row) = rows.next()? {
                let saved: SavedRegistration = unseal(&row.get::<_, Vec<u8>>(0)?)?;
                values.push(saved.summary);
            }
            values.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
            Ok(values)
        })
        .await
    }

    pub async fn submit_workflow_command(
        &self,
        submission: WorkflowCommandSubmission,
    ) -> Result<Job> {
        let WorkflowCommandSubmission {
            project_id,
            run_id,
            node_id,
            inputs,
        } = submission;
        let (node, bound) = self
            .workflow_execution_inputs(&project_id, &run_id, &node_id, None, &inputs)
            .await?;
        let NodeConfiguration::Command(configuration) = node.instance.configuration else {
            anyhow::bail!("The pinned node is not a Command");
        };
        let ArtifactValue::Release(approval) = &bound["release"].value else {
            anyhow::bail!("A Command requires a prepared implementation release");
        };
        let ArtifactValue::Worktree { checkout_id, task } = &bound["worktree"].value else {
            anyhow::bail!("A Command requires the prepared managed worktree");
        };
        ensure!(
            !approval.release.revoked
                && approval.workflow_run_id == run_id
                && approval.release.checkout_id == *checkout_id
                && approval.release.task_id == task.task_id
                && task.project_id == project_id,
            "The command release and worktree belong to different implementation scopes"
        );
        let binding = PipelineDispatch {
            pipeline_run_id: run_id,
            node_instance_id: node_id,
            plan_revision: Some(approval.release.plan_revision.clone()),
            release_id: Some(approval.release.id.clone()),
        };
        let checkout = checkout_id.clone();
        let task = task.task_id.clone();
        let job = self
            .execute(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let registered = load_registration(&tx, &project_id, &configuration)?;
                let prepared = registered.invocation(configuration)?;
                let input = WorkflowJobInput {
                    unit_id: None,
                    artifacts: inputs,
                    plan_revision_request: None,
                };
                let job = queue_pipeline_command(
                    &tx,
                    &binding,
                    checkout,
                    task,
                    Vec::new(),
                    prepared,
                    Some(&input),
                )?;
                tx.commit()?;
                Ok(job)
            })
            .await?;
        self.job_signal.send_replace(());
        Ok(job)
    }

    pub async fn workflow_command_result(
        &self,
        project: String,
        run_id: String,
    ) -> Result<Option<WorkflowArtifact>> {
        self.execute(move |db| {
            let payload: Option<Vec<u8>> = db
                .query_row(
                    "SELECT c.payload FROM workflow_command_results c
                 JOIN workflow_run_pins w ON w.pipeline_run_id = c.pipeline_run_id
                 WHERE w.project_id = ?1 AND c.run_id = ?2",
                    params![project, run_id],
                    |row| row.get(0),
                )
                .optional()?;
            payload.map(|bytes| unseal(&bytes)).transpose()
        })
        .await
    }
}

fn valid_arguments(arguments: &[String]) -> bool {
    arguments.len() <= 128
        && arguments.iter().all(|arg| !arg.contains('\0'))
        && arguments
            .iter()
            .map(|arg| arg.encode_utf16().count() + 1)
            .sum::<usize>()
            <= ARGUMENT_LIMIT
}

fn identify_file(path: &Path) -> Result<(PathBuf, String)> {
    ensure!(path.is_absolute(), "Select an absolute local file path");
    let path = dunce::canonicalize(path).context("The local file is unavailable")?;
    let revision = DirectWindowsRunner
        .executable_revision(&path)
        .context("The local file could not be identified")?;
    Ok((path, revision))
}

fn prepare_registration(request: CommandRegistrationRequest) -> Result<SavedRegistration> {
    let name = request.name.trim().to_owned();
    ensure!(
        !name.is_empty() && name.len() <= 200,
        "Enter a command name up to 200 bytes"
    );
    let (target, executable_revision, script_revision) = match request.target {
        CommandTarget::Executable { path } => {
            let (path, revision) = identify_file(&path)?;
            (CommandTarget::Executable { path }, revision, None)
        }
        CommandTarget::Script {
            executable,
            path,
            interpreter_arguments,
        } => {
            ensure!(
                valid_arguments(&interpreter_arguments),
                "Interpreter arguments exceed the command limits"
            );
            let (executable, executable_revision) = identify_file(&executable)?;
            let (path, script_revision) = identify_file(&path)?;
            (
                CommandTarget::Script {
                    executable,
                    path,
                    interpreter_arguments,
                },
                executable_revision,
                Some(script_revision),
            )
        }
    };
    let revision = digest(&serde_json::to_vec(&(
        &target,
        &executable_revision,
        &script_revision,
    ))?);
    let summary = CommandRegistration {
        id: Uuid::new_v4().to_string(),
        revision,
        name,
        target,
    };
    Ok(SavedRegistration {
        summary,
        executable_revision,
        script_revision,
    })
}

fn load_registration(
    db: &Connection,
    project: &str,
    config: &CommandConfiguration,
) -> Result<SavedRegistration> {
    let payload: Vec<u8> = db.query_row(
        "SELECT payload FROM workflow_command_registrations WHERE project_id = ?1 AND id = ?2 AND revision = ?3",
        params![project, config.registration_id, config.registration_revision], |row| row.get(0),
    ).optional()?.context("The Command's local registration revision is unavailable in this project")?;
    unseal(&payload)
}

pub(super) fn validate_command_binding(
    db: &Connection,
    project: &str,
    configuration: &CommandConfiguration,
) -> Result<()> {
    load_registration(db, project, configuration)?.invocation(configuration.clone())?;
    Ok(())
}

impl SavedRegistration {
    fn invocation(self, configuration: CommandConfiguration) -> Result<PreparedCommand> {
        ensure!(
            configuration.is_valid(),
            "The pinned command configuration is invalid"
        );
        let (executable, mut arguments, script) = match self.summary.target {
            CommandTarget::Executable { path } => (path, Vec::new(), None),
            CommandTarget::Script {
                executable,
                path,
                mut interpreter_arguments,
            } => {
                interpreter_arguments
                    .push(path.to_str().context("Script path is not Unicode")?.into());
                let script = PinnedCommandScript {
                    path,
                    revision: self
                        .script_revision
                        .context("The script lost its registered revision")?,
                };
                (executable, interpreter_arguments, Some(script))
            }
        };
        arguments.extend(configuration.arguments);
        ensure!(
            valid_arguments(&arguments),
            "Combined command arguments exceed the command limits"
        );
        Ok(PreparedCommand {
            executable,
            arguments,
            revision: self.executable_revision,
            script,
            exchange: Some(CommandExchange {
                format: configuration.output,
                input: serde_json::to_vec(&configuration.input)?,
            }),
        })
    }
}

pub(crate) fn publish_command_result(
    db: &Connection,
    attempt: &str,
    output: CommandData,
) -> Result<()> {
    let (project, workflow, node): (String, String, String) = db
        .query_row(
            "SELECT p.project_id, p.pipeline_run_id, p.node_instance_id
         FROM job_attempts a JOIN job_pipeline_origins p ON p.job_id = a.job_id
         JOIN workflow_job_inputs i ON i.job_id = a.job_id
         JOIN runs r ON r.id = a.run_id AND r.kind = 'command' AND r.status = 'succeeded'
         WHERE a.run_id = ?1",
            [attempt],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("Command result lost its verified workflow attempt")?;
    let pinned = load_pinned_node(db, &project, &workflow, &node)?
        .context("Command result lost its pinned node")?;
    ensure!(
        pinned.definition.kind == BuiltinNode::Command
            && matches!(pinned.instance.configuration, NodeConfiguration::Command(config) if config.output == output.format()),
        "The command result differs from the declared output format"
    );
    let result = WorkflowCommandResult {
        run_id: attempt.into(),
        output,
    };
    let artifact = WorkflowArtifact {
        id: digest(&serde_json::to_vec(&(&workflow, &node, &result))?),
        workflow_run_id: workflow,
        node_instance_id: node,
        port: "result".into(),
        unit_id: None,
        value: ArtifactValue::CommandResult(result),
    };
    db.execute(
        "INSERT INTO workflow_command_results VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            artifact.id,
            artifact.workflow_run_id,
            artifact.node_instance_id,
            attempt,
            seal(&artifact)?
        ],
    )?;
    Ok(())
}

pub(super) fn load_command_artifact(
    db: &Connection,
    project: &str,
    workflow: &str,
    artifact_id: &str,
) -> Result<Option<WorkflowArtifact>> {
    let payload: Option<Vec<u8>> = db
        .query_row(
            "SELECT c.payload FROM workflow_command_results c
         JOIN workflow_run_pins w ON w.pipeline_run_id = c.pipeline_run_id
         WHERE w.project_id = ?1 AND c.pipeline_run_id = ?2 AND c.artifact_id = ?3",
            params![project, workflow, artifact_id],
            |row| row.get(0),
        )
        .optional()?;
    payload.map(|bytes| unseal(&bytes)).transpose()
}

fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}

fn unseal<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(bytes)?)?)
}
