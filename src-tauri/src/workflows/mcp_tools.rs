use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use uuid::Uuid;

use super::{WorkflowArtifact, digest};
use crate::{
    protection,
    runner::{DirectWindowsRunner, Runner},
    store::Store,
};

mod jobs;
mod protocol;
pub use jobs::{McpAttemptEvidence, McpJobReview, WorkflowMcpToolSubmission};
pub(crate) use jobs::{
    is_mcp_job, load_mcp_artifact, mcp_admission_blocker, run_isolated_mcp, settle_mcp_restart,
    settle_mcp_result,
};

const MAX_DATA: usize = 192 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolConfiguration {
    pub server_id: String,
    pub server_revision: String,
    pub tool_name: String,
    pub contract_revision: String,
    pub arguments: Map<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub argument_mappings: BTreeMap<String, String>,
    pub destination_pointer: Option<String>,
}

impl McpToolConfiguration {
    pub(super) fn is_valid(&self) -> bool {
        Uuid::parse_str(&self.server_id).is_ok()
            && sha(&self.server_revision)
            && sha(&self.contract_revision)
            && !self.tool_name.is_empty()
            && self.tool_name.len() <= 128
            && serde_json::to_vec(&self.arguments).is_ok_and(|encoded| encoded.len() <= MAX_DATA)
            && self.argument_mappings.len() <= 64
            && self.argument_mappings.iter().all(|(name, pointer)| {
                !name.is_empty()
                    && name.len() <= 256
                    && !self.arguments.contains_key(name)
                    && valid_pointer(pointer)
            })
            && self
                .destination_pointer
                .as_ref()
                .is_none_or(|pointer| valid_pointer(pointer))
    }

    fn with_resolved_arguments(mut self, artifact: Option<&WorkflowArtifact>) -> Result<Self> {
        if !self.argument_mappings.is_empty() {
            let value = serde_json::to_value(
                &artifact
                    .context("The MCP argument mappings are waiting for their source artifact")?
                    .value,
            )?;
            for (name, pointer) in std::mem::take(&mut self.argument_mappings) {
                let mapped = value.pointer(&pointer).with_context(|| {
                    format!("The source artifact has no value for MCP argument {name}")
                })?;
                self.arguments.insert(name, mapped.clone());
            }
        }
        Ok(self)
    }
}

