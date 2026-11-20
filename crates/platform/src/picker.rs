//! Attachment sources: the open-file dialog and the clipboard bitmap.
//! Called only after an explicit attach or paste action, never on a timer.
//! Errors are short user-facing reasons without paths or clipboard contents.
use std::path::PathBuf;
use windows_sys::Win32::System::{DataExchange::*, Memory::*};
use windows_sys::Win32::UI::Controls::Dialogs::*;

/// CF_DIB, CF_UNICODETEXT and CF_DIBV5 from the standard clipboard formats.
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;
/// Larger clipboard bitmaps are rejected before any byte is read.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;

/// Shows the modal open-file dialog owned by `owner` (a Win32 window handle, or 0).
/// Blocks the calling thread until the user chooses; `Ok(None)` means cancelled.
pub fn pick_file(owner: isize) -> Result<Option<PathBuf>, &'static str> {
    use std::os::windows::ffi::OsStringExt;
    let mut path = vec![0u16; 32768];
    let filter: Vec<u16> = "All files\0*.*\0Photos\0*.jpg;*.jpeg;*.png\0\0"
        .encode_utf16()
        .collect();
    let title: Vec<u16> = "Attach a file\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner as _,
        lpstrFile: path.as_mut_ptr(),
        nMaxFile: path.len() as u32,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every pointer in `dialog` refers to a buffer that outlives this modal call, and
    // `nMaxFile` is the length of `path`. The owner window keeps processing its own messages on
    // its thread; the handle is only passed through, never dereferenced here.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        // SAFETY: no pointers; reports the result of the dialog call above on this thread.
        return if unsafe { CommDlgExtendedError() } == 0 {
            Ok(None)
        } else {
            Err("Could not open the file dialog.")
        };
    }
    let end = path
        .iter()
        .position(|c| *c == 0)
        .ok_or("Invalid file selection.")?;
    Ok(Some(std::ffi::OsString::from_wide(&path[..end]).into()))
}

/// A cheap format query for Ctrl+V: true when the clipboard holds a device-independent bitmap
/// and no text. Spreadsheet and Office copies carry both (Windows synthesizes CF_DIB from their
/// bitmap); those must paste as text.
pub fn clipboard_has_image() -> bool {
    // SAFETY: read-only format queries without pointers or clipboard ownership.
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT) == 0
            && (IsClipboardFormatAvailable(CF_DIB) != 0
                || IsClipboardFormatAvailable(CF_DIBV5) != 0)
    }
}

/// Passes the clipboard bitmap (a packed DIB without a file header) to `read` while the
/// clipboard stays open and its memory locked. The bytes are borrowed, never copied.
pub fn read_clipboard_image<R>(read: impl FnOnce(&[u8]) -> R) -> Result<R, &'static str> {
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            // SAFETY: constructed only after OpenClipboard succeeded on this thread.
            unsafe {
                CloseClipboard();
            }
        }
    }
    struct Locked(windows_sys::Win32::Foundation::HGLOBAL);
    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: matches a successful GlobalLock; the allocation belongs to the clipboard.
            unsafe {
                GlobalUnlock(self.0);
            }
        }
    }
    // SAFETY: a null owner is valid for a read-only clipboard consumer.
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return Err("The clipboard is busy. Try pasting again.");
    }
    let _clipboard = Clipboard;
    // SAFETY: the clipboard is open on this thread. The returned handle is borrowed and stays
    // valid until CloseClipboard; it is never freed here.
    let handle = unsafe {
        GetClipboardData(if IsClipboardFormatAvailable(CF_DIBV5) != 0 {
            CF_DIBV5
        } else {
            CF_DIB
        })
    };
    if handle.is_null() {
        return Err("No image in the clipboard.");
    }
    // SAFETY: DIB clipboard formats are global memory handles; the query does not lock them.
    let size = unsafe { GlobalSize(handle) };
    if size == 0 || size > MAX_CLIPBOARD_BYTES {
        return Err("Clipboard image exceeds the 64 MiB limit.");
    }
    // SAFETY: `handle` is a live global memory handle owned by the open clipboard.
    let data = unsafe { GlobalLock(handle) };
    if data.is_null() {
        return Err("Could not read the clipboard image.");
    }
    let _locked = Locked(handle);
    // SAFETY: GlobalSize bounds the slice to the locked allocation. The slice is dropped when
    // `read` returns, before the lock and clipboard guards release the memory.
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
    Ok(read(bytes))
}
