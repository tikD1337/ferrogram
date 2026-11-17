//! Round-only placement and interaction. The shared audio controller owns the one player.
use super::*;
use slint::{Image, Model};

pub(super) struct Dock {
    corner: u8,
    drag: Option<Drag>,
    preview: Image,
    state: VideoState,
    shown: bool,
    occluded: bool,
}
struct Drag {
    pointer: (i32, i32),
    origin: VideoRect,
    rect: VideoRect,
    moved: bool,
}
impl Default for Dock {
    fn default() -> Self {
        Self {
            corner: 1,
            drag: None,
            preview: Image::default(),
            state: VideoState::default(),
            shown: true,
            occluded: false,
        }
    }
}
fn floating_rect(view: VideoRect, corner: u8, scale: f32) -> VideoRect {
    let margin = (12.0 * scale).round() as i32;
    let size = ((160.0 * scale).round() as i32)
        .min((view.width - 2 * margin).max(1))
        .min((view.height - 2 * margin).max(1));
    VideoRect {
        x: view.x
            + if corner & 1 != 0 {
                view.width - size - margin
            } else {
                margin
            },
        y: view.y
            + if corner & 2 != 0 {
                view.height - size - margin - (24.0 * scale).round() as i32
            } else {
                margin
            },
        width: size,
        height: size,
    }
}
fn placement(
    inline: Option<(VideoRect, VideoRect)>,
    view: VideoRect,
    dock: &Dock,
    scale: f32,
) -> (VideoRect, VideoRect, bool) {
    if let Some((rect, view)) = inline {
        return (rect, view, false);
    }
    (
        dock.drag
            .as_ref()
            .map_or_else(|| floating_rect(view, dock.corner, scale), |d| d.rect),
        view,
        true,
    )
}
fn nearest_corner(rect: VideoRect, view: VideoRect, scale: f32) -> u8 {
    (0..4)
        .min_by_key(|corner| {
            let candidate = floating_rect(view, *corner, scale);
            (candidate.x - rect.x).abs() + (candidate.y - rect.y).abs()
        })
        .unwrap_or(1)
}
fn dragged_out(rect: VideoRect, view: VideoRect) -> bool {
    let width = (rect.x + rect.width).min(view.x + view.width) - rect.x.max(view.x);
    let height = (rect.y + rect.height).min(view.y + view.height) - rect.y.max(view.y);
    width < rect.width / 2 || height < rect.height / 2
}
impl State {
    pub(crate) fn round_active(&self) -> bool {
        self.attachments
            .voice
            .borrow()
            .as_ref()
            .is_some_and(|v| v.round)
    }
    fn round_viewport(w: &AppWindow) -> VideoRect {
        let scale = w.window().scale_factor();
        VideoRect {
            x: (w.get_feed_left() * scale).round() as i32,
            y: (w.get_feed_top() * scale).round() as i32,
            width: (w.get_feed_view_width() * scale).round() as i32,
            height: (w.get_feed_height() * scale).round() as i32,
        }
    }
    pub(crate) fn clear_round(&self, w: &AppWindow) {
        let (corner, occluded) = {
            let dock = self.attachments.dock.borrow();
            (dock.corner, dock.occluded)
        };
        *self.attachments.dock.borrow_mut() = Dock {
            corner,
            occluded,
            ..Default::default()
        };
        w.set_round_expanded(false);
        w.set_round_floating(false);
        w.set_round_preview(Image::default());
        w.set_round_frame(Image::default());
        w.set_round_key("".into());
    }
    pub(crate) fn round_state(&self, w: &AppWindow, state: VideoState) {
        self.attachments.dock.borrow_mut().state = state;
        self.sync_round(w);
    }
    pub(crate) fn round_occluded(&self, w: &AppWindow, occluded: bool) {
        self.attachments.dock.borrow_mut().occluded = occluded;
        self.sync_round(w);
    }
    pub(crate) fn round_removed(&self, w: &AppWindow, chat: ChatId, ids: &[MessageId]) {
        let removed = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .is_some_and(|v| v.round && v.chat == chat && ids.contains(&v.message));
        if removed {
            self.stop_voice(w);
        }
    }
    fn round_rect(&self, w: &AppWindow, id: MessageId) -> Option<(VideoRect, VideoRect)> {
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
        (viewport.width > 0
            && viewport.height > 0
            && rect.x < viewport.x + viewport.width
            && rect.x + rect.width > viewport.x
            && rect.y < viewport.y + viewport.height
            && rect.y + rect.height > viewport.y)
            .then_some((rect, viewport))
    }
    fn round_placement(
        &self,
        w: &AppWindow,
        chat: ChatId,
        id: MessageId,
    ) -> (VideoRect, VideoRect, bool) {
        let inline = (self.chats.selected.get() == Some(chat))
            .then(|| self.round_rect(w, id))
            .flatten();
        placement(
            inline,
            Self::round_viewport(w),
            &self.attachments.dock.borrow(),
            w.window().scale_factor(),
        )
    }
    pub(crate) fn start_round(&self, w: &AppWindow, path: PathBuf, token: u64) {
        let current = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .map(|v| (v.chat, v.message));
        let Some((chat, id)) = current else {
            return;
        };
        let (rect, viewport, _) = self.round_placement(w, chat, id);
        if rect.width <= 0 || viewport.width <= 0 {
            self.stop_voice(w);
            return;
        }
        if let Some(v) = self.attachments.voice.borrow_mut().as_mut() {
            v.placed = Some((rect, viewport));
        }
        self.video(
            w,
            VideoRequest::OpenRound {
                path,
                size: rect.width as u32,
                token,
            },
        );
        self.round_scale(w, w.get_round_growth());
        w.set_round_expanded(true);
        self.sync_round(w);
    }
    pub(crate) fn round_scale(&self, w: &AppWindow, growth: f32) {
        let target = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .filter(|v| v.round)
            .map(|v| (v.chat, v.message))
            .or_else(|| self.feed.round_scale.get().map(|(chat, id, _)| (chat, id)));
        if let Some((chat, id)) = target {
            if let Some((old_chat, old, _)) = self.feed.round_scale.get()
                && self.chats.selected.get() == Some(old_chat)
            {
                self.feed.invalidate_message(old);
            }
            self.feed.round_scale.set(Some((chat, id, growth)));
            self.feed.invalidate_message(id);
            self.refresh(w);
        }
    }
    pub(crate) fn round_frame(&self, w: &AppWindow, frame: tg_core::RoundFrame) {
        if !self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .is_some_and(|v| v.round && v.token == frame.token)
        {
            return;
        }
        let Ok(pixels) = frame.pixels.lock() else {
            return;
        };
        if pixels.width == 0 || pixels.height == 0 {
            return;
        }
        let image = Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
            &pixels.rgba,
            pixels.width,
            pixels.height,
        ));
        w.set_round_frame(image);
    }
    pub(crate) fn sync_round(&self, w: &AppWindow) {
        let active = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .filter(|v| v.round)
            .map(|v| (v.chat, v.message, v.placed));
        let Some((chat, id, placed)) = active else {
            return;
        };
        if self.chats.selected.get() == Some(chat) {
            let preview = (0..self.feed.row_count())
                .filter_map(|i| self.feed.row_data(i))
                .find(|r| r.round && r.key == id.0.to_string())
                .and_then(|r| r.cells.row_data(0))
                .map(|c| c.image);
            if let Some(preview) = preview
                && preview.size().width > 0
            {
                self.attachments.dock.borrow_mut().preview = preview;
            }
        }
        let (rect, view, floating) = self.round_placement(w, chat, id);
        let shown = !w.get_menu_open()
            && w.get_action_dialog() == 0
            && view.width > 0
            && view.height > 0
            && !self.attachments.dock.borrow().occluded
            && w.window().is_visible();
        let scale = w.window().scale_factor();
        let (preview, state, visibility_changed) = {
            let mut dock = self.attachments.dock.borrow_mut();
            if !floating || !shown {
                dock.drag = None;
            }
            let changed = placed.is_some() && dock.shown != shown;
            dock.shown = shown;
            (dock.preview.clone(), dock.state, changed)
        };
        w.set_round_floating(floating && shown && placed.is_some());
        w.set_round_x(rect.x as f32 / scale);
        w.set_round_y(rect.y as f32 / scale);
        w.set_round_size(rect.width as f32 / scale);
        w.set_round_preview(preview);
        w.set_round_key(if self.chats.selected.get() == Some(chat) {
            id.0.to_string().into()
        } else {
            "".into()
        });
        w.set_round_time(clock::duration(state.position_ms / 1000).into());
        let progress =
            (f64::from(state.position_ms) / f64::from(state.duration_ms.max(1))).clamp(0.0, 0.999);
        let angle = progress * std::f64::consts::TAU;
        w.set_round_arc(
            format!(
                "M50 2 A48 48 0 {} 1 {:.3} {:.3}",
                i32::from(progress > 0.5),
                50.0 + 48.0 * angle.sin(),
                50.0 - 48.0 * angle.cos()
            )
            .into(),
        );
        if placed.is_some() && (visibility_changed || placed != Some((rect, view))) {
            if let Some(v) = self.attachments.voice.borrow_mut().as_mut() {
                v.placed = Some((rect, view));
            }
            self.video(
                w,
                VideoRequest::RoundSurface {
                    size: rect.width.max(1) as u32,
                    visible: shown,
                },
            );
        }
    }
    pub(crate) fn round_pointer(&self, w: &AppWindow, phase: i32, x: f32, y: f32) {
        if !w.get_round_floating() {
            return;
        }
        let scale = w.window().scale_factor();
        let pointer = ((x * scale).round() as i32, (y * scale).round() as i32);
        let view = Self::round_viewport(w);
        let mut dock = self.attachments.dock.borrow_mut();
        if phase == 0 {
            let rect = floating_rect(view, dock.corner, scale);
            dock.drag = Some(Drag {
                pointer,
                origin: rect,
                rect,
                moved: false,
            });
            return;
        }
        if let Some(drag) = &mut dock.drag {
            let delta = (pointer.0 - drag.pointer.0, pointer.1 - drag.pointer.1);
            drag.moved |= delta.0.abs() + delta.1.abs() > (6.0 * scale) as i32;
            if drag.moved {
                drag.rect.x = drag.origin.x + delta.0;
                drag.rect.y = drag.origin.y + delta.1;
            }
        }
        if phase == 2 || phase == 3 {
            let drag = dock.drag.take();
            let Some(drag) = drag else {
                return;
            };
            if phase == 3 {
                drop(dock);
                self.sync_round(w);
                return;
            }
            if drag.moved {
                dock.corner = nearest_corner(drag.rect, view, scale);
                drop(dock);
                if dragged_out(drag.rect, view) {
                    self.stop_voice(w);
                    return;
                }
            } else {
                drop(dock);
                self.video(w, VideoRequest::TogglePause);
            }
        } else {
            drop(dock);
        }
        self.sync_round(w);
    }
    pub(crate) fn return_round(&self, w: &AppWindow) {
        let target = self
            .attachments
            .voice
            .borrow()
            .as_ref()
            .filter(|v| v.round)
            .map(|v| (v.chat, v.message));
        if let Some((chat, message)) = target {
            self.open_quote(
                w,
                tg_core::Reply {
                    chat,
                    message,
                    resolved: true,
                    sender_id: 0,
                    sender: String::new(),
                    preview: String::new(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> VideoRect {
        VideoRect {
            x: 301,
            y: 94,
            width: 739,
            height: 594,
        }
    }
    #[test]
    fn inline_float_return_and_chat_change_keep_one_placement() {
        let dock = Dock::default();
        let view = view();
        let inline = VideoRect {
            x: 315,
            y: 160,
            width: 240,
            height: 240,
        };
        assert_eq!(
            placement(Some((inline, view)), view, &dock, 1.0),
            (inline, view, false)
        );
        let (floating, _, is_float) = placement(None, view, &dock, 1.0);
        assert!(is_float);
        assert_eq!(floating, floating_rect(view, 1, 1.0));
        assert_eq!(placement(Some((inline, view)), view, &dock, 1.0).0, inline);
    }
    #[test]
    fn docking_and_drag_out_respect_dpi_and_feed_bounds() {
        let view = view();
        for corner in 0..4 {
            let rect = floating_rect(view, corner, 1.5);
            assert_eq!(nearest_corner(rect, view, 1.5), corner);
            assert!(!dragged_out(rect, view));
            assert!(rect.x >= view.x && rect.y >= view.y);
        }
        let mut rect = floating_rect(view, 1, 1.0);
        rect.x = view.x + view.width - rect.width / 2 + 1;
        assert!(dragged_out(rect, view));
    }
}
