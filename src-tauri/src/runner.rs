use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::MaybeUninit,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use windows_sys::Win32::System::SystemInformation::GetNativeSystemInfo;

use crate::{
    process::{self, TerminalOutcome, WindowsProcessControl},
    process_io::{CapturedProcess, capture},
    runs::RunRecord,
};

pub const CONTRACT_VERSION: u16 = 1;

pub struct LaunchRequest<'a> {
    pub version: u16,
    pub executable: &'a Path,
    pub checkout: &'a Path,
    pub arguments: &'a [&'a OsStr],
}

pub struct AttachmentRunScope<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub checkout_id: &'a str,
    pub checkout: &'a Path,
}

impl<'a> AttachmentRunScope<'a> {
    pub(crate) fn for_attempt(run: &'a RunRecord, checkout: &'a Path) -> Self {
        Self {
            project_id: &run.project_id,
            run_id: &run.id,
            checkout_id: &run.checkout_id,
            checkout,
        }
    }
}

pub struct CheckoutAttachment<'a> {
    pub relative_path: &'a Path,
    pub size: u64,
    pub sha256: &'a str,
}

pub struct StagedAttachmentFile<'a> {
    pub name: &'a str,
    pub size: u64,
    pub sha256: &'a str,
}

/// A private directory for one attempt. A runner must verify the size and digest
/// before making the file readable, and keep it read-only for the lease's lifetime.
pub trait AttachmentStaging: Send {
    fn directory(&self) -> &Path;
    fn stage_read_only(
        &mut self,
        file: StagedAttachmentFile<'_>,
        bytes: &mut dyn io::Read,
    ) -> io::Result<()>;
}

pub trait RunControl: Send + Sync {
    fn cancel(&self) -> io::Result<bool>;
    fn wait(&self) -> io::Result<TerminalOutcome>;
    fn confirm_quiescence(&self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the runner cannot confirm that all writers have stopped",
        ))
    }
}

pub trait Runner: Send + Sync {
    fn identity(&self) -> &str;
    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        None
    }
    fn executable_revision(&self, _path: &Path) -> Option<String> {
        None
    }
    fn attachment_path(&self, _staged: &Path) -> Option<PathBuf> {
        None
    }
    /// Resolves the verified reference in the destination checkout. A runner
    /// with its own checkout must verify the size and digest there.
    fn attachment_reference(
        &self,
        scope: &AttachmentRunScope<'_>,
        file: CheckoutAttachment<'_>,
    ) -> io::Result<PathBuf> {
        if self.attachment_path(scope.checkout).as_deref() == Some(scope.checkout) {
            return Ok(scope.checkout.join(file.relative_path));
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "runner cannot verify a reference in the selected checkout",
        ))
    }
    fn attachment_staging(
        &self,
        _scope: &AttachmentRunScope<'_>,
    ) -> io::Result<Box<dyn AttachmentStaging>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the runner has no read-only attachment staging",
        ))
    }
    fn launch_attempt(
        &self,
        _scope: &AttachmentRunScope<'_>,
        request: LaunchRequest<'_>,
    ) -> io::Result<CapturedProcess> {
        self.launch(request)
    }
    fn launch(&self, request: LaunchRequest<'_>) -> io::Result<CapturedProcess>;
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub revision: u32,
    pub native_architecture: u16,
    pub server: bool,
}

impl WindowsVersion {
    fn current() -> Option<Self> {
        let version = windows_version::OsVersion::current();
        if version.build == 0 {
            return None;
        }
        let mut info = MaybeUninit::uninit();
        // GetNativeSystemInfo initializes SYSTEM_INFO and the architecture field of its union.
        let native_architecture = unsafe {
            GetNativeSystemInfo(info.as_mut_ptr());
            info.assume_init()
                .Anonymous
                .Anonymous
                .wProcessorArchitecture
        };
        Some(Self {
            major: version.major,
            minor: version.minor,
            build: version.build,
            revision: windows_version::revision(),
            native_architecture,
            server: windows_version::is_server(),
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlatform {
    pub host: WindowsVersion,
    pub guest: Option<WindowsVersion>,
}

pub struct DirectWindowsRunner;

impl DirectWindowsRunner {
    pub const IDENTITY: &str = "windows-direct";
}

impl Runner for DirectWindowsRunner {
    fn identity(&self) -> &str {
        Self::IDENTITY
    }

    fn attachment_path(&self, staged: &Path) -> Option<PathBuf> {
        Some(staged.to_owned())
    }

    fn attachment_staging(
        &self,
        scope: &AttachmentRunScope<'_>,
    ) -> io::Result<Box<dyn AttachmentStaging>> {
        crate::attachments::local_attachment_staging(scope)
    }

    fn execution_platform(&self) -> Option<ExecutionPlatform> {
        Some(ExecutionPlatform {
            host: WindowsVersion::current()?,
            guest: None,
        })
    }

    fn executable_revision(&self, path: &Path) -> Option<String> {
        let mut executable = File::open(path).ok()?;
        if !executable.metadata().ok()?.is_file() {
            return None;
        }
        let mut hash = Sha256::new();
        io::copy(&mut executable, &mut hash).ok()?;
        Some(format!("{:x}", hash.finalize()))
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

    fn confirm_quiescence(&self) -> io::Result<()> {
        self.confirm_quiescence()
    }
}
