//! The only native boundary. Loading is deferred until engine startup, never test discovery.

use crate::engine::Transport;
use crate::mapping::{MAX_JSON_BYTES, error};
use libloading::Library;
use std::ffi::{CStr, c_char, c_int};
use std::path::PathBuf;
use tg_core::CommandError;

type Create = unsafe extern "C" fn() -> c_int;
type Send = unsafe extern "C" fn(c_int, *const c_char);
type Receive = unsafe extern "C" fn(f64) -> *const c_char;
type Execute = unsafe extern "C" fn(*const c_char) -> *const c_char;

pub(crate) struct TdJson {
    // The library outlives every copied function pointer and all client operations.
    _library: Library,
    td_create_client_id: Create,
    td_send: Send,
    td_receive: Receive,
    td_execute: Execute,
}

impl TdJson {
    pub(crate) fn load() -> Result<Self, CommandError> {
        // A release ships tdjson.dll next to the executable, as Telegram Desktop ships its
        // libraries. Only that absolute path is tried, never the working directory; a build
        // without it falls back to the TDLib installation it was built against.
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("tdjson.dll")))
            .filter(|path| path.is_absolute() && path.is_file());
        let path = beside.unwrap_or_else(|| {
            PathBuf::from(env!("TG_TDLIB_DIR"))
                .join("bin")
                .join("tdjson.dll")
        });
        if !path.is_absolute() {
            return Err(error(-4, "TDLIB_DIR must be absolute."));
        }
        #[cfg(windows)]
        // SAFETY: the trusted TDLib copy beside the executable or the build's installation.
        // Its dependencies come from the DLL's own directory and the standard system search
        // directories (LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | DEFAULT_DIRS), never the CWD.
        let library: Library = unsafe {
            libloading::os::windows::Library::load_with_flags(&path, 0x0000_0100 | 0x0000_1000)
        }
        .map_err(|_| error(-4, "Could not load TDLib (tdjson.dll)."))?
        .into();
        #[cfg(not(windows))]
        // SAFETY: same trusted absolute path requirement; only the Windows build is supported.
        let library =
            unsafe { Library::new(&path) }.map_err(|_| error(-4, "Could not load TDLib."))?;
        // SAFETY: names and C signatures match the installed td_json_client.h. The library is
        // retained in Self, so none of these pointers can outlive its code.
        let (td_create_client_id, td_send, td_receive, td_execute) = unsafe {
            (
                *library
                    .get::<Create>(b"td_create_client_id\0")
                    .map_err(|_| missing_symbol())?,
                *library
                    .get::<Send>(b"td_send\0")
                    .map_err(|_| missing_symbol())?,
                *library
                    .get::<Receive>(b"td_receive\0")
                    .map_err(|_| missing_symbol())?,
                *library
                    .get::<Execute>(b"td_execute\0")
                    .map_err(|_| missing_symbol())?,
            )
        };
        Ok(Self {
            _library: library,
            td_create_client_id,
            td_send,
            td_receive,
            td_execute,
        })
    }

    pub(crate) fn create(&self) -> i32 {
        // SAFETY: td_create_client_id has no preconditions and the library is alive.
        unsafe { (self.td_create_client_id)() }
    }

    // Called on the sole engine thread before create, never concurrently with receive.
    pub(crate) fn execute(&self, request: &CStr) -> Result<Option<String>, CommandError> {
        // SAFETY: request is NUL terminated and alive throughout the synchronous call.
        let pointer = unsafe { (self.td_execute)(request.as_ptr()) };
        copy_response(pointer)
    }
}

impl Transport for TdJson {
    fn send(&self, client: i32, request: &CStr) {
        // SAFETY: TDLib documents td_send as thread-safe and copies the request before return;
        // request is valid and NUL terminated and the library is retained by Shared.
        unsafe { (self.td_send)(client, request.as_ptr()) };
    }

    fn receive(&self) -> Result<Option<String>, CommandError> {
        // SAFETY: the process-wide engine lease permits exactly one receive thread. No execute
        // calls occur after startup. Copy the result before the next receive call.
        let pointer = unsafe { (self.td_receive)(60.0) };
        copy_response(pointer)
    }
}

fn missing_symbol() -> CommandError {
    error(-4, "TDLib is missing a required tdjson function.")
}

fn copy_response(pointer: *const c_char) -> Result<Option<String>, CommandError> {
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: a non-null pointer returned by td_receive/td_execute is a NUL-terminated string
    // owned by TDLib and remains valid until the next receive/execute, which has not happened.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    if bytes.len() > MAX_JSON_BYTES {
        return Err(error(-1, "Telegram response exceeds the size limit."));
    }
    // At most MAX_JSON_BYTES copied from TDLib. No native response is retained beyond decoding.
    let text =
        std::str::from_utf8(bytes).map_err(|_| error(-1, "Invalid Telegram response encoding."))?;
    Ok(Some(text.into()))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "Requires a trusted tdjson.dll installation; loads symbols only, never signs in."]
    fn loads_the_four_tdjson_symbols() {
        assert!(super::TdJson::load().is_ok());
    }
}
