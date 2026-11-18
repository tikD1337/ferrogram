//! Win32 lifetime boundary: the native window and all its state belong to one message thread.
use crate::Error;
use std::sync::mpsc::{Receiver, SyncSender};
use tg_core::{ChatId, Notification};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
fn native_error(operation: &'static str) -> Error {
    // SAFETY: reads this native thread's last-error code; no pointers or user data.
    Error(operation, unsafe { GetLastError() })
}
const WAKE: u32 = WM_APP + 1;
const BALLOON: u32 = WM_APP + 2;
const CLASS: &[u16] = &[
    116, 103, 45, 114, 117, 115, 116, 45, 110, 111, 116, 105, 102, 105, 99, 97, 116, 105, 111, 110,
    115, 0,
];
struct State {
    receiver: Receiver<Notification>,
    callback: Box<dyn Fn(ChatId) + Send>,
    current: Option<ChatId>,
    icon: NOTIFYICONDATAW,
}
/// Copies complete UTF-16 scalars only and always leaves a trailing NUL.
fn wide<const N: usize>(destination: &mut [u16; N], text: &str) {
    destination.fill(0);
    let mut index = 0;
    for ch in text.chars().filter(|ch| *ch != '\0') {
        let mut encoded = [0; 2];
        let units = ch.encode_utf16(&mut encoded);
        if index + units.len() >= N {
            break;
        }
        destination[index..index + units.len()].copy_from_slice(units);
        index += units.len();
    }
}
impl State {
    fn next(&mut self) {
        if self.current.is_some() {
            return;
        }
        if let Ok(notification) = self.receiver.try_recv() {
            self.icon.uFlags = NIF_INFO;
            wide(&mut self.icon.szInfoTitle, &notification.title);
            wide(&mut self.icon.szInfo, &notification.body);
            self.icon.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
            self.current = Some(notification.chat);
            // SAFETY: icon has its documented size and a live HWND on this message thread.
            if unsafe { Shell_NotifyIconW(NIM_MODIFY, &self.icon) } == 0 {
                self.current = None;
            }
            self.icon.szInfo.fill(0);
            self.icon.szInfoTitle.fill(0);
        }
    }
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: Windows supplies CREATESTRUCTW for WM_NCCREATE. Its payload is the
        // worker-owned Box<State>, which outlives this window and is never moved.
        unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    // SAFETY: USERDATA is null before WM_NCCREATE, then points to the worker's live State.
    let state =
        unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const std::cell::RefCell<State> };
    if !state.is_null() {
        // SAFETY: the worker-owned RefCell outlives the HWND. A checked borrow prevents
        // aliasing if Windows synchronously reenters while dispatching Shell calls.
        let cell = unsafe { &*state };
        let Ok(mut state) = cell.try_borrow_mut() else {
            // SAFETY: post the reentrant event for dispatch after the active borrow ends.
            unsafe {
                PostMessageW(window, message, wparam, lparam);
            }
            return 0;
        };
        if message == WAKE {
            state.next();
            return 0;
        }
        if message == BALLOON {
            match lparam as u32 {
                NIN_BALLOONUSERCLICK => {
                    if let Some(chat) = state.current.take() {
                        (state.callback)(chat);
                    }
                    state.next();
                }
                NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT => {
                    state.current = None;
                    state.next();
                }
                _ => {}
            }
            return 0;
        }
    }
    if message == WM_CLOSE {
        // SAFETY: this procedure owns the live window and destroys it on its owning thread.
        unsafe {
            DestroyWindow(window);
        }
        return 0;
    }
    if message == WM_DESTROY {
        // SAFETY: requests termination of this thread's blocking GetMessage loop.
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    // SAFETY: forwards the unmodified parameters Windows supplied to its default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
pub(super) fn run(
    receiver: Receiver<Notification>,
    callback: Box<dyn Fn(ChatId) + Send>,
    ready: SyncSender<Result<usize, Error>>,
) {
    // SAFETY: null selects the current module; no borrowed string or ownership transfer.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class = WNDCLASSW {
        lpfnWndProc: Some(procedure),
        hInstance: instance,
        lpszClassName: CLASS.as_ptr(),
        ..Default::default()
    };
    // SAFETY: class and its NUL-terminated name live throughout registration/window creation.
    let registered = unsafe { RegisterClassW(&class) };
    if registered == 0 {
        let _ = ready.send(Err(native_error("RegisterClassW")));
        return;
    }
    let state = Box::new(std::cell::RefCell::new(State {
        receiver,
        callback,
        current: None,
        icon: NOTIFYICONDATAW::default(),
    }));
    // SAFETY: message-only HWND, valid registered class, null title and stable State payload.
    let window = unsafe {
        CreateWindowExW(
            0,
            CLASS.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::from_ref(&*state) as *mut std::ffi::c_void,
        )
    };
    if window.is_null() {
        // SAFETY: this thread registered the class and created no windows with it.
        unsafe {
            UnregisterClassW(CLASS.as_ptr(), instance);
        }
        let _ = ready.send(Err(native_error("CreateWindowExW")));
        return;
    }
    let mut data = state.borrow_mut();
    data.icon.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.icon.hWnd = window;
    data.icon.uID = 1;
    data.icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.icon.uCallbackMessage = BALLOON;
    // SAFETY: predefined shared application icon; ownership stays with Windows, so no DestroyIcon.
    data.icon.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
    wide(&mut data.icon.szTip, "Veygram");
    // SAFETY: initialized NOTIFYICONDATAW, live HWND and shared valid HICON.
    let installed = unsafe { Shell_NotifyIconW(NIM_ADD, &data.icon) } != 0;
    if !installed || ready.send(Ok(window as usize)).is_err() {
        let _ = ready.try_send(Err(native_error("Shell_NotifyIconW(NIM_ADD)")));
        // SAFETY: own HWND and, if installed, this exact tray icon; no foreign resources deleted.
        unsafe {
            if installed {
                Shell_NotifyIconW(NIM_DELETE, &data.icon);
            }
            DestroyWindow(window);
            UnregisterClassW(CLASS.as_ptr(), instance);
        }
        return;
    }
    let cleanup_icon = data.icon;
    drop(data);
    // Use legacy callback format deliberately: lParam is the notification code, wParam the icon ID.
    let mut message = MSG::default();
    loop {
        // SAFETY: valid writable MSG; blocks until an OS message, never polls.
        let received = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if received <= 0 {
            break;
        }
        // SAFETY: dispatches only messages obtained from this thread's GetMessage call.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // SAFETY: this thread owns the registered icon; the window has finished its message loop.
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &cleanup_icon);
        if IsWindow(window) != 0 {
            DestroyWindow(window);
        }
        UnregisterClassW(CLASS.as_ptr(), instance);
    }
}
pub(super) fn wake(window: usize) -> Result<(), Error> {
    // SAFETY: opaque HWND belongs to the running service. PostMessage transfers no borrowed data.
    if unsafe { PostMessageW(window as HWND, WAKE, 0, 0) } == 0 {
        Err(native_error("PostMessageW"))
    } else {
        Ok(())
    }
}
pub(super) fn close(window: usize) {
    // SAFETY: requests closure of the service's own HWND, with no transferred pointers.
    unsafe {
        PostMessageW(window as HWND, WM_CLOSE, 0, 0);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_buffers_are_nul_terminated_without_split_surrogates() {
        let mut out = [0u16; 4];
        wide(&mut out, "ab\u{1f642}");
        assert_eq!(out, [97, 98, 0, 0]);
        wide(&mut out, "a\u{1f642}");
        assert_eq!(out[3], 0);
        assert!(String::from_utf16(&out[..3]).is_ok());
    }
}
