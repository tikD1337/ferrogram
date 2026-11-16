//! Minimal DPAPI wrapper: binds a blob to the current Windows user account.
//!
//! This is the weakest layer of the planned key protection (see `docs/SECURITY.md`): it stops
//! the database from being opened after a plain file copy to another machine or account, and
//! nothing more. The probe uses it so that a real session is never stored unencrypted.

use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    transform(plain, true)
}

pub fn unprotect(blob: &[u8]) -> io::Result<Vec<u8>> {
    transform(blob, false)
}

fn transform(input: &[u8], protect: bool) -> io::Result<Vec<u8>> {
    let length = u32::try_from(input.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "blob too large"))?;
    let source = CRYPT_INTEGER_BLOB {
        cbData: length,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    // SAFETY: `source` points at `input`, which outlives the call and is only read by it;
    // `output` is a valid out-parameter; every optional pointer is null, as the API allows.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &source,
                ptr::null(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: on success the API returns a buffer of exactly `cbData` bytes allocated with
    // LocalAlloc. It is copied out and then released exactly once.
    let result = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        bytes
    };
    Ok(result)
}
