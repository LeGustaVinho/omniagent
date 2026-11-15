use std::{
    env,
    ffi::OsStr,
    io::{BufRead, BufReader},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
};

use omniagent::process::{TerminalOutcome, launch};
use omniagent::process_io::{Stream, capture};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

#[test]
fn cancelled_job_terminates_the_owned_grandchild() {
    let process = launch(
        Path::new(env!("CARGO_BIN_EXE_omniagent")),
        &env::current_dir().unwrap(),
        &[OsStr::new("--fixture"), OsStr::new("--grandchild")],
    )
    .unwrap();
    let mut stdout = BufReader::new(process.stdout).lines();
    let child_identity = stdout.next().unwrap().unwrap();
    let pid: u32 = child_identity
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
