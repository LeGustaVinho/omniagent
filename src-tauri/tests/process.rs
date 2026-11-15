use std::{
    env,
    ffi::OsStr,
    io::{BufRead, BufReader},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
};

use omniagent::process::{TerminalOutcome, launch};
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
