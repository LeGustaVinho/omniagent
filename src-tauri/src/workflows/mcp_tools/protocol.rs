use std::{
    ffi::OsStr,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{McpToolContract, SavedServer};
use crate::{
    process_io::{CapturedProcess, OutputFrame, Stream},
    runner::{CONTRACT_VERSION, LaunchRequest, RunControl, Runner},
};

const FRAME_LIMIT: usize = 256 * 1024;
const MAX_TOOLS: usize = 128;
const PROTOCOL: &str = "2025-06-18";

pub(super) struct Client {
    control: Arc<dyn RunControl>,
    stdin: mpsc::SyncSender<Vec<u8>>,
    stdout: mpsc::Receiver<std::io::Result<OutputFrame>>,
    request: u64,
}

impl Client {
    pub(super) fn connect(
        server: &SavedServer,
        runner: &dyn Runner,
        outbound_network: bool,
        stopping: &impl Fn() -> bool,
    ) -> Result<Self> {
        let args = server
            .summary
            .arguments
            .iter()
            .map(OsStr::new)
            .collect::<Vec<_>>();
        let executable = &server.summary.executable;
        let CapturedProcess {
            control,
            input,
            output,
        } = runner
            .launch_isolated_tool(
                LaunchRequest {
                    version: CONTRACT_VERSION,
                    executable,
                    checkout: executable
                        .parent()
                        .context("Missing executable directory")?,
                    arguments: &args,
                },
                &server.executable_revision,
                outbound_network,
            )
            .context("The runner could not establish isolated MCP execution")?;
        let (stdin, receiver) = mpsc::sync_channel::<Vec<u8>>(2);
        let mut client = Self {
            control,
            stdin,
            stdout: output,
            request: 0,
        };
        thread::Builder::new()
            .name("mcp-stdin".into())
            .spawn(move || {
                let mut input = input;
                while let Ok(frame) = receiver.recv() {
                    if input.send(&frame).is_err() {
                        break;
                    }
                }
            })?;
        let initialized = client.request(
            "initialize",
            json!({"protocolVersion":PROTOCOL,"capabilities":{},
            "clientInfo":{"name":"OmniAgent","version":env!("CARGO_PKG_VERSION")}}),
            Duration::from_secs(8),
            stopping,
        )?;
        ensure!(
            matches!(
                initialized["protocolVersion"].as_str(),
                Some("2024-11-05" | "2025-03-26" | PROTOCOL | "2025-11-25")
            ) && initialized
                .pointer("/capabilities/tools")
                .is_some_and(Value::is_object),
            "The server did not negotiate a supported MCP tool protocol"
        );
        client.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
        Ok(client)
    }

    pub(super) fn tools(&mut self, stopping: &impl Fn() -> bool) -> Result<Vec<McpToolContract>> {
        let mut tools = Vec::new();
        let mut cursor = None::<String>;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            let request = cursor
                .as_deref()
                .map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let listed = self.request("tools/list", request, Duration::from_secs(5), stopping)?;
            let page = listed["tools"]
                .as_array()
                .context("The server omitted its MCP tools")?;
            ensure!(
                tools.len() + page.len() <= MAX_TOOLS,
                "The MCP tool inventory exceeds the discovery limit"
            );
            for tool in page {
                let tool = McpToolContract::from_server(tool)?;
                ensure!(
                    seen.insert(tool.name.clone()),
                    "Duplicate tool name in MCP inventory"
                );
                tools.push(tool);
            }
            cursor = listed["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        anyhow::bail!("The MCP server did not finish its tool inventory")
    }

    pub(super) fn call(
        &mut self,
        tool: &str,
        arguments: &serde_json::Map<String, Value>,
        stopping: &impl Fn() -> bool,
    ) -> Result<Value> {
        self.request(
            "tools/call",
            json!({"name":tool,"arguments":arguments}),
            Duration::from_secs(30),
            stopping,
        )
    }

    fn send(&self, frame: Value) -> Result<()> {
        let mut encoded = serde_json::to_vec(&frame)?;
        ensure!(
            encoded.len() < FRAME_LIMIT,
            "The MCP request exceeds the frame limit"
        );
        encoded.push(b'\n');
        self.stdin
            .try_send(encoded)
            .context("MCP stdio is unavailable")
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
        stopping: &impl Fn() -> bool,
    ) -> Result<Value> {
        ensure!(!stopping(), "The MCP operation was interrupted");
        self.request += 1;
        let id = self.request;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + timeout;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            ensure!(!stopping(), "The MCP operation was interrupted");
            let frame = match self
                .stdout
                .recv_timeout(remaining.min(Duration::from_millis(100)))
            {
                Ok(frame) => frame?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("The MCP server closed before confirming its response")
                }
            };
            if frame.stream == Stream::Stderr {
                continue;
            }
            ensure!(
                frame.data.len() <= FRAME_LIMIT,
                "The MCP response exceeds the frame limit"
            );
            let frame: Value =
                serde_json::from_str(&frame.data).context("The MCP server sent invalid JSON")?;
            ensure!(
                frame["jsonrpc"] == "2.0",
                "The MCP server sent an invalid protocol envelope"
            );
            if frame.get("id").is_none() && frame["method"].is_string() {
                continue;
            }
            ensure!(
                frame["id"].as_u64() == Some(id),
                "The MCP response has the wrong request identity"
            );
            ensure!(
                frame.get("error").is_none(),
                "The MCP server rejected {method}"
            );
            return frame
                .get("result")
                .cloned()
                .context("The MCP response has no result");
        }
        anyhow::bail!("The MCP server did not answer {method} before its deadline")
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.control.cancel();
        let _ = self.control.confirm_quiescence();
    }
}
