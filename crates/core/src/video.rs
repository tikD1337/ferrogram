//! Video playback requests from the UI and state reports back. No pixels cross this boundary:
//! the player renders into its own child window.
use std::path::PathBuf;

/// TDLib video file of a message. The preview frame travels as the message's `photo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub file: crate::MediaFile,
    /// Seconds, as reported by Telegram.
    pub duration: u32,
    /// Frame size in pixels as reported by Telegram; zero when unknown.
    pub width: u16,
    pub height: u16,
}

/// Area of the main window in physical pixels where the video is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoRequest {
    /// Shows the viewer window as Telegram does: centred on its monitor, sized to the video's
    /// aspect ratio plus the control bar (`bar`, physical pixels), over a dimmed backdrop.
    Present {
        viewer: isize,
        width: u32,
        height: u32,
        bar: i32,
    },
    /// `window` is the native handle of the main window (an `HWND` on Windows).
    Open {
        path: PathBuf,
        window: isize,
        rect: VideoRect,
    },
    Move(VideoRect),
    TogglePause,
    /// Fraction of the duration, clamped to 0..=1.
    Seek(f64),
    Close,
}

/// Reported on player events only (time updates come from the player, no polling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoState {
    pub position_ms: u32,
    pub duration_ms: u32,
    pub playing: bool,
    pub ended: bool,
    pub failed: bool,
    /// The user clicked the dimmed backdrop: the viewer should close.
    pub dismissed: bool,
}
