//! Startup output for a GUI-subsystem executable: the parent's console when there is one,
//! otherwise a message box.
use windows_sys::Win32::{
    Foundation::{ERROR_ACCESS_DENIED, GetLastError},
    System::Console::{ATTACH_PARENT_PROCESS, AttachConsole},
    UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW},
};

/// Attaches to the console of the process that started us (a terminal), so `--help` and
/// startup errors are printed there. Returns whether a console is available: either attached
/// now or already present (a console-subsystem debug build).
pub fn attach_parent_console() -> bool {
    // SAFETY: AttachConsole takes a process id by value and has no pointer arguments.
    // GetLastError only reads the calling thread's last-error value.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 || GetLastError() == ERROR_ACCESS_DENIED }
}

/// Shows a modal error box with no owner window. Only for startup failures, before or after the
/// UI loop; never from a UI callback.
pub fn show_error(text: &str) {
    let text = wide(text);
    let title = wide("Veygram");
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that live until the call returns; a
    // null owner window is allowed.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

fn wide(text: &str) -> Vec<u16> {
    // Interior NULs would truncate the text silently; replace them.
    text.encode_utf16()
        .map(|unit| if unit == 0 { u16::from(b' ') } else { unit })
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::wide;

    #[test]
    fn wide_strings_are_terminated_once_and_have_no_interior_nul() {
        let text = wide("a\0b");
        assert_eq!(text, [u16::from(b'a'), 32, u16::from(b'b'), 0]);
    }
}
