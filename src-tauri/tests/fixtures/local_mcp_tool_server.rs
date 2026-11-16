use std::{
    ffi::c_void,
    io::{self, BufRead, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    thread,
    time::Duration,
};

#[link(name = "advapi32")]
unsafe extern "system" {
    fn CreateWellKnownSid(kind: i32, domain: *mut c_void, sid: *mut c_void, size: *mut u32) -> i32;
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: i32,
        data: *mut c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    fn EqualSid(first: *mut c_void, second: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
}

#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}

#[repr(C)]
struct TokenGroups {
    count: u32,
    groups: [SidAndAttributes; 0],
}

fn has_internet_capability() -> io::Result<bool> {
    const INTERNET_CLIENT_SID: i32 = 85;
    const TOKEN_QUERY: u32 = 8;
    const TOKEN_CAPABILITIES: i32 = 30;
    let mut sid = [0_u32; 17];
    let mut size = size_of_val(&sid) as u32;
    let sid = sid.as_mut_ptr().cast();
    let mut token = std::ptr::null_mut();
    if unsafe { CreateWellKnownSid(INTERNET_CLIENT_SID, std::ptr::null_mut(), sid, &mut size) } == 0
        || unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut buffer = [0_usize; 128];
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TOKEN_CAPABILITIES,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token_groups = unsafe { &*buffer.as_ptr().cast::<TokenGroups>() };
    let offset = std::mem::offset_of!(TokenGroups, groups);
    if offset + token_groups.count as usize * size_of::<SidAndAttributes>() > returned as usize {
        return Err(io::Error::other("Invalid fixture token capability list"));
    }
    let groups = unsafe {
        std::slice::from_raw_parts(token_groups.groups.as_ptr(), token_groups.count as usize)
    };
    Ok(groups
        .iter()
        .any(|group| unsafe { EqualSid(group.sid, sid) != 0 }))
}

fn main() -> io::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let protected = PathBuf::from(arguments.next().expect("fixture repository path"));
    let changed = arguments.any(|value| value == "--changed-schema");
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut mutations = 0;
    for incoming in stdin.lock().lines() {
        let input = incoming?;
        if input.contains("\"method\":\"initialize\"") {
            writeln!(
                stdout,
                r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fixture","version":"1"}}}}}}"#
            )?;
        } else if input.contains("\"method\":\"tools/list\"") {
            let kind = if changed { "number" } else { "string" };
            let input_schema = format!(
                r#"{{"type":"object","properties":{{"destination":{{"type":"{kind}"}},"action":{{"type":"string","enum":["mutate","hang","verify_mapping"]}}}},"required":["destination","action"],"additionalProperties":false}}"#
            );
            let output_schema = r#"{"type":"object","properties":{"repositoryDenied":{"type":"boolean"},"userProfileAbsent":{"type":"boolean"},"outboundCapability":{"type":"boolean"},"mutationCount":{"type":"integer"}},"required":["repositoryDenied","userProfileAbsent","outboundCapability","mutationCount"],"additionalProperties":false}"#;
            writeln!(
                stdout,
                r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"mutate_fixture","inputSchema":{input_schema},"outputSchema":{output_schema}}}]}}}}"#
            )?;
        } else if input.contains("\"method\":\"tools/call\"") {
            if input.contains("\"action\":\"verify_mapping\"")
                && !input
                    .contains(r#""arguments":{"action":"verify_mapping","destination":"brief.md"}"#)
            {
                writeln!(
                    stdout,
                    r#"{{"jsonrpc":"2.0","id":3,"error":{{"code":-32602,"message":"Unexpected fixture argument mapping"}}}}"#
                )?;
                stdout.flush()?;
                continue;
            }
            let denied = |attempt: io::Error| attempt.kind() == io::ErrorKind::PermissionDenied;
            let read_denied = std::fs::read(&protected).is_err_and(denied);
            let write_denied = std::fs::write(&protected, b"escaped sandbox").is_err_and(denied);
            let isolated = read_denied && write_denied;
            let environment = std::env::var_os("USERPROFILE").is_none();
            let outbound = has_internet_capability()?;
            mutations += 1;
            if input.contains("\"action\":\"hang\"") {
                writeln!(
                    stdout,
                    r#"{{"jsonrpc":"2.0","method":"notifications/message","params":{{"level":"info","data":"fixture-call-received"}}}}"#
                )?;
                stdout.flush()?;
                thread::sleep(Duration::from_secs(60));
                return Ok(());
            }
            writeln!(
                stdout,
                r#"{{"jsonrpc":"2.0","id":3,"result":{{"structuredContent":{{"repositoryDenied":{isolated},"userProfileAbsent":{environment},"outboundCapability":{outbound},"mutationCount":{mutations}}},"content":[{{"type":"text","text":"Local fixture operation applied"}}]}}}}"#
            )?;
        }
        stdout.flush()?;
    }
    Ok(())
}