fn valid_pointer(pointer: &str) -> bool {
    pointer.starts_with('/')
        && pointer.len() <= 2048
        && pointer
            .split('~')
            .skip(1)
            .all(|part| part.starts_with(['0', '1']))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerRequest {
    pub name: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub outbound_network: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub outbound_network: bool,
    pub tools: Vec<McpToolContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolContract {
    pub name: String,
    pub revision: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}

impl McpToolContract {
    fn from_server(tool: &Value) -> Result<Self> {
        let name = tool["name"]
            .as_str()
            .filter(|name| !name.is_empty() && name.len() <= 128)
            .context("Invalid MCP tool name")?;
        let input_schema = tool
            .get("inputSchema")
            .filter(|schema| schema["type"] == "object")
            .context("The MCP input schema must describe an object")?;
        let output_schema = tool.get("outputSchema");
        ensure!(
            output_schema.is_none_or(|schema| schema["type"] == "object"),
            "The MCP output schema must describe an object"
        );
        // Compile both schemas when discovering a tool so unsupported schemas cannot bind.
        compile_schema(input_schema)?;
        if let Some(schema) = output_schema {
            compile_schema(schema)?;
        }
        let revision = digest(&serde_json::to_vec(&(name, input_schema, output_schema))?);
        Ok(Self {
            name: name.into(),
            revision,
            input_schema: input_schema.clone(),
            output_schema: output_schema.cloned(),
        })
    }

    fn validate_input(&self, arguments: &Map<String, Value>) -> Result<()> {
        ensure!(
            compile_schema(&self.input_schema)?.is_valid(&Value::Object(arguments.clone())),
            "Mapped arguments do not match the pinned MCP input schema"
        );
        Ok(())
    }

    fn validate_result(&self, result: Value) -> Result<McpToolOutput> {
        ensure!(result.is_object(), "Invalid MCP result envelope");
        let rejected = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        ensure!(
            result.get("isError").is_none_or(Value::is_boolean),
            "Invalid MCP result status"
        );
        let content = result["content"]
            .as_array()
            .context("The MCP result omitted its content blocks")?;
        ensure!(
            content.iter().all(|item| item["type"].is_string()),
            "Invalid MCP result content block"
        );
        let structured = result.get("structuredContent").cloned();
        if !rejected && let Some(schema) = &self.output_schema {
            let value = structured
                .as_ref()
                .context("The MCP result omitted declared structured output")?;
            ensure!(
                compile_schema(schema)?.is_valid(value),
                "MCP output differs from its pinned schema"
            );
        }
        Ok(McpToolOutput {
            rejected,
            structured,
            content: content.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolOutput {
    pub rejected: bool,
    pub structured: Option<Value>,
    pub content: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMcpToolResult {
    pub run_id: String,
    pub output: McpToolOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedServer {
    summary: McpServer,
    executable_revision: String,
}

impl Store {
    pub async fn register_workflow_mcp_server(
        &self,
        project: String,
        request: McpServerRequest,
    ) -> Result<McpServer> {
        let saved = tokio::task::spawn_blocking(move || discover(request)).await??;
        self.execute(move |db| {
            db.execute(
                "INSERT INTO workflow_mcp_servers VALUES (?1,?2,?3,?4)",
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

    pub async fn workflow_mcp_servers(&self, project: String) -> Result<Vec<McpServer>> {
        self.execute(move |db| {
            let mut query = db.prepare("SELECT payload FROM workflow_mcp_servers WHERE project_id = ?1 ORDER BY rowid DESC")?;
            let all = query.query_map([project], |row| row.get::<_, Vec<u8>>(0))?;
            all.map(|bytes| Ok(unseal::<SavedServer>(&bytes?)?.summary)).collect()
        }).await
    }
}

fn discover(request: McpServerRequest) -> Result<SavedServer> {
    let name = request.name.trim().to_owned();
    ensure!(
        !name.is_empty() && name.len() <= 200 && request.executable.is_absolute(),
        "Select a named local executable"
    );
    ensure!(
        request.arguments.len() <= 64
            && request.arguments.iter().all(|arg| !arg.contains('\0'))
            && request.arguments.iter().map(String::len).sum::<usize>() <= 8192,
        "Invalid MCP server arguments"
    );
    let executable = dunce::canonicalize(&request.executable)?;
    ensure!(
        executable
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe")),
        "Select a self-contained Windows executable"
    );
    let runner = DirectWindowsRunner;
    let executable_revision = runner
        .executable_revision(&executable)
        .context("Could not identify the local MCP executable")?;
    let mut saved = SavedServer {
        summary: McpServer {
            id: Uuid::new_v4().to_string(),
            revision: String::new(),
            name,
            executable,
            arguments: request.arguments,
            outbound_network: request.outbound_network,
            tools: Vec::new(),
        },
        executable_revision,
    };
    let tools = protocol::Client::connect(&saved, &runner, false, &|| false)?.tools(&|| false)?;
    saved.summary.tools = tools;
    saved.summary.revision = digest(&serde_json::to_vec(&saved)?);
    Ok(saved)
}

fn binding(
    db: &Connection,
    project: &str,
    config: &McpToolConfiguration,
) -> Result<(SavedServer, McpToolContract)> {
    ensure!(config.is_valid(), "The MCP tool binding is invalid");
    let payload: Vec<u8> = db.query_row("SELECT payload FROM workflow_mcp_servers WHERE project_id = ?1 AND id = ?2 AND revision = ?3",
        params![project, config.server_id, config.server_revision], |row| row.get(0)).optional()?.context("The pinned local MCP server is unavailable")?;
    let saved: SavedServer = unseal(&payload)?;
    let tool = saved
        .summary
        .tools
        .iter()
        .find(|tool| tool.name == config.tool_name && tool.revision == config.contract_revision)
        .context("The pinned MCP tool schema is unavailable")?
        .clone();
    if config.argument_mappings.is_empty() {
        tool.validate_input(&config.arguments)?;
        destination(config)?;
    }
    Ok((saved, tool))
}

pub(crate) fn validate_mcp_binding(
    db: &Connection,
    project: &str,
    config: &McpToolConfiguration,
) -> Result<()> {
    binding(db, project, config).map(|_| ())
}

fn destination(config: &McpToolConfiguration) -> Result<Option<String>> {
    config
        .destination_pointer
        .as_ref()
        .map(|pointer| {
            let arguments = Value::Object(config.arguments.clone());
            let destination = arguments
                .pointer(pointer)
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty() && text.len() <= 2048)
                .context(
                    "The destination pointer must resolve to an explicit, nonempty string argument",
                )?;
            Ok(destination.to_owned())
        })
        .transpose()
}

fn compile_schema(schema: &Value) -> Result<jsonschema::Validator> {
    ensure!(
        serde_json::to_vec(schema)?.len() <= MAX_DATA,
        "The MCP schema exceeds the size limit"
    );
    jsonschema::options().build(schema).map_err(|_| {
        anyhow::anyhow!("The MCP server declared an invalid or unsupported JSON Schema")
    })
}

fn sha(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn seal(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(protection::protect(&serde_json::to_vec(value)?)?)
}
fn unseal<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    Ok(serde_json::from_slice(&protection::unprotect(payload)?)?)
}
