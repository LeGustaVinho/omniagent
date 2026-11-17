use std::{
    io,
    os::windows::io::{FromRawHandle, OwnedHandle},
    ptr::{NonNull, null_mut},
};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub(super) fn current_user_pipe(name: &str) -> io::Result<NamedPipeServer> {
    let sid = current_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})\0").encode_utf16().collect();
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = LocalAllocation(NonNull::new(descriptor).expect("valid security descriptor"));
    let mut attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.as_ptr(),
        bInheritHandle: 0,
    };
    // CreateNamedPipe copies the descriptor before the call returns.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

pub(crate) fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _token_owner = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut bytes = 0;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut bytes) };
    if (bytes as usize) < size_of::<TOKEN_USER>() {
        return Err(io::Error::last_os_error());
    }
    let mut info = vec![0_usize; (bytes as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            info.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // The aligned buffer contains TOKEN_USER and its SID until conversion finishes.
    let user = unsafe { &*info.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _sid_owner = LocalAllocation(NonNull::new(sid.cast()).expect("valid SID string"));
    let length = unsafe { (0..).find(|index| *sid.add(*index) == 0).unwrap() };
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(sid, length)
    }))
}

struct LocalAllocation(NonNull<std::ffi::c_void>);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0.as_ptr()) };
    }
}
