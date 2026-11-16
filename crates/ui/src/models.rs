use crate::media::{Key, Media};
use crate::{
    ChatRow, MessageRow, PhotoCell, clock, history::History, text::TextEngine, viewport::Geometry,
};
use slint::{
    Color, Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SharedPixelBuffer,
    VecModel,
};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
    rc::Rc,
};
use tg_core::{Chat, ChatId, ChatListChange, Message, MessageId, SendState};

/// Chat-list row height and avatar size in logical pixels; app.slint uses the same values.
pub(crate) const CHAT_ROW: f32 = 62.0;
pub(crate) const CHAT_AVATAR: f32 = 46.0;
/// Telegram's seven profile colours, used for avatar backgrounds and sender names.
const PALETTE: [(u8, u8, u8); 7] = [
    (0xe1, 0x70, 0x76),
    (0xfa, 0xa7, 0x74),
    (0xa6, 0x95, 0xe7),
    (0x7b, 0xc8, 0x62),
    (0x6e, 0xc9, 0xcb),
    (0x65, 0xaa, 0xdd),
    (0xee, 0x7a, 0xae),
];
pub(crate) fn peer_color(id: i64) -> Color {
    let (r, g, b) = PALETTE[id.rem_euclid(PALETTE.len() as i64) as usize];
    Color::from_rgb_u8(r, g, b)
}
/// Up to two initials from the first two words, as Telegram shows them.
pub(crate) fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .take(2)
        .collect()
}

