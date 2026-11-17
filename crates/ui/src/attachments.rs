//! Voice/document interaction state. No native API or file I/O on the UI thread.
use super::*;
use slint::{ModelRc, VecModel};
use tg_core::{Attachment, ChatId, FileRequest, MediaFile, Message, VideoState};
type FileHandler = Box<dyn Fn(FileRequest) -> Result<(), CommandError>>;
#[derive(Default)]
pub(super) struct Attachments {
    voice: RefCell<Option<VoicePlayback>>,
    next_token: Cell<u64>,
    confirm: RefCell<Option<PathBuf>>,
    pub files: RefCell<Option<FileHandler>>,
}
struct VoicePlayback {
    token: u64,
    chat: ChatId,
    message: MessageId,
    file: MediaFile,
    seek: Option<f64>,
    opened: bool,
    round: bool,
    placed: Option<(VideoRect, VideoRect)>,
}
pub(super) fn decorate(
    row: &mut MessageRow,
    message: &Message,
    audio: Option<(MessageId, VideoState)>,
) {
    match &message.attachment {
        Some(Attachment::Round(round)) => {
            let state = audio.filter(|(id, _)| *id == message.id).map(|(_, s)| s);
            row.voice_playing = state.is_some_and(|s| s.playing);
            row.voice_unlistened = !round.viewed;
            row.voice_time = state
                .map_or_else(
                    || clock::duration(round.duration),
                    |s| {
                        format!(
                            "{} / {}",
                            clock::duration(s.position_ms / 1000),
                            clock::duration(round.duration)
                        )
                    },
                )
                .into();
            row.file_downloading = round.file.downloading;
            let fraction = if round.file.size == 0 {
                0.02
            } else {
                (round.file.downloaded as f64 / round.file.size as f64).clamp(0.02, 0.999)
            };
            let angle = fraction * std::f64::consts::TAU;
            row.round_progress = format!(
                "M50 2 A48 48 0 {} 1 {:.3} {:.3}",
                i32::from(fraction > 0.5),
                50.0 + 48.0 * angle.sin(),
                50.0 - 48.0 * angle.cos()
            )
            .into();
            row.file_progress = if round.file.downloading {
                percent(&round.file)
            } else {
                String::new()
            }
            .into();
        }
        Some(Attachment::Voice(voice)) => {
            row.voice = true;
            row.voice_unlistened = !voice.listened;
            let state = audio.filter(|(id, _)| *id == message.id).map(|(_, s)| s);
            row.voice_playing = state.is_some_and(|s| s.playing);
            row.voice_time = state
                .map_or_else(
                    || clock::duration(voice.duration),
                    |s| {
                        format!(
                            "{} / {}",
                            clock::duration(s.position_ms / 1000),
                            clock::duration(s.duration_ms / 1000)
                        )
                    },
                )
                .into();
            let progress =
                state.map_or(0.0, |s| s.position_ms as f32 / s.duration_ms.max(1) as f32);
            row.waveform = ModelRc::new(VecModel::from(
                tg_core::waveform_bars(&voice.waveform)
                    .iter()
                    .enumerate()
                    .map(|(i, s)| generated::WaveBar {
                        amplitude: 2.0 + f32::from(*s) * 0.8,
                        played: (i as f32 / 48.0) < progress,
                    })
                    .collect::<Vec<_>>(),
            ));
            row.file_progress = if voice.file.downloading {
                percent(&voice.file)
            } else {
                String::new()
            }
            .into();
        }
        Some(Attachment::Document(document)) => {
            row.document = true;
            row.file_name = document.name.as_str().into();
            row.file_size = size(document.file.size).into();
            row.file_progress = if document.file.downloading {
                percent(&document.file)
            } else {
                String::new()
            }
            .into();
            row.file_available = document.file.path.is_some();
            row.file_downloading = document.file.downloading;
        }
        _ => {}
    }
}
fn percent(file: &MediaFile) -> String {
    if file.size == 0 {
        "...".into()
    } else {
        format!(
            "{}%",
            file.downloaded
                .saturating_mul(100)
                .checked_div(file.size)
                .unwrap_or(0)
                .min(100)
        )
    }
}
fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
impl State {
    pub(super) fn stop_voice(&self, w: &AppWindow) {
        let current = self.attachments.voice.borrow_mut().take();
        if let Some(current) = current {
            self.video(w, VideoRequest::Close);
            if current.file.path.is_none() {
                self.set_download_state(current.message, false);
                self.send(w, Command::CancelDownload(current.file.id));
            }
            self.feed.audio.set(None);
            self.feed.notify.reset();
        }
    }
    pub(super) fn voice_state(&self, w: &AppWindow, state: VideoState) {
        let mut current = self.attachments.voice.borrow_mut();
        let Some(voice) = current.as_mut() else {
            return;
        };
        if state.audio_token != Some(voice.token) {
            return;
        }
        let (chat, id) = (voice.chat, voice.message);
        let seek = if state.duration_ms > 0 {
            voice.seek.take()
        } else {
            None
        };
        let mark_listened = state.ended && !voice.opened;
        if mark_listened {
            voice.opened = true;
        }
        drop(current);
        self.feed.audio.set(Some((id, state)));
        self.feed.notify.reset();
        if let Some(fraction) = seek {
            self.video(w, VideoRequest::Seek(fraction));
        }
        if mark_listened {
            self.send(w, Command::OpenMessageContent { chat, message: id });
        }
        if state.failed {
            w.set_error("This message cannot be played.".into());
            self.stop_voice(w);
        }
    }
    pub(super) fn demo_play_voice(&self, w: &AppWindow, message: &Message) {
        self.play_voice(w, message, None);
    }
    fn play_voice(&self, w: &AppWindow, message: &Message, seek: Option<f64>) {
        let (file, duration, round) = match &message.attachment {
            Some(Attachment::Voice(v)) => (&v.file, v.duration, false),
            Some(Attachment::Round(v)) => (&v.file, v.duration, true),
            _ => return,
        };
        let same = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .is_some_and(|v| v.chat == message.chat && v.message == message.id);
        if same && file.path.is_none() {
            self.stop_voice(w);
            return;
        }
        if same {
            if self.feed.audio.get().is_some_and(|(_, s)| s.ended)
                && let Some(active) = self.attachments.voice.borrow_mut().as_mut()
            {
                active.opened = false;
            }
            self.video(
                w,
                seek.map_or(VideoRequest::TogglePause, VideoRequest::Seek),
            );
            return;
        }
        self.close_video(w);
        self.stop_voice(w);
        let token = self.attachments.next_token.get().wrapping_add(1);
        self.attachments.next_token.set(token);
        *self.attachments.voice.borrow_mut() = Some(VoicePlayback {
            token,
            chat: message.chat,
            message: message.id,
            file: file.clone(),
            seek,
            opened: false,
            round,
            placed: None,
        });
        self.feed.audio.set(Some((
            message.id,
            VideoState {
                duration_ms: duration.saturating_mul(1000),
                ..Default::default()
            },
        )));
        if let Some(path) = &file.path {
            self.start_attachment_player(w, path.clone(), token, round);
        } else if !self.send(w, Command::DownloadFileUrgent(file.id)) {
            self.stop_voice(w);
        } else {
            self.set_download_state(message.id, true);
        }
        self.feed.notify.reset();
    }
    pub(super) fn attachment_click(&self, w: &AppWindow, key: &str, action: i32, fraction: f32) {
        let Ok(id) = key.parse::<i64>().map(MessageId) else {
            return;
        };
        let message = self
            .feed
            .history
            .borrow()
            .messages
            .iter()
            .find(|m| m.id == id)
            .cloned();
        let Some(message) = message else { return };
        match &message.attachment {
            Some(Attachment::Voice(_) | Attachment::Round(_)) => self.play_voice(
                w,
                &message,
                (action == 1).then_some(f64::from(fraction.clamp(0.0, 1.0))),
            ),
            Some(Attachment::Document(document)) => {
                if let Some(path) = &document.file.path {
                    if action == 2 {
                        self.file_request(w, FileRequest::ShowInFolder(path.clone()));
                    } else if tg_core::is_executable_name(&document.name)
                        || path
                            .file_name()
                            .is_some_and(|n| tg_core::is_executable_name(&n.to_string_lossy()))
                    {
                        *self.attachments.confirm.borrow_mut() = Some(path.clone());
                        w.set_file_name(document.name.as_str().into());
                        w.set_action_dialog(3);
                    } else {
                        self.file_request(w, FileRequest::Open(path.clone()));
                    }
                } else if document.file.downloading {
                    if self.send(w, Command::CancelDownload(document.file.id)) {
                        self.set_download_state(message.id, false);
                        self.refresh(w);
                    }
                } else if self.send(w, Command::DownloadFileUrgent(document.file.id)) {
                    self.set_download_state(message.id, true);
                    self.refresh(w);
                }
            }
            _ => {}
        }
    }
    fn set_download_state(&self, id: MessageId, downloading: bool) {
        if let Some(message) = self
            .feed
            .history
            .borrow_mut()
            .messages
            .iter_mut()
            .find(|m| m.id == id)
        {
            match &mut message.attachment {
                Some(Attachment::Document(d)) => d.file.downloading = downloading,
                Some(Attachment::Round(v)) => v.file.downloading = downloading,
                Some(Attachment::Voice(v)) => v.file.downloading = downloading,
                _ => {}
            }
        }
        self.feed.invalidate_message(id);
    }
    fn file_request(&self, w: &AppWindow, request: FileRequest) {
        let result = self.attachments.files.borrow().as_ref().map_or_else(
            || {
                Err(CommandError {
                    code: -4,
                    message: "File operations are unavailable.".into(),
                })
            },
            |handler| handler(request),
        );
        if let Err(error) = result {
            w.set_error(error.message.into());
        }
    }
    pub(super) fn download_failed(&self, w: &AppWindow, id: i32) {
        let active = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .is_some_and(|v| v.file.id == id);
        if active {
            self.stop_voice(w);
        }
        for message in &mut self.feed.history.borrow_mut().messages {
            match &mut message.attachment {
                Some(Attachment::Voice(v)) if v.file.id == id => v.file.downloading = false,
                Some(Attachment::Round(v)) if v.file.id == id => v.file.downloading = false,
                Some(Attachment::Document(d)) if d.file.id == id => d.file.downloading = false,
                _ => {}
            }
        }
        self.feed.invalidate();
        self.refresh(w);
        w.set_error("Could not download this file. Click to retry.".into());
    }
    pub(super) fn attachment_file(&self, w: &AppWindow, file: &MediaFile) {
        let mut changed = Vec::new();
        for message in &mut self.feed.history.borrow_mut().messages {
            let target = match &mut message.attachment {
                Some(Attachment::Voice(v)) => Some(&mut v.file),
                Some(Attachment::Round(v)) => Some(&mut v.file),
                Some(Attachment::Document(d)) => Some(&mut d.file),
                _ => None,
            };
            if let Some(target) = target
                && target.id == file.id
                && *target != *file
            {
                *target = file.clone();
                changed.push(message.id);
            }
        }
        let ready = {
            let mut voice = self.attachments.voice.borrow_mut();
            if let Some(voice) = voice.as_mut()
                && voice.file.id == file.id
                && voice.file.path.is_none()
                && file.path.is_some()
            {
                voice.file = file.clone();
                file.path
                    .clone()
                    .map(|path| (path, voice.token, voice.round))
            } else {
                None
            }
        };
        if let Some((path, token, round)) = ready {
            self.start_attachment_player(w, path, token, round);
        }
        if !changed.is_empty() {
            // Progress/path changes do not alter row geometry: keep caption rasters and anchors.
            self.feed.notify.reset();
        }
    }
    fn round_rect(&self, w: &AppWindow, id: MessageId) -> Option<(VideoRect, VideoRect)> {
        use slint::Model;
        let row = (0..self.feed.row_count())
            .filter_map(|i| self.feed.row_data(i))
            .find(|r| r.round && r.key == id.0.to_string())?;
        let x = w.get_feed_left()
            + if row.outgoing {
                w.get_feed_view_width() - row.bubble_width - 14.0
            } else {
                14.0
            };
        let y = w.get_feed_top()
            + row.row_y
            + w.get_feed_y()
            + (w.get_feed_height() - w.get_feed_content_height()).max(0.0)
            + row.bubble_y
            + row.media_y;
        let scale = w.window().scale_factor();
        let rect = VideoRect {
            x: (x * scale).round() as i32,
            y: (y * scale).round() as i32,
            width: (row.media_width * scale).round() as i32,
            height: (row.media_height * scale).round() as i32,
        };
        let viewport = VideoRect {
            x: (w.get_feed_left() * scale).round() as i32,
            y: (w.get_feed_top() * scale).round() as i32,
            width: (w.get_feed_view_width() * scale).round() as i32,
            height: (w.get_feed_height() * scale).round() as i32,
        };
        (rect.y < viewport.y + viewport.height && rect.y + rect.height > viewport.y)
            .then_some((rect, viewport))
    }
    fn start_attachment_player(&self, w: &AppWindow, path: PathBuf, token: u64, round: bool) {
        if !round {
            self.video(w, VideoRequest::OpenAudio { path, token });
            return;
        }
        let id = self.attachments.voice.borrow().as_ref().map(|v| v.message);
        if let Some((rect, viewport)) = id.and_then(|id| self.round_rect(w, id))
            && let Some(window) = native_window(w.window())
        {
            if let Some(active) = self.attachments.voice.borrow_mut().as_mut() {
                active.placed = Some((rect, viewport));
            }
            self.video(
                w,
                VideoRequest::OpenRound {
                    path,
                    window,
                    rect,
                    viewport,
                    token,
                },
            );
        } else {
            self.stop_voice(w);
        }
    }
    pub(super) fn sync_round(&self, w: &AppWindow) {
        let active = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .filter(|v| v.round)
            .map(|v| (v.message, v.placed));
        let Some((id, placed)) = active else {
            return;
        };
        if w.get_menu_open() || w.get_action_dialog() != 0 {
            self.stop_voice(w);
            return;
        }
        let Some((rect, viewport)) = self.round_rect(w, id) else {
            self.stop_voice(w);
            return;
        };
        if placed.is_some() && placed != Some((rect, viewport)) {
            if let Some(v) = self.attachments.voice.borrow_mut().as_mut() {
                v.placed = Some((rect, viewport));
            }
            self.video(w, VideoRequest::RoundViewport(viewport));
            self.video(w, VideoRequest::Move(rect));
        }
    }
    pub(super) fn clear_file_confirmation(&self) {
        self.attachments.confirm.borrow_mut().take();
    }
    pub(super) fn content_opened(&self, w: &AppWindow, chat: ChatId, id: MessageId) {
        if let Some(m) = self
            .feed
            .history
            .borrow_mut()
            .messages
            .iter_mut()
            .find(|m| m.chat == chat && m.id == id)
        {
            match &mut m.attachment {
                Some(Attachment::Voice(v)) => v.listened = true,
                Some(Attachment::Round(v)) => v.viewed = true,
                _ => {}
            }
        }
        self.feed.invalidate_message(id);
        self.refresh(w);
    }
}
pub(super) fn bind(w: &AppWindow, state: &Rc<State>) {
    let weak = w.as_weak();
    let s = state.clone();
    w.on_attachment_action(move |key, action, fraction| {
        if let Some(w) = weak.upgrade() {
            s.attachment_click(&w, &key, action, fraction);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_confirm_file(move || {
        if let Some(w) = weak.upgrade() {
            let path = s.attachments.confirm.borrow_mut().take();
            if let Some(path) = path {
                s.file_request(&w, FileRequest::Open(path));
            }
            w.set_action_dialog(0);
        }
    });
}
