use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    thread,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use omniagent::{
    diagnostics,
    jobs::{ExternalWork, JobScheduler},
    process::create_job,
    repository::{checkout_items::checkout_directory, checkout_root, verify_checkout},
    store::Store,
};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use tauri::{State, ipc::Channel};
use uuid::Uuid;
use windows_sys::Win32::System::{
    JobObjects::AssignProcessToJobObject, SystemInformation::GetSystemDirectoryW,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualTerminalInfo {
    terminal_id: String,
    checkout_id: String,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ManualTerminalEvent {
    Output { data: String },
    Exited { exit_code: u32 },
    Error { message: String },
}

#[derive(Default)]
pub struct ManualTerminals(Arc<Sessions>);

#[derive(Default)]
struct Sessions(Mutex<HashMap<String, Arc<ManualSession>>>);

impl Drop for Sessions {
    fn drop(&mut self) {
        for session in self
            .0
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .values()
        {
            session.stop();
        }
    }
}

struct ManualSession {
    checkout_id: String,
    _work: Option<ExternalWork>,
    job: Mutex<Option<OwnedHandle>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
}

impl ManualSession {
    fn stop(&self) {
        let job = self
            .job
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(job);
    }

    fn release_pty(&self) {
        let master = self
            .master
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        // Close ConPTY's input independently of a writer waiting on its pipe.
        drop(master);
        self.writer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn ensure_running(&self) -> Result<()> {
        ensure!(
            self.job.lock().expect("manual terminal job").is_some(),
            "PowerShell has closed"
        );
        Ok(())
    }

    fn write(&self, data: String) -> Result<()> {
        self.ensure_running()?;
        self.writer
            .lock()
            .expect("manual terminal writer")
            .as_mut()
            .context("PowerShell input has closed")?
            .write_all(data.as_bytes())
            .context("Send input to PowerShell")
    }

    fn resize(&self, size: PtySize) -> Result<()> {
        self.ensure_running()?;
        self.master
            .lock()
            .expect("manual terminal master")
            .as_ref()
            .context("PowerShell has closed")?
            .resize(size)
            .context("Resize PowerShell")
    }
}

impl ManualTerminals {
    async fn start(
        &self,
        store: &Store,
        checkout_id: String,
        relative_directory: Option<String>,
        size: PtySize,
        work: Option<ExternalWork>,
        send: impl Fn(ManualTerminalEvent) -> bool + Send + Sync + 'static,
    ) -> Result<ManualTerminalInfo> {
        let root = checkout_root(store, checkout_id.clone()).await?;
        let layout = verify_checkout(store, checkout_id.clone(), root).await?;
        let sessions = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let directory =
                checkout_directory(&layout.root, relative_directory.as_deref().unwrap_or(""))?;
            launch(&sessions, checkout_id, &directory, size, work, send)
        })
        .await?
    }

    fn find(&self, checkout_id: &str, terminal_id: &str) -> Result<Option<Arc<ManualSession>>> {
        let session = self
            .0
            .0
            .lock()
            .expect("manual terminals")
            .get(terminal_id)
            .cloned();
        ensure!(
            session
                .as_ref()
                .is_none_or(|entry| entry.checkout_id == checkout_id),
            "The terminal belongs to another checkout"
        );
        Ok(session)
    }

    fn close(&self, checkout_id: &str, terminal_id: &str) -> Result<()> {
        if let Some(session) = self.find(checkout_id, terminal_id)? {
            session.stop();
        }
        Ok(())
    }
}

fn launch(
    sessions: &Arc<Sessions>,
    checkout_id: String,
    root: &Path,
    size: PtySize,
    work: Option<ExternalWork>,
    send: impl Fn(ManualTerminalEvent) -> bool + Send + Sync + 'static,
) -> Result<ManualTerminalInfo> {
    ensure!(
        dunce::canonicalize(root)? == root,
        "The checkout directory changed before launch"
    );
    let mut command = CommandBuilder::new(windows_powershell()?);
    command.args(["-NoLogo", "-NoProfile"]);
    command.cwd(root);
    command.env("TERM", "xterm-256color");
    let job = create_job().context("Create the manual terminal process job")?;
    let pair = native_pty_system()
        .openpty(size)
        .context("Open Windows ConPTY")?;
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let mut child = pair
        .slave
        .spawn_command(command)
        .context("Start Windows PowerShell")?;
    if let Err(error) = assign_to_job(&job, child.as_ref()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    drop(pair.slave);

    let terminal_id = Uuid::new_v4().to_string();
    let session = Arc::new(ManualSession {
        checkout_id: checkout_id.clone(),
        _work: work,
        job: Mutex::new(Some(job)),
        master: Mutex::new(Some(pair.master)),
        writer: Mutex::new(Some(writer)),
    });
    let send = Arc::new(send);
    let output = thread::Builder::new()
        .name("manual-terminal-output".into())
        .spawn({
            let send = send.clone();
            let session = session.clone();
            move || read_output(reader, &session, &*send)
        });
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            session.stop();
            let _ = child.wait();
            return Err(error).context("Start the PowerShell output reader");
        }
    };
    sessions
        .0
        .lock()
        .expect("manual terminals")
        .insert(terminal_id.clone(), session.clone());
    let supervisor = thread::Builder::new()
        .name("manual-terminal-exit".into())
        .spawn({
            let session = session.clone();
            let registry = Arc::downgrade(sessions);
            let terminal_id = terminal_id.clone();
            move || finish_terminal(child, output, session, registry, terminal_id, &*send)
        });
    if let Err(error) = supervisor {
        session.stop();
        session.release_pty();
        sessions
            .0
            .lock()
            .expect("manual terminals")
            .remove(&terminal_id);
        return Err(error).context("Start the PowerShell exit watcher");
    }
    Ok(ManualTerminalInfo {
        terminal_id,
        checkout_id,
    })
}

