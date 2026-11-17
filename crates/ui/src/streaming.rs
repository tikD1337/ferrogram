//! Viewer streaming state. Missing byte ranges drive TDLib requests; the playback position only
//! decides how far ahead the download window may slide.
use super::*;
use tg_core::{
    LoadedRanges, StreamFile, StreamRanges, StreamStatus, StreamWindow, Video, VideoState,
};
/// Loaded ahead when the duration is unknown, so no byte rate can be derived.
const AHEAD_WITHOUT_DURATION: u64 = 8 * tg_core::STREAM_PRELOAD;
pub(super) struct ViewerStream {
    pub token: u64,
    pub file: StreamFile,
    ranges: StreamRanges,
    loaded: LoadedRanges,
    waiting: bool,
    engine_waiting: bool,
    duration_ms: u64,
    position_ms: u64,
}
impl ViewerStream {
    /// The file offset playback needs data up to: `STREAM_AHEAD_SECONDS` past the position, at
    /// the file's average byte rate, as Telegram Desktop loads ahead by time.
    fn target(&self) -> u64 {
        let size = self.file.size;
        if self.duration_ms == 0 {
            return AHEAD_WITHOUT_DURATION;
        }
        let ahead_ms = self
            .position_ms
            .saturating_add(tg_core::STREAM_AHEAD_SECONDS * 1000);
        let bytes = u128::from(ahead_ms) * u128::from(size) / u128::from(self.duration_ms);
        u64::try_from(bytes).unwrap_or(u64::MAX).min(size)
    }
    /// The loaded range shown on the seek bar: the one holding the current download window, or
    /// ending where it starts while the window has not received its first part yet.
    fn shown(&self) -> (f32, f32) {
        let offset = self.file.offset;
        let (start, end) = self
            .loaded
            .around(offset)
            .or_else(|| offset.checked_sub(1).and_then(|o| self.loaded.around(o)))
            .unwrap_or((offset, offset));
        let size = self.file.size.max(1) as f32;
        (start as f32 / size, end as f32 / size)
    }
}
fn download(file: i32, window: StreamWindow) -> Command {
    Command::DownloadRange {
        file,
        offset: window.offset,
        limit: window.limit,
    }
}
impl State {
    fn send_stream(&self, w: &AppWindow, command: Option<Command>) {
        if let Some(command) = command
            && !self.send(w, command)
        {
            self.close_video(w);
            w.set_error("Could not request video data.".into());
        }
    }
    pub(super) fn start_stream(&self, w: &AppWindow, video: &Video) {
        let token = self.stream_serial.get().wrapping_add(1);
        self.stream_serial.set(token);
        let file = video.file.partial.clone().unwrap_or(StreamFile {
            id: video.file.id,
            size: video.file.size,
            path: None,
            offset: 0,
            prefix: 0,
            completed: false,
        });
        let mut ranges = StreamRanges::default();
        ranges.update(&file);
        let mut loaded = LoadedRanges::default();
        loaded.add(&file);
        let first = ranges.need(0, file.size).unwrap_or(StreamWindow {
            offset: 0,
            limit: file.size,
        });
        *self.streaming.borrow_mut() = Some(ViewerStream {
            token,
            file: file.clone(),
            ranges,
            loaded,
            waiting: true,
            engine_waiting: true,
            duration_ms: u64::from(video.duration) * 1000,
            position_ms: 0,
        });
        if !self.send(w, download(video.file.id, first)) {
            self.close_video(w);
            w.set_error("Could not start the video download.".into());
            return;
        }
        self.viewer.set_loading(true);
        self.viewer.set_download_start(0.0);
        self.viewer.set_download_end(0.0);
        if let Some(window) = native_window(self.viewer.window()) {
            self.video(
                w,
                VideoRequest::OpenStream {
                    file,
                    token,
                    window,
                    rect: Self::video_rect(&self.viewer),
                },
            );
        } else {
            self.close_video(w);
        }
    }
    pub(super) fn stream_status(&self, w: &AppWindow, status: StreamStatus) {
        let command = {
            let mut current = self.streaming.borrow_mut();
            let Some(s) = current
                .as_mut()
                .filter(|s| s.token == status.token && s.file.id == status.file)
            else {
                return;
            };
            s.waiting = status.waiting;
            self.viewer.set_loading(s.waiting || s.engine_waiting);
            status
                .needed
                .and_then(|offset| s.ranges.need(offset, s.file.size))
                .map(|window| download(status.file, window))
        };
        self.send_stream(w, command);
    }
    pub(super) fn stream_file(&self, w: &AppWindow, file: &tg_core::MediaFile) -> bool {
        let update = {
            let mut current = self.streaming.borrow_mut();
            let Some(s) = current.as_mut().filter(|s| s.file.id == file.id) else {
                return false;
            };
            if let Some(partial) = &file.partial {
                s.file = partial.clone();
                s.ranges.update(partial);
                s.loaded.add(partial);
            }
            let next = if s.file.completed {
                None
            } else {
                s.ranges.advance(s.target(), s.file.size)
            };
            let (start, end) = s.shown();
            self.viewer.set_download_start(start);
            self.viewer.set_download_end(end);
            (
                s.file.clone(),
                s.token,
                next.map(|window| download(file.id, window)),
            )
        };
        self.video(
            w,
            VideoRequest::Availability {
                file: update.0,
                token: update.1,
            },
        );
        self.send_stream(w, update.2);
        true
    }
    pub(super) fn stream_playback(&self, w: &AppWindow, state: VideoState) -> bool {
        let mut current = self.streaming.borrow_mut();
        if let Some(token) = state.stream_token {
            let Some(s) = current.as_mut().filter(|s| s.token == token) else {
                return false;
            };
            if state.duration_ms > 0 {
                s.duration_ms = u64::from(state.duration_ms);
            }
            s.position_ms = u64::from(state.position_ms);
            let next = if s.file.completed {
                None
            } else {
                s.ranges.advance(s.target(), s.file.size)
            };
            let next = next.map(|window| download(s.file.id, window));
            s.engine_waiting = state.buffering;
            self.viewer.set_loading(s.waiting || s.engine_waiting);
            drop(current);
            self.send_stream(w, next);
        } else if current.is_some() {
            return false;
        }
        true
    }
}
