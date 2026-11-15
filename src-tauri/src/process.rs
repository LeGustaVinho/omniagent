use std::{
    ffi::OsStr,
    fs::File,
    io,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
    sync::Mutex,
};
use windows_sys::Win32::{
    Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Security::SECURITY_ATTRIBUTES,
    System::{
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

const CANCEL_EXIT_CODE: u32 = 0xC000_013A;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalOutcome {
    Exited(u32),
    Cancelled(u32),
}

pub struct WindowsProcess {
    pub pid: u32,
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
    pub control: WindowsProcessControl,
}

pub struct WindowsProcessControl {
    process: OwnedHandle,
    job: OwnedHandle,
    cancelled: Mutex<bool>,
}

impl WindowsProcessControl {
    pub fn cancel(&self) -> io::Result<bool> {
        let mut cancelled = self.cancelled.lock().expect("process state lock");
        if *cancelled {
            return Ok(false);
        }
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => {
                self.terminate_job()?;
                *cancelled = true;
                Ok(true)
            }
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub fn wait(&self) -> io::Result<TerminalOutcome> {
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let cancelled = self.cancelled.lock().expect("process state lock");
        let mut code = 0;
        checked(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) })?;
        // A descendant can otherwise keep output pipes open after the main process exits.
        self.terminate_job()?;
        Ok(if *cancelled {
            TerminalOutcome::Cancelled(code)
        } else {
            TerminalOutcome::Exited(code)
        })
    }

    fn terminate_job(&self) -> io::Result<()> {
        checked(unsafe { TerminateJobObject(self.job.as_raw_handle(), CANCEL_EXIT_CODE) })
    }
}

pub fn launch(executable: &Path, cwd: &Path, args: &[&OsStr]) -> io::Result<WindowsProcess> {
    if !executable.is_absolute() || !cwd.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "launch paths must be absolute",
        ));
    }
    let executable_path = terminated(executable.as_os_str())?;
    let cwd = terminated(cwd.as_os_str())?;
    let mut command_line = Vec::new();
    append_arg(&mut command_line, executable.as_os_str())?;
    for arg in args {
        command_line.push(b' ' as u16);
        append_arg(&mut command_line, arg)?;
    }
    command_line.push(0);

    let job = create_job()?;
    let (child_stdin, parent_stdin) = pipe(false)?;
    let (parent_stdout, child_stdout) = pipe(true)?;
    let (parent_stderr, child_stderr) = pipe(true)?;
    let inherited = [
        child_stdin.as_raw_handle(),
        child_stdout.as_raw_handle(),
        child_stderr.as_raw_handle(),
    ];
    let jobs = [job.as_raw_handle()];
    let mut attrs = ProcessAttributes::new(2)?;
    attrs.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)?;
    attrs.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;

    let mut info = STARTUPINFOEXW::default();
    info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    [
        info.StartupInfo.hStdInput,
        info.StartupInfo.hStdOutput,
        info.StartupInfo.hStdError,
    ] = inherited;
    info.lpAttributeList = attrs.ptr();
    let mut process = PROCESS_INFORMATION::default();
    // Pointers are valid throughout creation. The job list places the child under
    // supervision before its first instruction executes.
    checked(unsafe {
        CreateProcessW(
            executable_path.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            null(),
            cwd.as_ptr(),
            &info.StartupInfo,
            &mut process,
        )
    })?;
    let _thread = unsafe { OwnedHandle::from_raw_handle(process.hThread) };
    Ok(WindowsProcess {
        pid: process.dwProcessId,
        stdin: File::from(parent_stdin),
        stdout: File::from(parent_stdout),
        stderr: File::from(parent_stderr),
        control: WindowsProcessControl {
            process: unsafe { OwnedHandle::from_raw_handle(process.hProcess) },
            job,
            cancelled: Mutex::new(false),
        },
    })
}

fn create_job() -> io::Result<OwnedHandle> {
    // Null security attributes create a non-inheritable handle.
    let raw = unsafe { CreateJobObjectW(null(), null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    checked(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of_val(&limits) as u32,
        )
    })?;
    Ok(job)
}

fn pipe(parent_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let (mut read, mut write) = (null_mut(), null_mut());
    checked(unsafe { CreatePipe(&mut read, &mut write, &attrs, 0) })?;
    let (read, write) = unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    };
    let parent = if parent_reads { &read } else { &write };
    checked(unsafe { SetHandleInformation(parent.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) })?;
    Ok((read, write))
}

struct ProcessAttributes(Vec<usize>);

impl ProcessAttributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0;
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        // The list is opaque and needs pointer alignment.
        let mut storage = vec![0_usize; size.div_ceil(size_of::<usize>())];
        checked(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut size)
        })?;
        Ok(Self(storage))
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }

    fn set(&mut self, key: u32, values: &[HANDLE]) -> io::Result<()> {
        checked(unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                key as usize,
                values.as_ptr().cast(),
                size_of_val(values),
                null_mut(),
                null(),
            )
        })
    }
}

impl Drop for ProcessAttributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn terminated(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut encoded: Vec<_> = value.encode_wide().collect();
    if encoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "embedded NUL in launch parameter",
        ));
    }
    encoded.push(0);
    Ok(encoded)
}

fn append_arg(output: &mut Vec<u16>, argument: &OsStr) -> io::Result<()> {
    const QUOTE: u16 = b'"' as u16;
    const SLASH: u16 = b'\\' as u16;
    output.push(QUOTE);
    let mut slashes = 0;
    for unit in argument.encode_wide() {
        match unit {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "embedded NUL in argument",
                ));
            }
            SLASH => slashes += 1,
            _ => {
                if unit == QUOTE {
                    slashes = slashes * 2 + 1;
                }
                output.extend(std::iter::repeat_n(SLASH, slashes));
                output.push(unit);
                slashes = 0;
            }
        }
    }
    output.extend(std::iter::repeat_n(SLASH, slashes * 2));
    output.push(QUOTE);
    Ok(())
}

fn checked(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
