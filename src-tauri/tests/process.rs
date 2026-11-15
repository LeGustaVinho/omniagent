use std::{
    env,
    ffi::OsStr,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    time::{Duration, Instant},
};

use omniagent::process::{TerminalOutcome, launch};
use omniagent::process_io::{Stream, capture};
use omniagent::runner::{CONTRACT_VERSION, DirectWindowsRunner, LaunchRequest, Runner};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

#[test]
fn noisy_job_streams_and_cancel_terminates_its_owned_grandchild() {
    let process = launch(
        Path::new(env!("CARGO_BIN_EXE_omniagent")),
        &env::current_dir().unwrap(),
        &[
            OsStr::new("--fixture"),
            OsStr::new("--grandchild"),
            OsStr::new("--stderr-flood"),
        ],
    )
    .unwrap();
    let process = capture(process).unwrap();
    let mut pid_line = None;
    let mut ready = false;
    let mut stderr_bytes = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(ready && pid_line.is_some() && stderr_bytes >= 64 * 1_024) {
        let frame = process
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("stdout and stderr continue before input")
            .unwrap();
        match frame.stream {
            Stream::Stderr => stderr_bytes += frame.data.len(),
            Stream::Stdout if frame.data.contains(r#""type":"ready""#) => ready = true,
            Stream::Stdout => pid_line = Some(frame.data),
        }
    }
    let child_identity = pid_line.unwrap();
    let pid: u32 = child_identity
        .trim_end()
        .strip_prefix(r#"{"type":"grandchild","pid":"#)
        .unwrap()
        .strip_suffix('}')
        .unwrap()
        .parse()
        .unwrap();
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!raw.is_null());
    let grandchild = unsafe { OwnedHandle::from_raw_handle(raw) };

    assert!(process.control.cancel().unwrap());
    assert!(!process.control.cancel().unwrap());
    assert!(matches!(
        process.control.wait().unwrap(),
        TerminalOutcome::Cancelled(_)
    ));
    assert_eq!(
        unsafe { WaitForSingleObject(grandchild.as_raw_handle(), 3_000) },
        WAIT_OBJECT_0
    );
}

#[test]
fn the_supervisor_rejects_relative_executables() {
    let error = launch(
        Path::new("omniagent.exe"),
        &env::current_dir().unwrap(),
        &[],
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn a_runner_rejects_an_unknown_contract_version_before_launch() {
    let runner: &dyn Runner = &DirectWindowsRunner;
    assert_eq!(runner.identity(), "windows-direct");
    let failure = runner
        .launch(LaunchRequest {
            version: CONTRACT_VERSION + 1,
            executable: Path::new(env!("CARGO_BIN_EXE_omniagent")),
            checkout: &env::current_dir().unwrap(),
            arguments: &[OsStr::new("--fixture")],
        })
        .err()
        .unwrap();
    assert_eq!(failure.kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn supervised_pipes_drain_concurrently_and_keep_utf8_frames() {
    let process = launch(
        Path::new(env!("CARGO_BIN_EXE_omniagent")),
        &env::current_dir().unwrap(),
        &[OsStr::new("--fixture"), OsStr::new("--stderr-flood")],
    )
    .unwrap();
    let mut process = capture(process).unwrap();
    let mut stdout = Vec::new();
    let mut stderr_bytes = 0;
    for frame in process.output {
        let frame = frame.unwrap();
        match frame.stream {
            Stream::Stdout => {
                if stdout.is_empty() {
                    assert_eq!(
                        frame.data.trim_end(),
                        r#"{"type":"ready","message":"Pronto ☕"}"#
                    );
                    process.input.send(b"literal %PATH% & | ^\n").unwrap();
                }
                stdout.push(frame.data);
            }
            Stream::Stderr => stderr_bytes += frame.data.len(),
        }
    }
    assert_eq!(stdout.len(), 3);
    assert_eq!(stdout[1].trim_end(), r#"{"type":"input","bytes":21}"#);
    assert_eq!(stderr_bytes, 1_049_600);
    assert_eq!(process.control.wait().unwrap(), TerminalOutcome::Exited(0));
}
