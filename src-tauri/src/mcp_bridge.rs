mod security;

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer},
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

use crate::{
    diagnostics,
    dispatch::{Delivery, EventSource},
    jobs::{DownstreamWaitState, JobRequest, JobRequestConflict},
    protection,
    providers::{ApprovalDecision, ProviderEvent, ProviderEventData, ProviderLocation, RequestId},
    runs,
    runtime_inspection::RuntimeInspection,
    store::Store,
};

pub const HELPER_FLAG: &str = "--mcp-bridge";
pub(crate) const SERVER_NAME: &str = "omniagent_runtime";
const PIPE_PREFIX: &str = r"\\.\pipe\omniagent-run-";
pub(crate) const INSPECT_SCOPE: &str = "inspect_run_scope";
pub(crate) const INSPECT_OMNIAGENT: &str = "inspect_omniagent";
pub(crate) const SUBMIT_JOB: &str = "submit_job";
pub(crate) const GET_JOB_STATUS: &str = "get_job_status";
pub(crate) const WAIT_FOR_JOBS: &str = "wait_for_jobs";
pub(crate) const REQUEST_PERMISSION: &str = "request_permission";
pub(crate) const PERMISSION_PREFIX: &str = "omniagent-permission:";
pub(crate) const VERSION: u8 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(3);
const PERMISSION_DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunScope {
    pub(crate) run_id: String,
    project_id: String,
    checkout_id: String,
    turn_id: String,
}

