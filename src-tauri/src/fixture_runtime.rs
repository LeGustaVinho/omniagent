use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use omniagent::{
    process::{self, TerminalOutcome, WindowsProcessControl},
    process_io::{ProcessInput, Stream, capture},
    repository,
    runs::{OutputStream, RunEventData, RunRecord, RunStatus},
    store::Store,
};
use serde::Deserialize;
use tauri::{AppHandle, State};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone, Default)]
pub struct FixtureRuns(Arc<Mutex<HashMap<String, ActiveRun>>>);

#[derive(Clone)]
struct ActiveRun {
    checkout_id: String,
    control: Arc<WindowsProcessControl>,
    input: Arc<Mutex<Option<ProcessInput>>>,
}

impl FixtureRuns {
    fn active(&self, checkout_id: &str, run_id: &str) -> Result<ActiveRun> {
        self.0
            .lock()
            .expect("active run lock")
            .get(run_id)
            .filter(|run| run.checkout_id == checkout_id)
            .cloned()
            .context("fixture run is no longer active")
    }

    pub fn stop_all(&self) {
        for run in self.0.lock().expect("active run lock").values() {
            let _ = run.control.cancel();
        }
    }

    async fn launch(&self, app: AppHandle, store: Store, checkout_id: String) -> Result<RunRecord> {
        let cwd = repository::checkout_root(&store, checkout_id.clone()).await?;
        let executable = env::current_exe()?;
        let (mut run, _) = store.start_run(checkout_id.clone()).await?;
        let process = tokio::task::spawn_blocking(move || {
            capture(process::launch(
                &executable,
                &cwd,
                &[OsStr::new("--fixture")],
            )?)
        })
        .await?;
        let process = match process {
            Ok(process) => process,
            Err(error) => {
                store
                    .record_run_event(
                        run.id.clone(),
                        RunEventData::Output {
                            stream: OutputStream::Stderr,
                            preview: format!("Cannot launch fixture: {error}"),
                        },
                    )
                    .await?;
                record_status(&store, &run.id, RunStatus::Failed).await?;
                return Err(error.into());
            }
        };
        record_status(&store, &run.id, RunStatus::Running).await?;
        run.status = RunStatus::Running;
        self.0.lock().expect("active run lock").insert(
            run.id.clone(),
            ActiveRun {
                checkout_id,
                control: process.control.clone(),
                input: Arc::new(Mutex::new(Some(process.input))),
            },
        );
        let active = self.0.clone();
        let run_id = run.id.clone();
        tauri::async_runtime::spawn(async move {
            let wait = tokio::task::spawn_blocking({
                let control = process.control.clone();
                move || control.wait()
            });
            let runtime = tokio::runtime::Handle::current();
            let read = tokio::task::spawn_blocking({
                let store = store.clone();
                let run_id = run_id.clone();
                move || {
                    for frame in process.output {
                        let frame = frame?;
                        runtime.block_on(record_output(
                            &store,
                            &run_id,
                            frame.stream,
                            frame.data,
                        ))?;
                    }
                    Ok::<_, anyhow::Error>(())
                }
            })
            .await;
            let output_error = read
                .map_err(anyhow::Error::from)
                .and_then(|result| result)
                .err();
            if let Some(error) = &output_error {
                let _ = process.control.cancel();
                let _ = store
                    .record_run_event(
                        run_id.clone(),
                        RunEventData::Output {
                            stream: OutputStream::Stderr,
                            preview: error.to_string(),
                        },
                    )
                    .await;
            }
            let status = match wait.await {
                Ok(Ok(TerminalOutcome::Exited(0))) if output_error.is_none() => {
                    RunStatus::Succeeded
                }
                Ok(Ok(TerminalOutcome::Cancelled(_))) => RunStatus::Cancelled,
                _ => RunStatus::Failed,
            };
            active.lock().expect("active run lock").remove(&run_id);
            if record_status(&store, &run_id, status).await.is_ok() {
                let body = match status {
                    RunStatus::Succeeded => "The fixture run completed.",
                    RunStatus::Cancelled => "The fixture run was cancelled.",
                    _ => "The fixture run failed.",
                };
                let _ = app
                    .notification()
                    .builder()
                    .title("OmniAgent")
                    .body(body)
                    .show();
            }
        });
        Ok(run)
    }
}

#[derive(Deserialize)]
struct FixtureMessage {
    #[serde(rename = "type")]
    kind: String,
}

async fn record_output(store: &Store, run_id: &str, stream: Stream, preview: String) -> Result<()> {
    let ready = stream == Stream::Stdout
        && serde_json::from_str::<FixtureMessage>(&preview)
            .is_ok_and(|event| event.kind == "ready");
    let stream = match stream {
        Stream::Stdout => OutputStream::Stdout,
        Stream::Stderr => OutputStream::Stderr,
    };
    store
        .record_run_event(run_id.to_owned(), RunEventData::Output { stream, preview })
        .await?;
    if ready {
        record_status(store, run_id, RunStatus::WaitingForInput).await?;
    }
    Ok(())
}

async fn record_status(store: &Store, run_id: &str, status: RunStatus) -> Result<()> {
    store
        .record_run_event(run_id.to_owned(), RunEventData::Status { status })
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn start_fixture(
    app: AppHandle,
    store: State<'_, Store>,
    runs: State<'_, FixtureRuns>,
    checkout_id: String,
) -> Result<RunRecord, String> {
    runs.launch(app, store.inner().clone(), checkout_id)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn send_fixture_input(
    store: State<'_, Store>,
    runs: State<'_, FixtureRuns>,
    checkout_id: String,
    run_id: String,
    input: String,
) -> Result<(), String> {
    let send = async {
        ensure!(
            !input.contains(['\r', '\n']),
            "fixture input must be one line"
        );
        let active = runs.active(&checkout_id, &run_id)?;
        let input = format!("{input}\n");
        store
            .prepare_fixture_input(run_id.clone(), input.clone())
            .await?;
        let mut pipe = active
            .input
            .lock()
            .expect("fixture stdin lock")
            .take()
            .context("fixture input was already sent")?;
        tokio::task::spawn_blocking(move || pipe.send(input.as_bytes())).await??;
        store.mark_fixture_input_sent(run_id).await
    };
    send.await.map_err(|error: anyhow::Error| error.to_string())
}

#[tauri::command]
pub fn cancel_fixture(
    runs: State<'_, FixtureRuns>,
    checkout_id: String,
    run_id: String,
) -> Result<bool, String> {
    runs.active(&checkout_id, &run_id)
        .and_then(|run| Ok(run.control.cancel()?))
        .map_err(|error| error.to_string())
}
