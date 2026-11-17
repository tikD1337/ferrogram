//! Viewer streaming state. Missing byte ranges, rather than media-time estimates, drive TDLib.
use super::*;
use tg_core::{StreamFile, StreamRanges, StreamStatus, Video, VideoState};
pub(super) struct ViewerStream {
    pub token: u64,
    pub file: StreamFile,
    ranges: StreamRanges,
    waiting: bool,
    engine_waiting: bool,
}
impl State {
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
        let _ = ranges.need(0, file.size);
        *self.streaming.borrow_mut() = Some(ViewerStream {
            token,
            file: file.clone(),
            ranges,
            waiting: true,
            engine_waiting: true,
        });
        if !self.send(
            w,
            Command::DownloadRange {
                file: video.file.id,
                offset: 0,
            },
        ) {
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
                .map(|offset| Command::DownloadRange {
                    file: status.file,
                    offset,
                })
        };
        if let Some(command) = command
            && !self.send(w, command)
        {
            self.close_video(w);
            w.set_error("Could not request video data.".into());
        }
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
            }
            self.viewer.set_download_start(if s.file.completed {
                0.0
            } else {
                s.file.offset as f32 / s.file.size.max(1) as f32
            });
            self.viewer.set_download_end(if s.file.completed {
                1.0
            } else {
                (s.file.offset + s.file.prefix).min(s.file.size) as f32 / s.file.size.max(1) as f32
            });
            (s.file.clone(), s.token)
        };
        self.video(
            w,
            VideoRequest::Availability {
                file: update.0,
                token: update.1,
            },
        );
        true
    }
    pub(super) fn stream_playback(&self, state: VideoState) -> bool {
        let mut current = self.streaming.borrow_mut();
        if let Some(token) = state.stream_token {
            let Some(s) = current.as_mut().filter(|s| s.token == token) else {
                return false;
            };
            s.engine_waiting = state.buffering;
            self.viewer.set_loading(s.waiting || s.engine_waiting);
        } else if current.is_some() {
            return false;
        }
        true
    }
}