pub(crate) struct Chats {
    // Matches the engine's 2,048-row bound: titles <=1 KiB, previews <=512 bytes.
    pub rows: RefCell<Vec<Chat>>,
    pub selected: Cell<Option<ChatId>>,
    pub notify: ModelNotify,
    pub media: RefCell<Option<Rc<RefCell<Media>>>>,
    pub scale: Cell<f32>,
}
impl Default for Chats {
    fn default() -> Self {
        Self {
            rows: RefCell::new(Vec::new()),
            selected: Cell::new(None),
            notify: ModelNotify::default(),
            media: RefCell::default(),
            scale: Cell::new(1.0),
        }
    }
}
impl Chats {
    pub fn apply(&self, change: ChatListChange) {
        // Notifications may call row_data immediately; never hold a RefCell borrow there.
        let mut rows = self.rows.borrow_mut();
        match change {
            ChatListChange::Insert { position, chat }
                if position <= rows.len() && rows.len() < 2048 =>
            {
                rows.insert(position, chat);
                drop(rows);
                self.notify.row_added(position, 1);
            }
            ChatListChange::Update { position, chat } if position < rows.len() => {
                rows[position] = chat;
                drop(rows);
                self.notify.row_changed(position);
            }
            ChatListChange::Move { from, to } if from < rows.len() && to < rows.len() => {
                let row = rows.remove(from);
                rows.insert(to, row);
                drop(rows);
                self.notify.reset();
            }
            ChatListChange::Remove { position } if position < rows.len() => {
                rows.remove(position);
                drop(rows);
                self.notify.row_removed(position, 1);
            }
            _ => {}
        }
    }
}
impl Model for Chats {
    type Data = ChatRow;
    fn row_count(&self) -> usize {
        self.rows.borrow().len()
    }
    fn row_data(&self, row: usize) -> Option<ChatRow> {
        let rows = self.rows.borrow();
        let chat = rows.get(row)?;
        let avatar = chat
            .avatar
            .as_ref()
            .and_then(|p| {
                self.media.borrow().as_ref().map(|m| {
                    m.borrow_mut()
                        .image(Key::new(p, CHAT_AVATAR, CHAT_AVATAR, self.scale.get()))
                })
            })
            .unwrap_or_default();
        Some(ChatRow {
            avatar,
            mentions: if chat.unread_mentions == 0 {
                String::new()
            } else {
                chat.unread_mentions.to_string()
            }
            .into(),
            muted: chat.is_muted(crate::unix_now()),
            initials: initials(&chat.title).into(),
            color: peer_color(chat.id.0),
            time: chat
                .last_message
                .as_ref()
                .map_or_else(String::new, |m| clock::list_label(m.date))
                .into(),
            title: chat.title.as_str().into(),
            preview: chat
                .last_message
                .as_ref()
                .map_or("", |m| m.text.as_str())
                .into(),
            unread: if chat.unread_count == 0 {
                String::new()
            } else {
                chat.unread_count.to_string()
            }
            .into(),
            selected: self.selected.get() == Some(chat.id),
        })
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
// Logical layout constants of a message row, in pixels.
const PAD: f32 = 10.0;
const PAD_TOP: f32 = 6.0;
const PAD_BOTTOM: f32 = 6.0;
const SENDER: f32 = 20.0;
const FOOTER: f32 = 15.0;
const DATE: f32 = 40.0;
const GAP: f32 = 8.0;
const GAP_RUN: f32 = 2.0;
const TIME_HEIGHT: f32 = 14.0;
/// Telegram allows at most 10 items in one album.
const MAX_ALBUM: usize = 10;

/// Consecutive messages with one non-zero album ID form one row, at most MAX_ALBUM each.
pub(crate) fn groups(messages: &[Message]) -> Vec<Range<usize>> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    for (i, message) in messages.iter().enumerate() {
        match groups.last_mut() {
            Some(last)
                if message.album != 0
                    && messages[last.start].album == message.album
                    && last.len() < MAX_ALBUM =>
            {
                last.end = i + 1;
            }
            _ => groups.push(i..i + 1),
        }
    }
    groups
}

/// Everything around a row that changes how it looks; a cached row is reused only if equal.
#[derive(Clone, PartialEq, Debug)]
struct Context {
    complete: bool,
    members: Vec<MessageId>,
    date: bool,
    continued: bool,
    tail: bool,
    last: bool,
    read: bool,
    unread: bool,
}

struct Cached {
    key: MessageId,
    context: Context,
    row: MessageRow,
    /// Cell size per album member, for image keys: (member, width, height).
    cells: Vec<(MessageId, f32, f32)>,
}

pub(crate) struct Feed {
    pub history: RefCell<History>,
    pub engine: RefCell<TextEngine>,
    pub width: Cell<f32>,
    pub scale: Cell<f32>,
    pub notify: ModelNotify,
    pub geometry: RefCell<Geometry>,
    /// Outgoing messages up to this ID are read in the open chat.
    pub read_outbox: Cell<MessageId>,
    pub unread_after: Cell<Option<MessageId>>,
    pub jump_unread: Cell<bool>,
    pub jump_target: Cell<Option<MessageId>>,
    pub highlight: Cell<Option<MessageId>>,
    // Only viewport rows own raster images, at most 64 rows and 96 MiB in total.
    visible: RefCell<Vec<Cached>>,
    pub media: RefCell<Option<Rc<RefCell<Media>>>>,
}
impl Feed {
    pub fn new(font: Option<Vec<u8>>) -> Rc<Self> {
        Rc::new(Self {
            media: RefCell::default(),
            history: RefCell::new(History::default()),
            geometry: RefCell::default(),
            visible: RefCell::default(),
            engine: RefCell::new(TextEngine::new(font)),
            width: Cell::new(504.0),
            scale: Cell::new(1.0),
            read_outbox: Cell::new(MessageId(0)),
            unread_after: Cell::new(None),
            jump_unread: Cell::new(false),
            jump_target: Cell::new(None),
            highlight: Cell::new(None),
            notify: ModelNotify::default(),
        })
    }
    pub fn invalidate(&self) {
        self.visible.borrow_mut().clear();
    }
    pub fn invalidate_message(&self, id: MessageId) {
        self.visible
            .borrow_mut()
            .retain(|row| !row.context.members.contains(&id));
    }
    fn context(&self, messages: &[Message], groups: &[Range<usize>], i: usize) -> Context {
        let group = &messages[groups[i].clone()];
        let (first, last) = (&group[0], &group[group.len() - 1]);
        let previous = i.checked_sub(1).map(|j| &messages[groups[j].end - 1]);
        let next = groups.get(i + 1).map(|range| &messages[range.start]);
        let day = clock::day(first.date);
        let date = previous.is_none_or(|p| clock::day(p.date) != day);
        // One sender within ten minutes on one day is drawn as a run of bubbles.
        let run = |a: &Message, b: &Message| {
            a.outgoing == b.outgoing
                && a.sender_id == b.sender_id
                && (b.date - a.date).abs() < 600
                && clock::day(a.date) == clock::day(b.date)
        };
        let unread = self
            .unread_after
            .get()
            .and_then(|id| messages.iter().position(|m| !m.outgoing && m.id > id))
            .is_some_and(|index| groups[i].contains(&index));
        Context {
            complete: self.complete_album(messages, &groups[i]),
            unread,
            members: group.iter().map(|m| m.id).collect(),
            date,
            continued: !date && !unread && previous.is_some_and(|p| run(p, first)),
            tail: next.is_none_or(|n| !run(last, n)),
            last: next.is_none(),
            read: last.outgoing && last.id <= self.read_outbox.get(),
        }
    }
    pub fn layout(&self, top: f32, height: f32, tail: bool) -> (f32, f32) {
        let history = self.history.borrow();
        let groups = groups(&history.messages);
        // Rows are keyed by their newest member: it stays when older album parts arrive.
        let keys: Vec<MessageId> = groups
            .iter()
            .map(|range| history.messages[range.end - 1].id)
            .collect();
        let resolve = |id: MessageId| {
            history
                .messages
                .iter()
                .position(|m| m.id == id)
                .and_then(|index| groups.iter().position(|range| range.contains(&index)))
                .map_or(id, |group| keys[group])
        };
        let mut geometry = self.geometry.borrow_mut();
        let anchor = geometry
            .anchor(top)
            .map(|(id, offset)| (resolve(id), offset));
        geometry.sync(keys.iter().copied());
        let jump = if self.jump_unread.get() {
            self.unread_after
                .get()
                .and_then(|read| {
                    history
                        .messages
                        .iter()
                        .position(|m| !m.outgoing && m.id > read)
                })
                .and_then(|index| groups.iter().position(|range| range.contains(&index)))
                .map(|index| keys[index])
        } else {
            None
        };
        // Other queued updates may refresh an empty history while the around page is in flight.
        let target = self
            .jump_target
            .get()
            .filter(|id| history.messages.iter().any(|m| m.id == *id));
        let jump = target
            .map(|id| {
                self.jump_target.set(None);
                resolve(id)
            })
            .or(jump);
        let mut top = if let Some(key) = jump {
            self.jump_unread.set(false);
            geometry.restore(Some((key, 0.0)))
        } else if tail {
            (geometry.total() - height).max(0.0)
        } else {
            geometry.restore(anchor)
        };
        let mut cache = std::mem::take(&mut *self.visible.borrow_mut());
        // Refine one viewport row per pass, discarding images that move off screen.
        // At most 64 passes/rows; each pass keeps the same ID + intra-row anchor.
        for _ in 0..64 {
            let anchor = geometry.anchor(top);
            let mut y = 0.0;
            let mut rendered = false;
            // At most 64 transient viewport indices, no text/layout storage.
            let indices: Vec<usize> = geometry
                .rows
                .iter()
                .enumerate()
                .filter_map(|(i, row)| {
                    let visible = y + row.1 > top && y < top + height;
                    y += row.1;
                    visible.then_some(i)
                })
                .take(64)
                .collect();
            for i in indices {
                let context = self.context(&history.messages, &groups, i);
                if !cache
                    .iter()
                    .any(|row| row.key == keys[i] && row.context == context)
                {
                    cache.retain(|row| row.key != keys[i]);
                    let (row, cells) = self.render(&history.messages[groups[i].clone()], &context);
                    rendered = true;
                    geometry.rows[i].1 = row.height;
                    cache.push(Cached {
                        key: keys[i],
                        context,
                        row,
                        cells,
                    });
                    break;
                }
            }
            top = if tail {
                (geometry.total() - height).max(0.0)
            } else {
                geometry.restore(anchor)
            };
            // Cache has at most the previous 64 viewport rows plus one new row.
            cache.retain(|cached| {
                let mut y = 0.0;
                for row in &geometry.rows {
                    if row.0 == cached.key {
                        return y + row.1 > top && y < top + height;
                    }
                    y += row.1;
                }
                false
            });
            if !rendered {
                break;
            }
        }
        let mut y = 0.0;
        let mut bytes = 0_usize;
        let mut visible = self.visible.borrow_mut();
        let scale = self.scale.get();
        for &(id, h) in &geometry.rows {
            if y + h > top
                && y < top + height
                && visible.len() < 64
                && let Some(index) = cache.iter().position(|row| row.key == id)
            {
                let mut cached = cache.swap_remove(index);
                let cost =
                    (cached.row.text_width * scale * cached.row.text_height * scale * 4.0) as usize;
                if bytes.saturating_add(cost) <= 96 * 1024 * 1024 {
                    bytes += cost;
                    cached.row.row_y = y;
                    visible.push(cached);
                }
            }
            y += h;
        }
        drop(visible);
        let total = geometry.total();
        drop(geometry);
        drop(history);
        self.notify.reset();
        (top.min((total - height).max(0.0)), total)
    }
    fn render(
        &self,
        group: &[Message],
        context: &Context,
    ) -> (MessageRow, Vec<(MessageId, f32, f32)>) {
        let scale = self.scale.get().clamp(0.5, 4.0);
        let max_width = self.width.get().clamp(80.0, 504.0);
        let (first, last) = (&group[0], &group[group.len() - 1]);
        let media = group.len() > 1 || first.photo.is_some() || first.video.is_some();
        // An album shows one caption: the first photo caption that is not empty.
        let caption = if media {
            group
                .iter()
                .find(|m| (m.photo.is_some() || m.video.is_some()) && !m.text.is_empty())
        } else {
            Some(first)
        };
        let sizes: Vec<(f32, f32)> = if media {
            group
                .iter()
                .map(|m| {
                    match (&m.photo, &m.video) {
                        (Some(p), _) => (f32::from(p.width), f32::from(p.height)),
                        // A video without a preview frame: a 16:9 cell with a play button.
                        (None, Some(_)) => (16.0, 9.0),
                        (None, None) => (4.0, 3.0),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let (frames, media_width, media_height) = album_layout(&sizes, max_width);
        let sender = if context.continued || first.outgoing {
            None
        } else {
            first.sender.as_deref()
        };
        let sender_height = if sender.is_some() { SENDER } else { 0.0 };
        let checks = match last.send_state {
            SendState::Pending => -1,
            SendState::Failed(_) => -2,
            SendState::Sent if last.outgoing => {
                if context.read {
                    2
                } else {
                    1
                }
            }
            SendState::Sent => 0,
        };
        let label_width = if last.edited { 72.0 } else { 32.0 };
        let time_width = label_width + if last.outgoing { 18.0 } else { 0.0 };
        let forward_height = if first.forwarded.is_some() { 22.0 } else { 0.0 };
        let quote_height = if first.reply.is_some() { 44.0 } else { 0.0 };
        let header_height = forward_height + quote_height;
        let available = if media { media_width } else { max_width } - 2.0 * PAD;
        let text = caption.map(|message| {
            self.engine
                .borrow_mut()
                .render(message, scale, available.max(1.0) * scale)
        });
        let (text_width, text_height, last_line, clipped) =
            text.as_ref().map_or((0.0, 0.0, 0.0, false), |t| {
                (
                    t.width as f32 / scale,
                    t.height as f32 / scale,
                    t.last_line as f32 / scale,
                    t.clipped,
                )
            });
        let media_y = if media && (sender.is_some() || header_height > 0.0) {
            PAD_TOP + sender_height + header_height
        } else {
            0.0
        };
        let text_y = if media {
            media_y + media_height + 6.0
        } else {
            PAD_TOP + sender_height + header_height
        };
        // As in Telegram: the time sits on the last text line when it fits, else on its own.
        let inline = last_line + 8.0 + time_width;
        let (content_width, footer) = if text.is_none() {
            (0.0, false)
        } else if inline <= text_width {
            (text_width, false)
        } else if inline <= available {
            (inline, false)
        } else {
            (text_width.max(time_width), true)
        };
        let mut bubble_width = if media {
            media_width
        } else {
            content_width + 2.0 * PAD
        };
        if let Some(name) = sender {
            let estimate = name.chars().count() as f32 * 8.0 + 2.0 * PAD;
            bubble_width = bubble_width.max(estimate.min(max_width));
        }
        if header_height > 0.0 {
            bubble_width = bubble_width.max(220.0_f32.min(max_width));
        }
        let bubble_height = if text.is_some() {
            text_y + text_height + if footer { FOOTER } else { 0.0 } + PAD_BOTTOM
        } else {
            media_y + media_height
        };
        let over_media = text.is_none();
        let (time_x, time_y) = if over_media {
            (
                bubble_width - time_width - 10.0,
                bubble_height - TIME_HEIGHT - 8.0,
            )
        } else {
            (
                bubble_width - PAD - time_width,
                bubble_height - PAD_BOTTOM - TIME_HEIGHT,
            )
        };
        let bubble_y = if context.date { DATE } else { 0.0 }
            + if context.unread { 32.0 } else { 0.0 }
            + if context.continued { GAP_RUN } else { GAP };
        let cells: Vec<(MessageId, f32, f32)> = group
            .iter()
            .zip(&frames)
            .map(|(m, f)| (m.id, f[2], f[3]))
            .collect();
        let cell_rows: Vec<PhotoCell> = group
            .iter()
            .zip(&frames)
            .map(|(m, f)| PhotoCell {
                image: Image::default(),
                label: if m.photo.is_some() || m.video.is_some() {
                    Default::default()
                } else {
                    m.text.as_str().into()
                },
                x: f[0],
                y: f[1],
                width: f[2],
                height: f[3],
                video: m.video.is_some(),
                duration: m
                    .video
                    .as_ref()
                    .map_or_else(String::new, |v| clock::duration(v.duration))
                    .into(),
            })
            .collect();
        let rendered = text.map_or_else(Image::default, |t| {
            Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                &t.pixels, t.width, t.height,
            ))
        });
        let mut time = clock::time(last.date);
        if last.edited {
            time.insert_str(0, "edited ");
        }
        if clipped {
            time.insert_str(0, "clipped  ");
        }
        let permissions = first.permissions.unwrap_or_default();
        let row = MessageRow {
            key: last.id.0.to_string().into(),
            can_edit: group.len() == 1 && first.outgoing && first.is_text && permissions.edit,
            can_reply: context.complete && permissions.reply,
            can_delete: context.complete
                && (group
                    .iter()
                    .all(|m| m.permissions.is_some_and(|p| p.delete_self))
                    || group
                        .iter()
                        .all(|m| m.permissions.is_some_and(|p| p.delete_all))),
            can_forward: context.complete
                && group
                    .iter()
                    .all(|m| m.permissions.is_some_and(|p| p.forward)),
            highlighted: group.iter().any(|m| self.highlight.get() == Some(m.id)),
            forwarded: first
                .forwarded
                .as_ref()
                .map(|n| format!("Forwarded from {n}"))
                .unwrap_or_default()
                .into(),
            sender_y: PAD_TOP + forward_height,
            quote_y: PAD_TOP + forward_height + sender_height,
            quote_sender: first
                .reply
                .as_ref()
                .map_or("", |r| r.sender.as_str())
                .into(),
            quote_preview: first
                .reply
                .as_ref()
                .map_or("", |r| r.preview.as_str())
                .into(),
            quote_color: peer_color(first.reply.as_ref().map_or(0, |r| r.sender_id)),
            has_quote: first.reply.is_some(),
            time_label_width: label_width,
            row_y: 0.0,
            unread: context.unread,
            unread_y: if context.date { DATE } else { 0.0 },
            height: bubble_y + bubble_height + if context.last { 10.0 } else { 0.0 },
            date: if context.date {
                clock::day_label(first.date).into()
            } else {
                Default::default()
            },
            bubble_y,
            bubble_width,
            bubble_height,
            outgoing: first.outgoing,
            tail: context.tail,
            sender: sender.unwrap_or_default().into(),
            sender_color: peer_color(first.sender_id),
            media_y,
            media_width,
            media_height,
            media_caption: media && !over_media,
            cells: ModelRc::new(VecModel::from(cell_rows)),
            rendered,
            text_y,
            text_width,
            text_height,
            time: time.into(),
            time_x,
            time_y,
            time_over_media: over_media,
            checks,
        };
        (row, cells)
    }
    fn complete_album(&self, messages: &[Message], range: &Range<usize>) -> bool {
        if messages[range.start].album == 0 || range.len() == MAX_ALBUM {
            return true;
        }
        let h = self.history.borrow();
        let older_known = range.start > 0
            || (h.older.is_none() && h.pending_older.is_none() && h.pending_around.is_none());
        let newer_known = range.end < messages.len()
            || (h.newer.is_none() && h.pending_around.is_none() && !h.latest_requested);
        older_known && newer_known
    }
    /// Edge albums wait for the existing prefetch to establish both neighboring boundaries.
    pub fn action_members(&self, key: MessageId) -> Vec<MessageId> {
        let h = self.history.borrow();
        groups(&h.messages)
            .into_iter()
            .find(|r| h.messages[r.end - 1].id == key && self.complete_album(&h.messages, r))
            .map(|r| h.messages[r].iter().map(|m| m.id).collect())
            .unwrap_or_default()
    }
    pub fn visible_ids(&self) -> Vec<MessageId> {
        self.visible
            .borrow()
            .iter()
            .flat_map(|r| r.context.members.iter().copied())
            .take(300)
            .collect()
    }
    /// Uses exact bubble/cell bounds, never the prefetch demand set or date/unread separators.
    pub fn visible_incoming(&self, top: f32, height: f32) -> Vec<MessageId> {
        let history = self.history.borrow();
        let visible = self.visible.borrow();
        let mut ids = Vec::new();
        for cached in visible.iter() {
            let row = &cached.row;
            for (index, id) in cached.context.members.iter().enumerate() {
                let Some(message) = history.messages.iter().find(|m| m.id == *id && !m.outgoing)
                else {
                    continue;
                };
                let base = row.row_y + row.bubble_y;
                let (y, h) = row
                    .cells
                    .row_data(index)
                    .map_or((base, row.bubble_height), |cell| {
                        (base + row.media_y + cell.y, cell.height)
                    });
                if y + h > top && y < top + height {
                    ids.push(message.id);
                }
            }
        }
        // At most 300 message IDs, bounded by the open history (albums included).
        ids
    }
    fn photo(&self, id: MessageId) -> Option<tg_core::Photo> {
        self.history
            .borrow()
            .messages
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.photo.clone())
    }
    /// Demo measurements: the newest video in the loaded window.
    pub fn newest_video(&self) -> Option<tg_core::Video> {
        let history = self.history.borrow();
        history.messages.iter().rev().find_map(|m| m.video.clone())
    }
    /// The video behind a clicked cell of a visible row.
    pub fn video_at(&self, row: usize, cell: usize) -> Option<tg_core::Video> {
        let id = self.visible.borrow().get(row)?.cells.get(cell)?.0;
        self.history
            .borrow()
            .messages
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.video.clone())
    }
    pub fn media_requests(&self) -> Vec<(Key, tg_core::Photo)> {
        let scale = self.scale.get();
        self.visible
            .borrow()
            .iter()
            .flat_map(|row| row.cells.iter())
            .filter_map(|&(id, width, height)| {
                let photo = self.photo(id)?;
                Some((Key::new(&photo, width, height, scale), photo))
            })
            .take(64)
            .collect()
    }
}
impl Model for Feed {
    type Data = MessageRow;
    fn row_count(&self) -> usize {
        self.visible.borrow().len()
    }
    fn row_data(&self, row: usize) -> Option<MessageRow> {
        let visible = self.visible.borrow();
        let cached = visible.get(row)?;
        let mut row = cached.row.clone();
        if let Some(media) = self.media.borrow().as_ref() {
            let cells: Vec<PhotoCell> = cached
                .row
                .cells
                .iter()
                .zip(&cached.cells)
                .map(|(mut cell, &(id, width, height))| {
                    if let Some(photo) = self.photo(id) {
                        cell.image = media.borrow_mut().image(Key::new(
                            &photo,
                            width,
                            height,
                            self.scale.get(),
                        ));
                    }
                    cell
                })
                .collect();
            row.cells = ModelRc::new(VecModel::from(cells));
        }
        Some(row)
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

fn single(&(width, height): &(f32, f32), max_width: f32) -> (f32, f32) {
    let fit = max_width.min(width.max(1.0));
    let shown_height = (height * fit / width.max(1.0)).clamp(1.0, 360.0);
    let shown_width = (shown_height * width / height.max(1.0))
        .min(fit)
        .max(160.0_f32.min(max_width));
    (shown_width.round(), shown_height.round())
}

/// Album grid in rows, as in Telegram: cells fill the width and are centre-cropped.
/// Returns (x, y, width, height) per item and the total size.
pub(crate) fn album_layout(sizes: &[(f32, f32)], max_width: f32) -> (Vec<[f32; 4]>, f32, f32) {
    const SPACING: f32 = 2.0;
    let max_width = max_width.clamp(80.0, 504.0);
    let ratio = |&(w, h): &(f32, f32)| (w / h.max(1.0)).clamp(0.4, 3.0);
    match sizes {
        [] => (Vec::new(), 0.0, 0.0),
        [one] => {
            let (w, h) = single(one, max_width);
            (vec![[0.0, 0.0, w, h]], w, h)
        }
        _ => {
            let width = max_width.min(440.0);
            let rows: &[usize] = match sizes.len() {
                2 if sizes.iter().all(|s| ratio(s) > 1.2) => &[1, 1],
                2 => &[2],
                3 => &[1, 2],
                4 => &[1, 3],
                5 => &[2, 3],
                6 => &[3, 3],
                7 => &[2, 2, 3],
                8 => &[2, 3, 3],
                9 => &[3, 3, 3],
                _ => &[3, 3, 4],
            };
            let mut cells = Vec::with_capacity(sizes.len());
            let (mut y, mut start) = (0.0, 0);
            for &count in rows {
                let end = (start + count).min(sizes.len());
                if start >= end {
                    break;
                }
                let row = &sizes[start..end];
                let sum: f32 = row.iter().map(ratio).sum();
                let inner = width - SPACING * (row.len() - 1) as f32;
                // A lone wide item would dominate the album, so its row is kept lower.
                let (low, high) = if row.len() == 1 {
                    (120.0, 260.0)
                } else {
                    (70.0, 300.0)
                };
                let height = (inner / sum).clamp(low, high).round();
                let mut x = 0.0;
                for (k, size) in row.iter().enumerate() {
                    let w = if k + 1 == row.len() {
                        width - x
                    } else {
                        (inner * ratio(size) / sum).round()
                    };
                    cells.push([x, y, w, height]);
                    x += w + SPACING;
                }
                y += height + SPACING;
                start = end;
            }
            (cells, width, y - SPACING)
        }
    }
}

#[cfg(test)]
mod feed_tests {
    use super::*;
    use tg_core::{Message, MessageId};
    fn message(n: i64) -> Message {
        Message {
            reply: None,
            forwarded: None,
            edited: false,
            is_text: true,
            permissions: None,
            video: None,
            photo: (n % 4 == 0).then(|| tg_core::Photo {
                file: tg_core::MediaFile {
                    id: n as i32,
                    path: None,
                },
                width: 1008,
                height: 640,
                minithumbnail: Vec::new(),
            }),
            id: MessageId(n * 1048576),
            chat: ChatId(1),
            date: n as i32,
            outgoing: n % 2 == 0,
            text: if n % 7 == 0 {
                "Variable height message with enough text to wrap. ".repeat(12)
            } else {
                "Short message".into()
            },
            entities: vec![],
            send_state: SendState::Sent,
            album: 0,
            sender_id: 0,
            sender: None,
        }
    }
    #[test]
    fn first_unread_album_keeps_separator_and_receipts_exclude_overscan_and_outgoing() {
        let feed = Feed::new(None);
        feed.history
            .borrow_mut()
            .open_around(ChatId(1), MessageId(19980 * 1048576));
        feed.history.borrow_mut().around(
            ChatId(1),
            (19955..=20004).map(message).collect(),
            MessageId(19980 * 1048576),
        );
        feed.unread_after.set(Some(MessageId(19980 * 1048576)));
        feed.jump_unread.set(true);
        let (top, _) = feed.layout(0.0, 200.0, false);
        assert!(top > 0.0);
        assert!(feed.visible.borrow().iter().any(|row| row.row.unread));
        let ids = feed.visible_incoming(top, 200.0);
        assert!(!ids.is_empty());
        assert!(ids.iter().all(|id| (id.0 / 1048576) % 2 == 1));
        assert!(!ids.contains(&MessageId(19955 * 1048576)));
        assert!(!ids.contains(&MessageId(20003 * 1048576)));
        // Existing album rows stay grouped when the unread boundary splits an album.
        let mut album: Vec<_> = (10..=13).map(|n| album_item(n, 77)).collect();
        for m in &mut album {
            m.outgoing = false;
        }
        feed.history
            .borrow_mut()
            .open_around(ChatId(1), MessageId(11 * 1048576));
        feed.history
            .borrow_mut()
            .around(ChatId(1), album, MessageId(11 * 1048576));
        feed.unread_after.set(Some(MessageId(11 * 1048576)));
        feed.jump_unread.set(true);
        feed.invalidate();
        feed.layout(0.0, 1000.0, false);
        assert_eq!(feed.row_count(), 1);
        assert!(feed.visible.borrow()[0].row.unread);
    }
    #[test]
    fn real_text_geometry_preserves_visible_id_and_pixel_offset_when_window_slides() {
        let feed = Feed::new(None);
        let mut h = feed.history.borrow_mut();
        h.open(ChatId(1));
        h.page(
            ChatId(1),
            MessageId(0),
            Some(MessageId(951 * 1048576)),
            (951..=1000).map(message).collect(),
            false,
        );
        for end in [950, 900, 850, 800, 750] {
            let cursor = h.request_older().unwrap();
            h.page(
                ChatId(1),
                cursor,
                Some(MessageId((end - 49) * 1048576)),
                (end - 49..=end).map(message).collect(),
                false,
            );
        }
        drop(h);
        let (top, _) = feed.layout(1000.0, 530.0, false);
        let anchor = feed.geometry.borrow().anchor(top).unwrap();
        assert!(feed.row_count() > 0 && feed.row_count() < 20);
        assert!(
            feed.visible
                .borrow()
                .iter()
                .all(|cached| cached.row.row_y + cached.row.height > top
                    && cached.row.row_y < top + 530.0)
        );
        let mut h = feed.history.borrow_mut();
        let cursor = h.request_older().unwrap();
        h.page(
            ChatId(1),
            cursor,
            Some(MessageId(651 * 1048576)),
            (651..=700).map(message).collect(),
            false,
        );
        drop(h);
        let (new_top, _) = feed.layout(top, 530.0, false);
        assert_eq!(feed.geometry.borrow().anchor(new_top), Some(anchor));
        assert_eq!(feed.history.borrow().messages.len(), 300);
        // Move to the opposite edge and load a page that removes the oldest 50 rows.
        let total = feed.geometry.borrow().total();
        let (top, _) = feed.layout(total - 900.0, 530.0, false);
        let anchor = feed.geometry.borrow().anchor(top).unwrap();
        let mut h = feed.history.borrow_mut();
        let cursor = h.request_newer().unwrap();
        h.page(
            ChatId(1),
            cursor,
            Some(MessageId(1000 * 1048576)),
            (951..=1000).map(message).collect(),
            true,
        );
        drop(h);
        let (new_top, _) = feed.layout(top, 530.0, false);
        assert_eq!(feed.geometry.borrow().anchor(new_top), Some(anchor));
    }

    fn album_item(n: i64, album: i64) -> Message {
        let mut m = message(n);
        m.album = album;
        m.photo = Some(tg_core::Photo {
            file: tg_core::MediaFile {
                id: n as i32,
                path: None,
            },
            width: 800,
            height: 600,
            minithumbnail: Vec::new(),
        });
        m.text = if n == 10 {
            "Caption".into()
        } else {
            String::new()
        };
        m
    }

    #[test]
    fn consecutive_album_items_form_one_bounded_row() {
        let mut messages: Vec<Message> = (1..=3).map(message).collect();
        messages.extend((10..=21).map(|n| album_item(n, 77)));
        messages.push(album_item(22, 78));
        messages.push(message(23));
        let rows = groups(&messages);
        // 3 single rows, 12 items of album 77 split at the 10-item limit, album 78, a message.
        assert_eq!(rows, vec![0..1, 1..2, 2..3, 3..13, 13..15, 15..16, 16..17]);
    }

    #[test]
    fn album_grid_fills_the_width_without_overlap() {
        for count in 2..=10 {
            let sizes = vec![(800.0, 600.0); count];
            let (cells, width, height) = album_layout(&sizes, 504.0);
            assert_eq!(cells.len(), count);
            assert_eq!(width, 440.0);
            for (i, a) in cells.iter().enumerate() {
                assert!(a[0] >= 0.0 && a[0] + a[2] <= width + 0.5 && a[1] + a[3] <= height + 0.5);
                for b in &cells[i + 1..] {
                    let apart = a[0] + a[2] <= b[0] + 0.5
                        || b[0] + b[2] <= a[0] + 0.5
                        || a[1] + a[3] <= b[1] + 0.5
                        || b[1] + b[3] <= a[1] + 0.5;
                    assert!(apart, "{count} items overlap");
                }
            }
        }
        let (cells, width, height) = album_layout(&[(100.0, 1000.0)], 504.0);
        assert_eq!((cells.len(), width, height), (1, 160.0, 360.0));
    }

    #[test]
    fn album_split_across_pages_becomes_one_row_and_keeps_the_anchor() {
        let feed = Feed::new(None);
        let mut h = feed.history.borrow_mut();
        h.open(ChatId(1));
        // The newest page holds the end of the album only.
        let mut page: Vec<Message> = (13..=15).map(|n| album_item(n, 77)).collect();
        page.extend((16..=60).map(message));
        h.page(
            ChatId(1),
            MessageId(0),
            Some(MessageId(13 * 1048576)),
            page,
            false,
        );
        drop(h);
        let (top, total) = feed.layout(0.0, 2000.0, true);
        assert!(total > 0.0);
        let anchor = feed.geometry.borrow().anchor(top).unwrap();
        let rows = feed.geometry.borrow().rows.len();
        let mut h = feed.history.borrow_mut();
        let cursor = h.request_older().unwrap();
        h.page(
            ChatId(1),
            cursor,
            Some(MessageId(1048576)),
            (1..=12)
                .map(|n| {
                    if n >= 10 {
                        album_item(n, 77)
                    } else {
                        message(n)
                    }
                })
                .collect(),
            false,
        );
        drop(h);
        let (new_top, _) = feed.layout(top, 2000.0, false);
        // 9 older single messages arrived; the album gained 3 items but is still one row.
        assert_eq!(feed.geometry.borrow().rows.len(), rows + 9);
        assert_eq!(feed.geometry.borrow().anchor(new_top), Some(anchor));
    }
}

pub(crate) struct Destinations {
    pub chats: Rc<Chats>,
    pub query: RefCell<String>,
    pub notify: ModelNotify,
}
impl Destinations {
    pub fn index(&self, row: usize) -> Option<usize> {
        let query = self.query.borrow().to_lowercase();
        self.chats
            .rows
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.title.to_lowercase().contains(&query))
            .nth(row)
            .map(|(i, _)| i)
    }
}
impl Model for Destinations {
    type Data = ChatRow;
    fn row_count(&self) -> usize {
        let q = self.query.borrow().to_lowercase();
        self.chats
            .rows
            .borrow()
            .iter()
            .filter(|c| c.title.to_lowercase().contains(&q))
            .count()
    }
    fn row_data(&self, row: usize) -> Option<ChatRow> {
        self.chats.row_data(self.index(row)?)
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    fn message(n: i64) -> Message {
        Message {
            video: None,
            reply: None,
            forwarded: None,
            edited: false,
            is_text: false,
            permissions: None,
            id: MessageId(n * 1048576),
            chat: ChatId(7),
            date: 10,
            outgoing: true,
            text: String::new(),
            entities: vec![],
            send_state: SendState::Sent,
            album: 91,
            sender_id: 0,
            sender: None,
            photo: None,
        }
    }
    #[test]
    fn quoted_message_jump_survives_refresh_while_around_page_is_pending() {
        let feed = Feed::new(None);
        let id = MessageId(19995 * 1048576);
        feed.jump_target.set(Some(id));
        feed.layout(0.0, 200.0, false);
        assert_eq!(feed.jump_target.get(), Some(id));
    }
    #[test]
    fn deleting_older_bubbles_keeps_visible_message_and_pixel_offset() {
        let feed = Feed::new(None);
        {
            let mut h = feed.history.borrow_mut();
            h.chat = Some(ChatId(7));
            h.messages = (19901..=20000)
                .map(|n| {
                    let mut m = message(n);
                    m.album = 0;
                    m.is_text = true;
                    m.text = format!("Message {n}");
                    m
                })
                .collect();
        }
        feed.layout(0.0, 200.0, false);
        let requested = feed
            .geometry
            .borrow()
            .restore(Some((MessageId(19950 * 1048576), 13.0)));
        let (top, _) = feed.layout(requested, 200.0, false);
        let anchor = feed.geometry.borrow().anchor(top);
        feed.history.borrow_mut().remove(
            ChatId(7),
            &(19901..19920)
                .map(|n| MessageId(n * 1048576))
                .collect::<Vec<_>>(),
        );
        let (top, _) = feed.layout(top, 200.0, false);
        assert_eq!(feed.geometry.borrow().anchor(top), anchor);
        assert_eq!(feed.history.borrow().messages.len(), 81);
    }
    #[test]
    fn partial_album_at_a_paging_edge_cannot_be_forwarded_or_deleted() {
        let feed = Feed::new(None);
        {
            let mut h = feed.history.borrow_mut();
            h.chat = Some(ChatId(7));
            h.older = Some(MessageId(19990 * 1048576));
            h.messages = (19990..19994).map(message).collect();
        }
        assert!(feed.action_members(MessageId(19993 * 1048576)).is_empty());
        let mut neighbor = message(19989);
        neighbor.album = 0;
        feed.history.borrow_mut().messages.insert(0, neighbor);
        assert_eq!(feed.action_members(MessageId(19993 * 1048576)).len(), 4);
    }
    #[test]
    fn action_targets_cover_every_album_member_and_use_first_for_reply() {
        let feed = Feed::new(None);
        feed.history.borrow_mut().messages = (19990..19994).map(message).collect();
        let ids = feed.action_members(MessageId(19993 * 1048576));
        assert_eq!(
            ids,
            (19990..19994)
                .map(|n| MessageId(n * 1048576))
                .collect::<Vec<_>>()
        );
        assert_eq!(ids.first(), Some(&MessageId(19990 * 1048576)));
    }
}