type Credential = [u8; 16];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    version: u8,
    pipe: String,
    credential: Credential,
    #[serde(default)]
    permission_prompt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PermissionPrompt {
    tool_name: String,
    input: serde_json::Map<String, Value>,
    tool_use_id: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitJob {
    request_id: String,
    request: JobRequest,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobStatusRequest {
    job_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitForJobsRequest {
    request_id: String,
    child_job_ids: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    ListTools,
    InspectScope,
    InspectOmniagent,
    SubmitJob(SubmitJob),
    GetJobStatus(JobStatusRequest),
    WaitForJobs(WaitForJobsRequest),
    RequestPermission(PermissionPrompt),
    PollPermission { id: String },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    credential: Credential,
    operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
enum Response {
    Scope(RunScope),
    Inspection(Box<RuntimeInspection>),
    JobSubmitted {
        job_id: String,
    },
    JobStatus(Value),
    JobWaiting {
        wait_id: String,
        state: DownstreamWaitState,
    },
    JobRejected,
    JobRequestConflict,
    PermissionRequested(String),
    PermissionPending,
    PermissionDecided(ApprovalDecision),
    Unavailable,
}

/// Owns an endpoint and its ticket for the lifetime of one managed attempt.
pub struct RunBridge {
    task: JoinHandle<()>,
    ticket_path: PathBuf,
    observed: Arc<AtomicU8>,
    permission_prompt: bool,
    _directory: TempDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Registration {
    Configured,
    ToolsListed,
    ScopeVerified,
    Unavailable { reason: String },
}

#[derive(Clone, Serialize)]
pub struct BridgeLaunch {
    pub command: String,
    pub args: [String; 2],
    #[serde(skip)]
    pub(crate) permission_prompt: bool,
}

impl BridgeLaunch {
    pub(crate) fn acp_servers(&self) -> Value {
        json!([{"name": SERVER_NAME, "command": self.command, "args": self.args, "env": []}])
    }
}

impl RunBridge {
    pub async fn start(store: &Store, run_id: String) -> Result<Self> {
        Self::start_for_run(store, run_id, false).await
    }

    pub(crate) async fn start_for_run(
        store: &Store,
        run_id: String,
        permission_prompt: bool,
    ) -> Result<Self> {
        let scope = active_scope(store, run_id)
            .await?
            .context("cannot bind an MCP bridge to an inactive run")?;
        let ticket = Ticket {
            version: VERSION,
            pipe: format!("{PIPE_PREFIX}{}", Uuid::new_v4()),
            credential: *Uuid::new_v4().as_bytes(),
            permission_prompt,
        };
        let endpoint = security::current_user_pipe(&ticket.pipe)?;
        let directory = tempfile::Builder::new()
            .prefix("omniagent-bridge-")
            .tempdir()?;
        let ticket_path = directory.path().join("ticket.dpapi");
        fs::write(
            &ticket_path,
            protection::protect(&serde_json::to_vec(&ticket)?)?,
        )?;
        let observed = Arc::new(AtomicU8::new(0));
        let task = tokio::spawn(serve_pipe(
            endpoint,
            store.clone(),
            scope,
            ticket,
            observed.clone(),
        ));
        Ok(Self {
            task,
            ticket_path,
            observed,
            permission_prompt,
            _directory: directory,
        })
    }

    pub fn ticket_path(&self) -> &Path {
        &self.ticket_path
    }

    pub(crate) fn launch(&self, executable: &Path) -> Result<BridgeLaunch> {
        ensure!(
            executable.is_absolute() && executable.is_file(),
            "MCP helper executable is unavailable"
        );
        Ok(BridgeLaunch {
            command: executable
                .to_str()
                .context("MCP helper path is not Unicode")?
                .into(),
            args: [
                HELPER_FLAG.into(),
                self.ticket_path
                    .to_str()
                    .context("MCP ticket path is not Unicode")?
                    .into(),
            ],
            permission_prompt: self.permission_prompt,
        })
    }

    pub(crate) fn registration(&self) -> Registration {
        if self.task.is_finished() {
            return Registration::Unavailable {
                reason: "The local bridge stopped".into(),
            };
        }
        match self.observed.load(Ordering::Acquire) {
            0 => Registration::Configured,
            1 => Registration::ToolsListed,
            _ => Registration::ScopeVerified,
        }
    }
}

impl Drop for RunBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn active_scope(store: &Store, run_id: String) -> Result<Option<RunScope>> {
    store
        .execute(move |db| read_active_scope(db, &run_id))
        .await
}

pub(crate) fn read_active_scope(db: &Connection, run_id: &str) -> Result<Option<RunScope>> {
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE run_id = ?1)",
        [run_id],
        |row| row.get(0),
    )?;
    if !active {
        return Ok(None);
    }
    let run = runs::load_run(db, run_id)?;
    Ok((!run.status.is_terminal()).then_some(RunScope {
        run_id: run.id,
        project_id: run.project_id,
        checkout_id: run.checkout_id,
        turn_id: run.turn_id,
    }))
}

async fn serve_pipe(
    mut endpoint: NamedPipeServer,
    store: Store,
    scope: RunScope,
    ticket: Ticket,
    observed: Arc<AtomicU8>,
) {
    while endpoint.connect().await.is_ok() {
        let _ = timeout(DEADLINE, async {
            let request: Request = read_frame(&mut endpoint).await?;
            let allowed = request.version == VERSION
                && bool::from(request.credential.ct_eq(&ticket.credential));
            let (response, decision) = if allowed
                && active_scope(&store, scope.run_id.clone()).await? == Some(scope.clone())
            {
                match request.operation {
                    Operation::ListTools => {
                        observed.fetch_max(1, Ordering::Release);
                        (Response::Scope(scope.clone()), None)
                    }
                    Operation::InspectScope => {
                        observed.fetch_max(2, Ordering::Release);
                        (Response::Scope(scope.clone()), None)
                    }
                    Operation::InspectOmniagent => {
                        // Leave room for the containing response tag and fields.
                        match store
                            .inspect_omniagent(scope.clone(), MAX_FRAME_BYTES - 128)
                            .await
                        {
                            Ok(inspection) => {
                                observed.fetch_max(2, Ordering::Release);
                                (Response::Inspection(Box::new(inspection)), None)
                            }
                            Err(_) => (Response::Unavailable, None),
                        }
                    }
                    Operation::SubmitJob(input) => {
                        let response = match store
                            .submit_agent_job(scope.run_id.clone(), input.request_id, input.request)
                            .await
                        {
                            Ok(job) => Response::JobSubmitted { job_id: job.id },
                            Err(error) if error.is::<JobRequestConflict>() => {
                                Response::JobRequestConflict
                            }
                            Err(_) => Response::JobRejected,
                        };
                        (response, None)
                    }
                    Operation::GetJobStatus(input) => {
                        let response = match store
                            .active_run_child_job(scope.run_id.clone(), input.job_id)
                            .await?
                        {
                            Some(mut job) => {
                                let wait = job.downstream_wait.take();
                                let mut status = serde_json::to_value(job)?;
                                if let Some(wait) = wait {
                                    status["downstreamWait"] = json!({
                                        "id": wait.id, "state": wait.state, "selectedJobs": wait.inputs.len(),
                                        "pendingJobs": wait.inputs.iter().filter(|input| input.status.is_none()).count(),
                                    });
                                }
                                Response::JobStatus(status)
                            }
                            None => Response::JobRejected,
                        };
                        (response, None)
                    }
                    Operation::WaitForJobs(input) => {
                        let response = match store.request_downstream_wait(
                            scope.run_id.clone(), input.request_id, input.child_job_ids,
                        ).await {
                            Ok(wait) => Response::JobWaiting { wait_id: wait.id, state: wait.state },
                            Err(_) => Response::JobRejected,
                        };
                        (response, None)
                    }
                    Operation::RequestPermission(prompt) if ticket.permission_prompt => {
                        (record_permission(&store, &scope, prompt).await?, None)
                    }
                    Operation::PollPermission { id } if ticket.permission_prompt => match store
                        .pending_claude_permission(
                            scope.run_id.clone(),
                            scope.turn_id.clone(),
                            RequestId::Text(id),
                        )
                        .await?
                    {
                        Some((seq, Some(decision))) => {
                            (Response::PermissionDecided(decision), Some(seq))
                        }
                        Some((_, None)) => (Response::PermissionPending, None),
                        None => (Response::Unavailable, None),
                    },
                    _ => (Response::Unavailable, None),
                }
            } else {
                (Response::Unavailable, None)
            };
            write_frame(&mut endpoint, &response).await?;
            // DisconnectNamedPipe discards unread bytes. Wait for the client to receive the reply.
            if endpoint.read_u8().await? == 1
                && let Some(seq) = decision
            {
                store
                    .settle_permission_decision(seq, Delivery::Submitted)
                    .await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if endpoint.disconnect().is_err() {
            break;
        }
    }
}

async fn record_permission(
    store: &Store,
    scope: &RunScope,
    prompt: PermissionPrompt,
) -> Result<Response> {
    ensure!(
        !prompt.tool_name.trim().is_empty() && prompt.tool_name.len() <= 256,
        "invalid permission tool name"
    );
    let id = format!("{PERMISSION_PREFIX}{}", Uuid::new_v4());
    store
        .record_agent_observation(
            scope.run_id.clone(),
            EventSource::Provider,
            ProviderEvent {
                location: ProviderLocation {
                    item_id: prompt.tool_use_id.clone(),
                    ..Default::default()
                },
                data: ProviderEventData::Approval {
                    request_id: RequestId::Text(id.clone()),
                    operation: diagnostics::preview(&prompt.tool_name),
                    description: None,
                    prompt: Some(prompt),
                },
            },
        )
        .await?;
    Ok(Response::PermissionRequested(id))
}

async fn read_scope(ticket: &Ticket, operation: Operation) -> Result<RunScope> {
    match exchange(ticket, operation).await? {
        Response::Scope(scope) => Ok(scope),
        _ => anyhow::bail!("MCP bridge scope unavailable"),
    }
}

async fn exchange(ticket: &Ticket, operation: Operation) -> Result<Response> {
    timeout(DEADLINE, async {
        let mut pipe = loop {
            match ClientOptions::new().open(&ticket.pipe) {
                Ok(pipe) => break pipe,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    sleep(Duration::from_millis(15)).await;
                }
                Err(error) => return Err(error.into()),
            }
        };
        write_frame(
            &mut pipe,
            &Request {
                version: ticket.version,
                credential: ticket.credential,
                operation,
            },
        )
        .await?;
        let response: Response = read_frame(&mut pipe).await?;
        pipe.write_u8(1).await?;
        Ok(response)
    })
    .await?
}

async fn read_frame<T: DeserializeOwned>(io: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let length = io.read_u32().await? as usize;
    ensure!(
        length <= MAX_FRAME_BYTES,
        "MCP bridge frame exceeds size limit"
    );
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn write_frame(io: &mut (impl AsyncWrite + Unpin), frame: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    ensure!(
        bytes.len() <= MAX_FRAME_BYTES,
        "MCP bridge frame exceeds size limit"
    );
    let mut message = Vec::with_capacity(4 + bytes.len());
    message.extend((bytes.len() as u32).to_be_bytes());
    message.extend(bytes);
    io.write_all(&message).await?;
    Ok(())
}

fn read_ticket(path: &Path) -> Result<Ticket> {
    let mut encrypted = Vec::new();
    File::open(path)?
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut encrypted)?;
    ensure!(
        encrypted.len() <= MAX_FRAME_BYTES,
        "MCP bridge ticket exceeds size limit"
    );
    let ticket: Ticket = serde_json::from_slice(&protection::unprotect(&encrypted)?)?;
    ensure!(
        ticket.version == VERSION,
        "unsupported MCP bridge ticket version"
    );
    ensure!(
        ticket
            .pipe
            .strip_prefix(PIPE_PREFIX)
            .is_some_and(|name| Uuid::parse_str(name).is_ok()),
        "invalid MCP bridge pipe name"
    );
    Ok(ticket)
}

async fn permission_decision(
    ticket: &Ticket,
    prompt: PermissionPrompt,
) -> Result<ApprovalDecision> {
    let Response::PermissionRequested(id) =
        exchange(ticket, Operation::RequestPermission(prompt)).await?
    else {
        anyhow::bail!("MCP bridge rejected permission request");
    };
    timeout(PERMISSION_DEADLINE, async {
        loop {
            match exchange(ticket, Operation::PollPermission { id: id.clone() }).await? {
                Response::PermissionDecided(decision) => return Ok(decision),
                Response::PermissionPending => sleep(Duration::from_millis(150)).await,
                _ => anyhow::bail!("MCP permission request is no longer active"),
            }
        }
    })
    .await?
}

struct StdioBridge(Ticket);

impl ServerHandler for StdioBridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("omniagent", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        read_scope(&self.0, Operation::ListTools)
            .await
            .map_err(|_| bridge_unavailable())?;
        let mut tools = vec![
            Tool::new(
                INSPECT_SCOPE,
                "Returns the project, checkout, run, and turn bound to this OmniAgent connection.",
                json!({"type":"object", "properties":{}, "additionalProperties":false})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .with_annotations(
                ToolAnnotations::new()
                    .read_only(true)
                    .destructive(false)
                    .idempotent(true)
                    .open_world(false),
            ),
            Tool::new(
                INSPECT_OMNIAGENT,
                "Returns the running OmniAgent build, effective profile and policy, verified capability status, and visible task, queue and redacted diagnostics for this run.",
                json!({"type":"object", "properties":{}, "additionalProperties":false})
                    .as_object().unwrap().clone(),
            ).with_annotations(ToolAnnotations::new().read_only(true).destructive(false).idempotent(true).open_world(false)),
            Tool::new(
                SUBMIT_JOB,
                "Queues work in OmniAgent under this run and returns its job ID. Use a stable requestId when retrying the same request.",
                json!({"type":"object", "required":["requestId","request"], "additionalProperties":false, "properties":{
                    "requestId":{"type":"string","minLength":1,"maxLength":256},
                    "request":{"type":"object", "required":["mode","instructions"], "additionalProperties":false, "properties":{
                        "checkoutId":{"type":"string","description":"Checkout ID in this project. Omit to inherit the origin checkout."},
                        "taskId":{"type":"string","description":"Task ID in this project. Required for repository writes unless inherited from the parent job."},
                        "mode":{"enum":["read_only","plan","approved_write"]},
                        "instructions":{"type":"string","minLength":1},
                        "dependencies":{"type":"array","items":{"type":"string"}},
                        "requiresBridge":{"type":"boolean","description":"Whether the job requires the OmniAgent MCP tools to run."}
                    }}
                }}).as_object().unwrap().clone(),
            ).with_annotations(ToolAnnotations::new().read_only(false).destructive(false).idempotent(true).open_world(false)),
            Tool::new(
                GET_JOB_STATUS,
                "Returns the current queue state and latest attempt for a job submitted by this run.",
                json!({"type":"object", "required":["jobId"], "additionalProperties":false,
                    "properties":{"jobId":{"type":"string"}}}).as_object().unwrap().clone(),
            ).with_annotations(ToolAnnotations::new().read_only(true).destructive(false).idempotent(true).open_world(false)),
            Tool::new(
                WAIT_FOR_JOBS,
                "Park this scheduled attempt until the selected direct child jobs settle. Finish the current turn after this request. A linked attempt will receive the selected results.",
                json!({"type":"object", "required":["requestId","childJobIds"], "additionalProperties":false,
                    "properties":{"requestId":{"type":"string","minLength":1,"maxLength":256},
                        "childJobIds":{"type":"array","minItems":1,"items":{"type":"string"}}}})
                    .as_object().unwrap().clone(),
            ).with_annotations(ToolAnnotations::new().read_only(false).destructive(false).idempotent(true).open_world(false)),
        ];
        if self.0.permission_prompt {
            tools.push(Tool::new(
                REQUEST_PERMISSION,
                "Asks OmniAgent's user to decide whether to permit a Claude Code tool call.",
                json!({"type":"object", "properties": {
                    "tool_name": {"type":"string"}, "input": {"type":"object"}, "tool_use_id": {"type":"string"},
                }, "required": ["tool_name", "input"]}).as_object().unwrap().clone(),
            ));
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if matches!(
            request.name.as_ref(),
            SUBMIT_JOB | GET_JOB_STATUS | WAIT_FOR_JOBS
        ) {
            let arguments = Value::Object(request.arguments.unwrap_or_default());
            let invalid = |_| ErrorData::invalid_params("Invalid job tool arguments", None);
            let operation = match request.name.as_ref() {
                SUBMIT_JOB => {
                    Operation::SubmitJob(serde_json::from_value(arguments).map_err(invalid)?)
                }
                WAIT_FOR_JOBS => {
                    Operation::WaitForJobs(serde_json::from_value(arguments).map_err(invalid)?)
                }
                _ => Operation::GetJobStatus(serde_json::from_value(arguments).map_err(invalid)?),
            };
            let result = match exchange(&self.0, operation)
                .await
                .map_err(|_| bridge_unavailable())?
            {
                Response::JobSubmitted { job_id } => json!({"jobId": job_id}),
                Response::JobStatus(job) => job,
                Response::JobWaiting { wait_id, state } => {
                    json!({"waitId": wait_id, "state": state,
                    "next": "Finish the current turn. OmniAgent will continue it with the selected results."})
                }
                Response::JobRequestConflict => {
                    return Err(ErrorData::invalid_params(
                        "This run already used that request ID for a different job",
                        None,
                    ));
                }
                Response::JobRejected => {
                    return Err(ErrorData::invalid_params(
                        if request.name == WAIT_FOR_JOBS {
                            "The selected jobs cannot be awaited from this run"
                        } else {
                            "The job or its requested scope is unavailable to this run"
                        },
                        None,
                    ));
                }
                _ => return Err(bridge_unavailable()),
            };
            return Ok(CallToolResult::structured(result).into());
        }
        if request.name == REQUEST_PERMISSION && self.0.permission_prompt {
            let prompt =
                serde_json::from_value(Value::Object(request.arguments.unwrap_or_default()))
                    .map_err(|_| {
                        ErrorData::invalid_params("Invalid Claude permission request", None)
                    })?;
            let response = match permission_decision(&self.0, prompt).await {
                Ok(ApprovalDecision::AllowOnce) => json!({"behavior":"allow"}),
                Ok(ApprovalDecision::Deny) => {
                    json!({"behavior":"deny", "message":"The user declined this permission."})
                }
                Ok(ApprovalDecision::Cancel) => {
                    json!({"behavior":"deny", "message":"The user cancelled this permission.", "interrupt": true})
                }
                Err(_) => {
                    json!({"behavior":"deny", "message":"The OmniAgent permission request is unavailable."})
                }
            };
            return Ok(
                CallToolResult::success(vec![ContentBlock::text(response.to_string())]).into(),
            );
        }
        if !matches!(request.name.as_ref(), INSPECT_SCOPE | INSPECT_OMNIAGENT)
            || request
                .arguments
                .is_some_and(|arguments| !arguments.is_empty())
        {
            return Err(ErrorData::invalid_params(
                "Unknown tool or unexpected arguments",
                None,
            ));
        }
        if request.name == INSPECT_OMNIAGENT {
            let response = exchange(&self.0, Operation::InspectOmniagent)
                .await
                .map_err(|_| bridge_unavailable())?;
            let Response::Inspection(inspection) = response else {
                return Err(bridge_unavailable());
            };
            return Ok(CallToolResult::structured(json!(inspection)).into());
        }
        let scope = read_scope(&self.0, Operation::InspectScope)
            .await
            .map_err(|_| bridge_unavailable())?;
        Ok(CallToolResult::structured(json!(scope)).into())
    }
}

fn bridge_unavailable() -> ErrorData {
    ErrorData::internal_error("Run bridge is unavailable", None)
}

pub fn run_from_args() -> Option<i32> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(HELPER_FLAG.as_ref()) {
        return None;
    }
    let result = (|| -> Result<()> {
        let ticket = arguments.next().context("missing ticket path")?;
        ensure!(
            arguments.next().is_none(),
            "unexpected MCP bridge arguments"
        );
        let ticket = read_ticket(Path::new(&ticket))?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                StdioBridge(ticket)
                    .serve(rmcp::transport::stdio())
                    .await?
                    .waiting()
                    .await?;
                Ok(())
            })
    })();
    if result.is_ok() {
        Some(0)
    } else {
        eprintln!("OmniAgent MCP bridge could not serve stdio");
        Some(1)
    }
}
