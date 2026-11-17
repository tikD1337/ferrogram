//! Event-driven Windows tray balloons. No message text is logged.
#[allow(unsafe_code)]
mod console;
#[allow(unsafe_code)]
mod ffi;
mod files;
#[allow(unsafe_code)]
mod hardening;
pub use files::Files;
#[allow(unsafe_code)]
mod files_ffi;
mod video;
#[allow(unsafe_code)]
mod video_ffi;
pub use console::{attach_parent_console, show_error};
pub use hardening::{Hardening, harden_process};
use std::sync::mpsc;
use tg_core::{ChatId, Notification};
pub use video::Player;
#[derive(Debug, Clone, Copy)]
pub struct Error(pub(crate) &'static str, pub(crate) u32);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Desktop notification service failed: {} ({})",
            self.0, self.1
        )
    }
}
impl std::error::Error for Error {}
/// Create/drop off the running UI thread; show is nonblocking and bounded.
pub struct Notifications {
    sender: mpsc::SyncSender<Notification>,
    window: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl std::fmt::Debug for Notifications {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notifications(<redacted>)")
    }
}
impl Notifications {
    pub fn start(callback: impl Fn(ChatId) + Send + 'static) -> Result<Self, Error> {
        // At most 32 queued balloons plus one currently displayed balloon (<=32 KiB text).
        let (sender, receiver) = mpsc::sync_channel(32);
        let (ready, result) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("tg-notifications".into())
            .spawn(move || ffi::run(receiver, Box::new(callback), ready))
            .map_err(|_| Error("worker startup", 0))?;
        match result.recv() {
            Ok(Ok(window)) => Ok(Self {
                sender,
                window,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            _ => {
                let _ = thread.join();
                Err(Error("window startup", 0))
            }
        }
    }
    pub fn show(&self, mut notification: Notification) -> Result<(), Error> {
        // Public callers cannot bypass the per-item queue budget (192 + 768 UTF-8 bytes).
        for (text, limit) in [
            (&mut notification.title, 192),
            (&mut notification.body, 768),
        ] {
            let mut end = text.len().min(limit);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        self.sender
            .try_send(notification)
            .map_err(|_| Error("worker startup", 0))?;
        ffi::wake(self.window)
    }
}
impl Drop for Notifications {
    fn drop(&mut self) {
        ffi::close(self.window);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
