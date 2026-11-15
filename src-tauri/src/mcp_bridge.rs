mod security;

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
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

use crate::{protection, runs, store::Store};

pub const HELPER_FLAG: &str = "--mcp-bridge";
const PIPE_PREFIX: &str = r"\\.\pipe\omniagent-run-";
const INSPECT_SCOPE: &str = "inspect_run_scope";
const VERSION: u8 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunScope {
    run_id: String,
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
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    InspectScope,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    credential: Credential,
    operation: Operation,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "scope", rename_all = "snake_case")]
enum Response {
    Scope(RunScope),
    Unavailable,
}

/// Owns an endpoint and its ticket for the lifetime of one managed attempt.
pub struct RunBridge {
    task: JoinHandle<()>,
    ticket_path: PathBuf,
    _directory: TempDir,
}

impl RunBridge {
    pub async fn start(store: &Store, run_id: String) -> Result<Self> {
        let scope = active_scope(store, run_id)
            .await?
            .context("cannot bind an MCP bridge to an inactive run")?;
        let ticket = Ticket {
            version: VERSION,
            pipe: format!("{PIPE_PREFIX}{}", Uuid::new_v4()),
            credential: *Uuid::new_v4().as_bytes(),
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
        let task = tokio::spawn(serve_pipe(
            endpoint,
            store.clone(),
            scope,
            ticket.credential,
        ));
        Ok(Self {
            task,
            ticket_path,
            _directory: directory,
        })
    }

    pub fn ticket_path(&self) -> &Path {
        &self.ticket_path
    }
}

impl Drop for RunBridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn active_scope(store: &Store, run_id: String) -> Result<Option<RunScope>> {
    store
        .execute(move |db| {
            let active: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM active_agent_attempts WHERE run_id = ?1)",
                [&run_id],
                |row| row.get(0),
            )?;
            if !active {
                return Ok(None);
            }
            let run = runs::load_run(db, &run_id)?;
            Ok((!run.status.is_terminal()).then_some(RunScope {
                run_id: run.id,
                project_id: run.project_id,
                checkout_id: run.checkout_id,
                turn_id: run.turn_id,
            }))
        })
        .await
}

async fn serve_pipe(
    mut endpoint: NamedPipeServer,
    store: Store,
    scope: RunScope,
    credential: Credential,
) {
    while endpoint.connect().await.is_ok() {
        let _ = timeout(DEADLINE, async {
            let request: Request = read_frame(&mut endpoint).await?;
            let allowed =
                request.version == VERSION && bool::from(request.credential.ct_eq(&credential));
            let response = match request.operation {
                Operation::InspectScope
                    if allowed
                        && active_scope(&store, scope.run_id.clone()).await?
                            == Some(scope.clone()) =>
                {
                    Response::Scope(scope.clone())
                }
                Operation::InspectScope => Response::Unavailable,
            };
            write_frame(&mut endpoint, &response).await?;
            // DisconnectNamedPipe discards unread bytes. Wait for the client to receive the reply.
            endpoint.read_u8().await?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if endpoint.disconnect().is_err() {
            break;
        }
    }
}

async fn inspect(ticket: &Ticket) -> Result<RunScope> {
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
                operation: Operation::InspectScope,
            },
        )
        .await?;
        let response: Response = read_frame(&mut pipe).await?;
        pipe.write_u8(1).await?;
        match response {
            Response::Scope(scope) => Ok(scope),
            Response::Unavailable => anyhow::bail!("MCP bridge scope unavailable"),
        }
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
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            INSPECT_SCOPE,
            "Returns the project, checkout, run, and turn bound to this OmniAgent connection.",
            json!({"type":"object", "additionalProperties":false})
                .as_object()
                .unwrap()
                .clone(),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name != INSPECT_SCOPE
            || request
                .arguments
                .is_some_and(|arguments| !arguments.is_empty())
        {
            return Err(ErrorData::invalid_params(
                "Unknown tool or unexpected arguments",
                None,
            ));
        }
        let scope = inspect(&self.0)
            .await
            .map_err(|_| ErrorData::internal_error("Run bridge is unavailable", None))?;
        Ok(CallToolResult::structured(json!(scope)).into())
    }
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