fn assign_to_job(job: &OwnedHandle, child: &dyn Child) -> Result<()> {
    // The application supplies the command and disables profiles. Input is exposed only after assignment.
    let handle = child
        .as_raw_handle()
        .context("PowerShell did not provide a process handle")?;
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), handle) } == 0 {
        return Err(io::Error::last_os_error())
            .context("Attach PowerShell to its manual terminal job");
    }
    Ok(())
}

fn read_output(
    mut reader: Box<dyn Read + Send>,
    session: &ManualSession,
    send: &impl Fn(ManualTerminalEvent) -> bool,
) -> io::Result<()> {
    let mut bytes = [0; 8192];
    let mut connected = true;
    let result = loop {
        match reader.read(&mut bytes) {
            Ok(0) => break Ok(()),
            Ok(size) => {
                if connected
                    && !send(ManualTerminalEvent::Output {
                        data: STANDARD.encode(&bytes[..size]),
                    })
                {
                    connected = false;
                    session.stop();
                }
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => break Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => break Err(error),
        }
    };
    session.stop();
    result
}

fn finish_terminal(
    mut child: Box<dyn Child + Send + Sync>,
    output: thread::JoinHandle<io::Result<()>>,
    session: Arc<ManualSession>,
    registry: Weak<Sessions>,
    terminal_id: String,
    send: &impl Fn(ManualTerminalEvent) -> bool,
) {
    let exit = child.wait();
    session.stop();
    session.release_pty();
    let output = output.join().ok().and_then(Result::err);
    if let Some(registry) = registry.upgrade() {
        registry
            .0
            .lock()
            .expect("manual terminals")
            .remove(&terminal_id);
    }
    if let Some(error) = output {
        send(ManualTerminalEvent::Error {
            message: report(error),
        });
    }
    match exit {
        Ok(status) => send(ManualTerminalEvent::Exited {
            exit_code: status.exit_code(),
        }),
        Err(error) => send(ManualTerminalEvent::Error {
            message: report(error),
        }),
    };
}

fn windows_powershell() -> Result<PathBuf> {
    let mut buffer = [0; 260];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    ensure!(
        length != 0 && length < buffer.len(),
        "The Windows system directory is unavailable"
    );
    let executable = PathBuf::from(OsString::from_wide(&buffer[..length]))
        .join(r"WindowsPowerShell\v1.0\powershell.exe");
    ensure!(executable.is_file(), "Windows PowerShell is unavailable");
    Ok(executable)
}

fn terminal_size(cols: u16, rows: u16) -> Result<PtySize> {
    ensure!(
        cols > 0 && rows > 0 && i16::try_from(cols).is_ok() && i16::try_from(rows).is_ok(),
        "PowerShell terminal dimensions are out of range"
    );
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

fn report(error: impl std::fmt::Display) -> String {
    diagnostics::preview(&error.to_string())
}

#[tauri::command]
pub async fn start_manual_terminal(
    store: State<'_, Store>,
    terminals: State<'_, ManualTerminals>,
    scheduler: State<'_, JobScheduler>,
    checkout_id: String,
    relative_directory: Option<String>,
    cols: u16,
    rows: u16,
    channel: Channel<ManualTerminalEvent>,
) -> Result<ManualTerminalInfo, String> {
    let size = terminal_size(cols, rows).map_err(report)?;
    let work = scheduler.admit_external_work().await.map_err(report)?;
    terminals
        .start(
            &store,
            checkout_id,
            relative_directory,
            size,
            Some(work),
            move |event| channel.send(event).is_ok(),
        )
        .await
        .map_err(report)
}

#[tauri::command]
pub async fn write_manual_terminal(
    terminals: State<'_, ManualTerminals>,
    checkout_id: String,
    terminal_id: String,
    data: String,
) -> Result<(), String> {
    let session = terminals
        .find(&checkout_id, &terminal_id)
        .map_err(report)?
        .ok_or_else(|| "PowerShell has closed".to_owned())?;
    tokio::task::spawn_blocking(move || session.write(data))
        .await
        .map_err(report)?
        .map_err(report)
}

#[tauri::command]
pub async fn resize_manual_terminal(
    terminals: State<'_, ManualTerminals>,
    checkout_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let size = terminal_size(cols, rows).map_err(report)?;
    let Some(session) = terminals.find(&checkout_id, &terminal_id).map_err(report)? else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || session.resize(size))
        .await
        .map_err(report)?
        .map_err(report)
}

#[tauri::command]
pub fn close_manual_terminal(
    terminals: State<'_, ManualTerminals>,
    checkout_id: String,
    terminal_id: String,
) -> Result<(), String> {
    terminals.close(&checkout_id, &terminal_id).map_err(report)
}
