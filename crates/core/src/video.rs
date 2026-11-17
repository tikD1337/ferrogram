//! Video playback requests, state and bounded round frame exchange.
use std::path::PathBuf;

/// TDLib video file of a message. The preview frame travels as the message's `photo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Video {
    pub supports_streaming: bool,
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
    /// Plays a streamable video from TDLib's partial file while it downloads.
    OpenStream {
        file: crate::StreamFile,
        window: isize,
        rect: VideoRect,
        token: u64,
    },
    /// New downloaded ranges of the streamed file; wakes a read that waits for them.
    Availability {
        file: crate::StreamFile,
        token: u64,
    },
    /// `window` is the native handle of the main window (an `HWND` on Windows).
    Open {
        path: PathBuf,
        window: isize,
        rect: VideoRect,
    },
    /// Audio-only Media Engine; no native window or pixel buffer is created.
    OpenAudio {
        path: PathBuf,
        token: u64,
    },
    /// Frame-server round playback. No native window is created.
    OpenRound {
        path: PathBuf,
        size: u32,
        token: u64,
    },
    /// Physical output size and whether a round surface is visible.
    RoundSurface {
        size: u32,
        visible: bool,
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
    pub stream: Option<crate::StreamStatus>,
    pub stream_token: Option<u64>,
    pub buffering: bool,
    pub first_frame: bool,
    /// Correlates voice/round callbacks; late states cannot update another message/video.
    pub audio_token: Option<u64>,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub playing: bool,
    pub ended: bool,
    pub failed: bool,
    /// The user clicked the dimmed backdrop: the viewer should close.
    pub dismissed: bool,
}

/// One latest RGBA frame, shared by the platform producer and the UI consumer.
/// The storage is reused. Events carry this handle, never another pixel allocation.
#[derive(Debug, Clone)]
pub struct RoundFrame {
    pub token: u64,
    pub pixels: std::sync::Arc<std::sync::Mutex<RoundPixels>>,
}
#[derive(Debug, Default)]
pub struct RoundPixels {
    pub width: u32,
    pub height: u32,
    pub sequence: u64,
    pub rgba: Vec<u8>,
}
impl PartialEq for RoundFrame {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token && std::sync::Arc::ptr_eq(&self.pixels, &other.pixels)
    }
}
impl Eq for RoundFrame {}
