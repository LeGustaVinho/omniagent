use std::{
    io,
    ptr::{null, null_mut},
    slice,
};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};

pub(crate) fn protect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    transform(bytes, |input, output| unsafe {
        CryptProtectData(
            input,
            null(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            output,
        )
    })
}

pub(crate) fn unprotect(bytes: &[u8]) -> io::Result<Vec<u8>> {
    transform(bytes, |input, output| unsafe {
        CryptUnprotectData(
            input,
            null_mut(),
            null(),
            null(),
            null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            output,
        )
    })
}

fn transform(
    bytes: &[u8],
    call: impl FnOnce(&CRYPT_INTEGER_BLOB, &mut CRYPT_INTEGER_BLOB) -> i32,
) -> io::Result<Vec<u8>> {
    let size = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload exceeds DPAPI range"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: size,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    if call(&input, &mut output) == 0 {
        return Err(io::Error::last_os_error());
    }
    if output.pbData.is_null() {
        return Ok(Vec::new());
    }
    // DPAPI transfers ownership of the output buffer and accepts LocalFree.
    let result = unsafe { slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe {
        output.pbData.write_bytes(0, output.cbData as usize);
        LocalFree(output.pbData.cast());
    }
    Ok(result)
}
