use std::{
    ffi::{OsStr, OsString, c_void},
    fs, io,
    path::Path,
    ptr::{null, null_mut},
    sync::Arc,
};

use tempfile::TempDir;
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, LocalFree},
    Security::{
        ACL,
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT,
            SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, FreeSid,
        Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
        NO_INHERITANCE, PSID, SECURITY_CAPABILITIES, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
        WinCapabilityInternetClientSid,
    },
    Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ},
    System::{
        Threading::{
            CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
        },
        WindowsProgramming::{
            PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
            PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
        },
    },
};

use super::{ProcessAttributes, TerminalOutcome, launch_scoped, terminated};
use crate::{
    process_io::{CapturedProcess, capture},
    runner::{DirectWindowsRunner, RunControl, Runner},
};

pub(super) struct LaunchSecurity<'a> {
    capabilities: SECURITY_CAPABILITIES,
    package_policy: u32,
    child_policy: u32,
    environment: &'a [u16],
}

impl LaunchSecurity<'_> {
    pub(super) fn apply(&self, attributes: &mut ProcessAttributes) -> io::Result<()> {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            std::slice::from_ref(&self.capabilities),
        )?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            std::slice::from_ref(&self.package_policy),
        )?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            std::slice::from_ref(&self.child_policy),
        )
    }

    pub(super) fn flags(&self) -> u32 {
        // The child uses inherited pipes, without initializing a Windows console.
        DETACHED_PROCESS | CREATE_UNICODE_ENVIRONMENT
    }
    pub(super) fn environment(&self) -> *const c_void {
        self.environment.as_ptr().cast()
    }
}

struct PackageProfile {
    name: Vec<u16>,
    directory: TempDir,
}

impl Drop for PackageProfile {
    fn drop(&mut self) {
        unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
    }
}

struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

struct PackageSid(PSID);
impl Drop for PackageSid {
    fn drop(&mut self) {
        unsafe { FreeSid(self.0) };
    }
}

struct IsolatedControl {
    child: Arc<dyn RunControl>,
    _profile: PackageProfile,
}
impl RunControl for IsolatedControl {
    fn cancel(&self) -> io::Result<bool> {
        self.child.cancel()
    }
    fn wait(&self) -> io::Result<TerminalOutcome> {
        self.child.wait()
    }
    fn confirm_quiescence(&self) -> io::Result<()> {
        self.child.confirm_quiescence()
    }
}

pub(crate) fn launch(
    executable: &Path,
    args: &[&OsStr],
    revision: &str,
    outbound_network: bool,
) -> io::Result<CapturedProcess> {
    let directory = tempfile::tempdir()?;
    let staged = directory.path().join("server.exe");
    fs::copy(executable, &staged)?;
    if DirectWindowsRunner.executable_revision(&staged).as_deref() != Some(revision) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The pinned MCP server executable changed",
        ));
    }
    let name: Vec<u16> = format!("OmniAgent.Tool.{}\0", uuid::Uuid::new_v4().simple())
        .encode_utf16()
        .collect();
    let mut sid = null_mut();
    let status = unsafe {
        CreateAppContainerProfile(
            name.as_ptr(),
            name.as_ptr(),
            name.as_ptr(),
            null(),
            0,
            &mut sid,
        )
    };
    if status < 0 || sid.is_null() {
        return Err(io::Error::other(format!(
            "AppContainer profile creation failed (0x{:08x})",
            status as u32
        )));
    }
    let profile = PackageProfile { name, directory };
    let sid = PackageSid(sid);
    allow_read_execute(profile.directory.path(), sid.0)?;
    allow_read_execute(&staged, sid.0)?;

    // Allocate aligned memory for the capability SID and keep it alive across CreateProcessW.
    let mut network_sid = [0_u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<u32>())];
    let mut network_sid_size = size_of_val(&network_sid) as u32;
    let mut capability = SID_AND_ATTRIBUTES::default();
    if outbound_network {
        super::checked(unsafe {
            CreateWellKnownSid(
                WinCapabilityInternetClientSid,
                null_mut(),
                network_sid.as_mut_ptr().cast(),
                &mut network_sid_size,
            )
        })?;
        capability.Sid = network_sid.as_mut_ptr().cast();
        capability.Attributes = 4; // SE_GROUP_ENABLED
    }
    let environment = platform_environment()?;
    let security = LaunchSecurity {
        capabilities: SECURITY_CAPABILITIES {
            AppContainerSid: sid.0,
            Capabilities: if outbound_network {
                &mut capability
            } else {
                null_mut()
            },
            CapabilityCount: u32::from(outbound_network),
            Reserved: 0,
        },
        package_policy: PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
        child_policy: PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
        environment: &environment,
    };
    let process = capture(launch_scoped(
        &staged,
        profile.directory.path(),
        args,
        Some(&security),
    )?)?;
    Ok(CapturedProcess {
        control: Arc::new(IsolatedControl {
            child: process.control,
            _profile: profile,
        }),
        input: process.input,
        output: process.output,
    })
}

fn platform_environment() -> io::Result<Vec<u16>> {
    // Windows needs these directories to start an AppContainer process. Other
    // entries, including credentials, stay in the parent environment.
    let mut block = Vec::new();
    for key in ["LOCALAPPDATA", "SYSTEMROOT"] {
        let value = std::env::var_os(key)
            .ok_or_else(|| io::Error::other(format!("Missing Windows directory {key}")))?;
        if !Path::new(&value).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid Windows directory",
            ));
        }
        let mut entry = OsString::from(key);
        entry.push("=");
        entry.push(value);
        block.extend(terminated(&entry)?);
    }
    block.push(0);
    Ok(block)
}

fn allow_read_execute(path: &Path, sid: PSID) -> io::Result<()> {
    let path = terminated(path.as_os_str())?;
    let mut old_acl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut old_acl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalMemory(descriptor.cast());
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
            ..Default::default()
        },
    };
    let mut acl = null_mut();
    let status = unsafe { SetEntriesInAclW(1, &access, old_acl, &mut acl) };
    let _acl = LocalMemory(acl.cast());
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}
