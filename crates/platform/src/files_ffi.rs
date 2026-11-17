//! The shell boundary. Never builds paths from display names or invokes a command-line shell.
use crate::Error;
use std::{os::windows::ffi::OsStrExt, path::Path, sync::mpsc};
use tg_core::FileRequest;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::UI::{
    Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems, ShellExecuteW},
    WindowsAndMessaging::SW_SHOWNORMAL,
};
pub(crate) fn run(receiver: mpsc::Receiver<FileRequest>, report: impl Fn(bool)) {
    // SAFETY: this worker owns an STA for shell extensions; balanced on this same thread below.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
    while let Ok(request) = receiver.recv() {
        let result = if initialized {
            match request {
                FileRequest::Open(path) => open(&path),
                FileRequest::ShowInFolder(path) => reveal(&path),
            }
        } else {
            Err(Error("shell COM initialization", 0))
        };
        report(result.is_ok());
    }
    if initialized {
        // SAFETY: balances the successful CoInitializeEx on this worker.
        unsafe {
            CoUninitialize();
        }
    }
}
fn wide(path: &Path) -> Result<Vec<u16>, Error> {
    if !path.is_absolute() {
        return Err(Error("file path", 0));
    }
    let mut encoded: Vec<u16> = path.as_os_str().encode_wide().take(32768).collect();
    if encoded.len() >= 32768 || encoded.contains(&0) {
        return Err(Error("file path", 0));
    }
    encoded.push(0);
    Ok(encoded)
}
fn open(path: &Path) -> Result<(), Error> {
    // Fail closed if the provenance mark cannot be written. No executable bypass or shell command.
    crate::files::mark_internet(path).map_err(|_| Error("internet zone marking", 0))?;
    let path = wide(path)?;
    let verb = [111u16, 112, 101, 110, 0];
    // SAFETY: both UTF-16 buffers are terminated and alive for this synchronous call; all optional
    // arguments are null. This runs only for an explicit UI click/confirmed executable action.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(Error("open downloaded file", result as u32))
    }
}
fn reveal(path: &Path) -> Result<(), Error> {
    let path = wide(path)?;
    // SAFETY: the terminated path buffer lives through the call; the returned PIDL is owned here.
    let pidl = unsafe { ILCreateFromPathW(path.as_ptr()) };
    if pidl.is_null() {
        return Err(Error("locate downloaded file", 0));
    }
    // SAFETY: PIDL is valid; cidl=0 selects the item represented by that full PIDL, no child array.
    let status = unsafe { SHOpenFolderAndSelectItems(pidl, 0, std::ptr::null(), 0) };
    // SAFETY: release exactly the PIDL allocated by ILCreateFromPathW above.
    unsafe {
        ILFree(pidl);
    }
    if status >= 0 {
        Ok(())
    } else {
        Err(Error("show downloaded file", status as u32))
    }
}
