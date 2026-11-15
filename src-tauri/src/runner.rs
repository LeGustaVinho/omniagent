use std::{ffi::OsStr, io, path::Path};

use crate::{
    process::{self, TerminalOutcome, WindowsProcessControl},
    process_io::{CapturedProcess, capture},
};

pub const CONTRACT_VERSION: u16 = 1;

pub struct LaunchRequest<'a> {
    pub version: u16,
    pub executable: &'a Path,
    pub checkout: &'a Path,
    pub arguments: &'a [&'a OsStr],
}

pub trait RunControl: Send + Sync {
    fn cancel(&self) -> io::Result<bool>;
    fn wait(&self) -> io::Result<TerminalOutcome>;
}

pub trait Runner: Send + Sync {
    fn identity(&self) -> &str;
    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess>;
}

pub struct DirectWindowsRunner;

impl Runner for DirectWindowsRunner {
    fn identity(&self) -> &str {
        "windows-direct"
    }

    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess> {
        if request.version != CONTRACT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported runner contract version",
            ));
        }
        capture(process::launch(
            request.executable,
            request.checkout,
            request.arguments,
        )?)
    }
}

impl RunControl for WindowsProcessControl {
    fn cancel(&self) -> io::Result<bool> {
        self.cancel()
    }

    fn wait(&self) -> io::Result<TerminalOutcome> {
        self.wait()
    }
}
