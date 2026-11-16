//! The sole Slint boundary. tg-app sees domain commands/events and plain options only.
mod bridge;
mod clock;
mod history;
mod media;
mod models;
mod text;
mod viewport;
pub use bridge::EventSink;
use models::{CHAT_AVATAR, CHAT_ROW, Chats, Feed};
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, SharedPixelBuffer};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
use tg_core::{
    AuthState, Command, CommandError, CommandKind, Event, MessageId, Notification, ReadBatches,
    Secret,
};
pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
// Slint-generated code contains unwrap/todo and handles without Debug.
// Exceptions apply only to the generated module, never hand-written code.
mod generated {
    #![allow(missing_debug_implementations, clippy::unwrap_used, clippy::todo)]
    slint::include_modules!();
}
use generated::{AppWindow, ChatRow, MessageRow, PhotoCell, Theme};

#[derive(Debug, Default)]
pub struct Options {
    pub oled: bool,
    pub demo: bool,
    pub snapshot: Option<PathBuf>,
    pub demo_scroll: Option<u32>,
    /// Loaded off-thread by tg-app. At most 32 MiB, never bundled.
    pub emoji_font: Option<Vec<u8>>,
}
type NotifyFn = Box<dyn Fn(Notification) -> Result<(), CommandError>>;
struct State {
    media: Rc<RefCell<media::Media>>,
    chats: Rc<Chats>,
    feed: Rc<Feed>,
    command: Box<dyn Fn(Command) -> Result<(), CommandError>>,
    loading_chats: Cell<bool>,
    refreshing: Cell<bool>,
    focused: Cell<bool>,
    reads: RefCell<ReadBatches>,
    notifications: RefCell<NotifyFn>,
    total_unread: Cell<Option<u32>>,
    // Seek the actual oldest edge for an entirely unread chat (TDLib has no oldest sentinel).
    seek_start: Cell<bool>,
}
impl State {
    fn send(&self, window: &AppWindow, command: Command) -> bool {
        if window.get_blocked() {
            return false;
        }
        match (self.command)(command) {
            Ok(()) => {
                window.set_error("".into());
                true
            }
            Err(error) => {
                window.set_error(error.message.into());
                false
            }
        }
    }
    fn refresh(&self, window: &AppWindow) {
        if self.refreshing.replace(true) {
            return;
        }
        let height = window.get_feed_height().max(1.0);
        let (top, total) = self.feed.layout(
            (-window.get_feed_y()).max(0.0),
            height,
            window.get_follow_tail(),
        );
        window.set_feed_content_height(total);
        window.set_feed_y(-top);
        window.set_at_newest(self.feed.history.borrow().at_tail());
        self.refreshing.set(false);
        self.prefetch(window, top, height, total);
        self.sync_media(window);
        self.view_visible(window);
    }
    fn view_visible(&self, window: &AppWindow) {
        if !window.get_ready()
            || !window.get_chat_open()
            || !self.focused.get()
            || self.seek_start.get()
        {
            return;
        }
        let history = self.feed.history.borrow();
        let Some(chat) = history.chat else {
            return;
        };
        let read = self
            .chats
            .rows
            .borrow()
            .iter()
            .find(|c| c.id == chat)
            .map_or(MessageId(0), |c| c.last_read_inbox);
        drop(history);
        let top = (-window.get_feed_y()).max(0.0)
            - (window.get_feed_height() - window.get_feed_content_height()).max(0.0);
        let ids = self.feed.visible_incoming(top, window.get_feed_height());
        let batch = self.reads.borrow_mut().begin(chat, read, ids);
        if let Some(messages) = batch
            && !self.send(window, Command::ViewMessages { chat, messages })
        {
            self.reads.borrow_mut().complete(chat, false);
        }
    }
    fn unread_title(&self, window: &AppWindow) {
        let rows = self.chats.rows.borrow();
        let total = self.total_unread.get().unwrap_or_else(|| {
            rows.iter()
                .fold(0u32, |n, c| n.saturating_add(c.unread_count))
        });
        window.set_window_title(if total == 0 {
            "tg-rust".into()
        } else {
            format!("tg-rust ({total})").into()
        });
        let unread = rows
            .iter()
            .find(|c| Some(c.id) == self.chats.selected.get())
            .map_or(0, |c| c.unread_count);
        window.set_open_unread(
            if unread == 0 {
                String::new()
            } else {
                unread.to_string()
            }
            .into(),
        );
    }
    fn sync_media(&self, window: &AppWindow) {
        let mut wanted = if window.get_ready() {
            self.feed.media_requests()
        } else {
            Vec::new()
        };
        let rows = self.chats.rows.borrow();
        let first = ((-window.get_chat_y()).max(0.0) / CHAT_ROW).floor() as usize;
        let count = (window.get_chat_height().max(1.0) / CHAT_ROW).ceil() as usize + 2;
        self.chats.scale.set(self.feed.scale.get());
        for chat in rows
            .iter()
            .skip(first.saturating_sub(1))
            .take(count.min(31))
        {
            if window.get_ready()
                && let Some(photo) = &chat.avatar
            {
                wanted.push((
                    media::Key::new(photo, CHAT_AVATAR, CHAT_AVATAR, self.feed.scale.get()),
                    photo.clone(),
                ));
            }
        }
        let selected = rows
            .iter()
            .find(|c| window.get_ready() && Some(c.id) == self.chats.selected.get());
        if let Some(photo) = selected.and_then(|c| c.avatar.as_ref()) {
            wanted.push((
                media::Key::new(photo, HEADER_AVATAR, HEADER_AVATAR, self.feed.scale.get()),
                photo.clone(),
            ));
        }
        let commands = self.media.borrow_mut().sync(wanted);
        let avatar = selected
            .and_then(|c| c.avatar.as_ref())
            .map(|p| {
                self.media.borrow_mut().image(media::Key::new(
                    p,
                    HEADER_AVATAR,
                    HEADER_AVATAR,
                    self.feed.scale.get(),
                ))
            })
            .unwrap_or_default();
        window.set_chat_avatar(avatar);
        window.set_chat_initials(
            selected
                .map(|c| models::initials(&c.title))
                .unwrap_or_default()
                .into(),
        );
        if let Some(chat) = selected {
            window.set_chat_color(models::peer_color(chat.id.0));
        }
        drop(rows);
        for command in commands {
            self.send(window, command);
        }
    }
    fn prefetch(&self, window: &AppWindow, top: f32, height: f32, total: f32) {
        let (older, newer, chat) = {
            let mut h = self.feed.history.borrow_mut();
            let Some(chat) = h.chat else {
                return;
            };
            let geometry = self.feed.geometry.borrow();
            let first = geometry
                .rows
                .iter()
                .scan(0.0, |y, row| {
                    *y += row.1;
                    Some(*y)
                })
                .position(|y| y > top)
                .unwrap_or(0);
            // Never evict a visible row, including with both directions outstanding.
            let older_safe = h.messages.len() < 250 || first + 100 < h.messages.len();
            let newer_safe = h.messages.len() < 250 || first >= 100;
            let older = if h.latest_requested
                || self.seek_start.get()
                || (top <= 2.0 * height && older_safe)
            {
                h.request_older()
            } else {
                None
            };
            let newer = if total - top - height <= 2.0 * height && newer_safe {
                h.request_newer()
            } else {
                None
            };
            (older, newer, chat)
        };
        if let Some(before) = older
            && !self.send(window, Command::LoadOlderMessages { chat, before })
        {
            self.feed.history.borrow_mut().pending_older = None;
        }
        if let Some(after) = newer
            && !self.send(window, Command::LoadNewerMessages { chat, after })
        {
            self.feed.history.borrow_mut().pending_newer = None;
        }
    }

    fn open(&self, window: &AppWindow, index: usize) {
        let Some(chat) = self.chats.rows.borrow().get(index).cloned() else {
            return;
        };
        if self.chats.selected.get() == Some(chat.id) {
            return;
        }
        if let Some(old) = self.chats.selected.get() {
            self.send(window, Command::CloseChat(old));
        }
        let unread = chat.unread_count > 0;
        let command = if unread {
            Command::OpenChatAt {
                chat: chat.id,
                anchor: chat.last_read_inbox,
            }
        } else {
            Command::OpenChat(chat.id)
        };
        if !self.send(window, command) {
            return;
        }
        self.seek_start.set(unread && chat.last_read_inbox.0 == 0);
        self.chats.selected.set(Some(chat.id));
        self.chats.notify.reset();
        if unread {
            self.feed.history.borrow_mut().open_around(chat.id);
        } else {
            self.feed.history.borrow_mut().open(chat.id);
        }
        self.feed
            .unread_after
            .set(unread.then_some(chat.last_read_inbox));
        self.feed.jump_unread.set(unread && !self.seek_start.get());
        self.feed.read_outbox.set(chat.last_read_outbox);
        self.feed.invalidate();
        *self.feed.geometry.borrow_mut() = Default::default();
        window.set_follow_tail(!unread);
        window.set_chat_open(true);
        self.refresh(window);
        self.unread_title(window);
        window.set_chat_title(chat.title.into());
        window.set_draft("".into());
    }
    fn event(&self, window: &AppWindow, event: Event) {
        match event {
            Event::NotificationActivated(chat) => {
                let index = self.chats.rows.borrow().iter().position(|c| c.id == chat);
                if let Some(index) = index {
                    self.open(window, index);
                    let _ = window.show();
                    window.window().with_winit_window(|w| {
                        w.set_minimized(false);
                        w.focus_window();
                    });
                }
            }
            Event::TotalUnreadCount(total) => {
                self.total_unread.set(Some(total));
                self.unread_title(window);
            }
            Event::MessagesViewed { chat, success } => {
                self.reads.borrow_mut().complete(chat, success);
                if success {
                    self.view_visible(window);
                } else {
                    window.set_error(
                        "Could not update read state. Scroll or refocus to retry.".into(),
                    );
                }
            }
            Event::MessagesAroundLoaded {
                chat,
                anchor,
                messages,
            } => {
                if self
                    .feed
                    .history
                    .borrow_mut()
                    .around(chat, messages, anchor)
                {
                    self.feed.invalidate();
                    self.refresh(window);
                }
            }
            Event::IncomingMessage(message) => {
                if let Some(chat) = self
                    .chats
                    .rows
                    .borrow()
                    .iter()
                    .find(|c| c.id == message.chat)
                    && let Some(notification) = Notification::incoming(
                        chat,
                        &message,
                        self.chats.selected.get(),
                        self.focused.get(),
                        unix_now(),
                    )
                    && let Err(error) = (self.notifications.borrow())(notification)
                {
                    window.set_error(error.message.into());
                }
                self.event(window, Event::NewMessage(message));
            }
            Event::AuthStateChanged(auth) => {
                window.set_auth_input("".into());
                window.set_error("".into());
                if auth != AuthState::Ready {
                    window.set_ready(false);
                    self.sync_media(window);
                }
                match auth {
                    AuthState::WaitPhoneNumber => {
                        window.set_auth_step(1);
                        window.set_auth_title("Scan to sign in".into());
                        self.send(window, Command::RequestQrSignIn);
                    }
                    AuthState::WaitOtherDevice { link } => {
                        window.set_auth_step(1);
                        window.set_auth_title("Scan to sign in".into());
                        match qr_image(&link) {
                            Ok(image) => window.set_qr(image),
                            Err(()) => {
                                window.set_error("Could not generate the sign-in QR code.".into())
                            }
                        }
                    }
                    AuthState::WaitCode => {
                        window.set_auth_step(3);
                        window.set_auth_title("Enter your verification code".into());
                        window.set_qr(Default::default());
                    }
                    AuthState::WaitPassword { .. } => {
                        window.set_auth_step(4);
                        window.set_auth_title("Two-step verification".into());
                    }
                    AuthState::Ready => {
                        window.set_ready(true);
                        window.set_qr(Default::default());
                        if !self.loading_chats.replace(true)
                            && !self.send(window, Command::LoadMoreChats)
                        {
                            self.loading_chats.set(false);
                        }
                    }
                    AuthState::LoggingOut | AuthState::Closed => {
                        *self.reads.borrow_mut() = ReadBatches::default();
                        self.total_unread.set(None);
                        self.chats.rows.borrow_mut().clear();
                        self.chats.selected.set(None);
                        self.chats.notify.reset();
                        *self.feed.history.borrow_mut() = Default::default();
                        self.feed.invalidate();
                        *self.feed.geometry.borrow_mut() = Default::default();
                        self.feed.notify.reset();
                        window.set_chat_open(false);
                        self.unread_title(window);
                        window.set_draft("".into());
                        window.set_qr(Default::default());
                        window.set_auth_step(0);
                        window.set_auth_title("Telegram session closed".into());
                    }
                    AuthState::Unsupported => {
                        window.set_auth_step(0);
                        window.set_error("This sign-in step is not supported.".into());
                    }
                    _ => {
                        window.set_auth_step(0);
                        window.set_auth_title("Connecting to Telegram...".into());
                    }
                }
            }
            Event::FileUpdated(file) => {
                self.media.borrow_mut().file(file);
            }
            Event::ChatListChanged(change) => {
                self.chats.apply(change);
                self.unread_title(window);
                if window.get_demo() && self.chats.selected.get().is_none() {
                    self.open(window, 0);
                }
                let mut read_changed = false;
                if let Some(id) = self.chats.selected.get()
                    && let Some(chat) = self.chats.rows.borrow().iter().find(|chat| chat.id == id)
                {
                    window.set_chat_title(chat.title.as_str().into());
                    read_changed = self.feed.read_outbox.replace(chat.last_read_outbox)
                        != chat.last_read_outbox;
                }
                if read_changed {
                    // Rows compare their context, so only rows whose read state changed redraw.
                    self.refresh(window);
                }
                self.sync_media(window);
            }
            Event::ChatsLoaded { has_more } => {
                self.loading_chats.set(false);
                window.set_more_chats(has_more);
                if has_more && window.get_chat_near_end() {
                    window.invoke_load_chats();
                }
            }
            Event::MessagesLoaded {
                chat,
                before,
                next_before,
                messages,
            } => {
                let accepted =
                    self.feed
                        .history
                        .borrow_mut()
                        .page(chat, before, next_before, messages, false);
                if accepted {
                    if self.seek_start.get() && next_before.is_none() {
                        self.seek_start.set(false);
                        self.feed.jump_unread.set(true);
                    }
                    if before == MessageId(0) && self.feed.unread_after.get().is_none() {
                        window.set_follow_tail(true);
                        self.feed.invalidate();
                    }
                    self.refresh(window);
                }
            }
            Event::NewerMessagesLoaded {
                chat,
                after,
                next_after,
                messages,
            } => {
                if self
                    .feed
                    .history
                    .borrow_mut()
                    .page(chat, after, next_after, messages, true)
                {
                    if next_after.is_none()
                        && -window.get_feed_y() + window.get_feed_height()
                            >= window.get_feed_content_height() - 2.0
                    {
                        window.set_follow_tail(true);
                    }
                    self.refresh(window);
                }
            }
            Event::NewMessage(message) => {
                self.feed.invalidate_message(message.id);
                self.feed
                    .history
                    .borrow_mut()
                    .upsert(None, message, window.get_follow_tail());
                self.refresh(window);
            }
            Event::MessageUpdated { old_id, message } => {
                self.feed.invalidate_message(old_id);
                self.feed.invalidate_message(message.id);
                self.feed.history.borrow_mut().upsert(
                    Some(old_id),
                    message,
                    window.get_follow_tail(),
                );
                self.refresh(window);
            }
            Event::CommandFailed { command, error } => {
                if command == CommandKind::LoadMoreChats {
                    self.loading_chats.set(false);
                }
                if matches!(
                    command,
                    CommandKind::OpenChat
                        | CommandKind::LoadOlderMessages
                        | CommandKind::LoadNewerMessages
                ) {
                    let mut history = self.feed.history.borrow_mut();
                    if command == CommandKind::LoadNewerMessages {
                        history.pending_newer = None;
                    } else {
                        history.pending_older = None;
                    }
                }
                // Command kind and numeric code carry no user data and make reports useful.
                window.set_error(
                    format!("{} ({command:?}, code {})", error.message, error.code).into(),
                );
            }
            Event::SecurityWarning(_) => window.set_security_warning(true),
            _ => {}
        }
    }
}
/// Header avatar size in logical pixels; app.slint uses the same value.
const HEADER_AVATAR: f32 = 38.0;
fn qr_image(link: &str) -> Result<slint::Image, ()> {
    // QR input bounded by the engine to 4 KiB. QR version 40 caps the matrix at 177?.
    let code = qrcode::QrCode::new(link).map_err(|_| ())?;
    let side = (code.width() + 8) as u32;
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(side, side);
    for y in 0..side {
        for x in 0..side {
            let black = x >= 4
                && y >= 4
                && x < side - 4
                && y < side - 4
                && code[((x - 4) as usize, (y - 4) as usize)] == qrcode::Color::Dark;
            let value = if black { 0 } else { 255 };
            pixels.make_mut_slice()[(y * side + x) as usize] = Rgb8Pixel::new(value, value, value);
        }
    }
    Ok(slint::Image::from_rgb8(pixels))
}
/// Owns the complete UI on its event-loop thread. No Slint types escape this crate.
pub struct Ui {
    window: AppWindow,
    sink: EventSink,
    snapshot: Option<PathBuf>,
    demo_scroll: Option<u32>,
    state: Rc<State>,
}
impl std::fmt::Debug for Ui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ui")
    }
}
impl Ui {
    pub fn new(
        options: Options,
        command: impl Fn(Command) -> Result<(), CommandError> + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if std::env::var_os("SLINT_BACKEND").is_none() {
            slint::BackendSelector::new()
                .backend_name("winit".into())
                .renderer_name("software".into())
                .select()?;
        }
        let window = AppWindow::new()?;
        window.global::<Theme>().set_oled(options.oled);
        window.set_demo(options.demo);
        let chats = Rc::new(Chats::default());
        let feed = Feed::new(options.emoji_font);
        feed.scale.set(window.window().scale_factor());
        window.set_chats(ModelRc::from(chats.clone()));
        window.set_messages(ModelRc::from(feed.clone()));
        let media = Rc::new(RefCell::new(media::Media::new(&window)?));
        *chats.media.borrow_mut() = Some(media.clone());
        *feed.media.borrow_mut() = Some(media.clone());
        let state = Rc::new(State {
            media,
            chats,
            feed,
            command: Box::new(command),
            loading_chats: Cell::new(false),
            refreshing: Cell::new(false),
            focused: Cell::new(false),
            reads: RefCell::default(),
            notifications: RefCell::new(Box::new(|_| Ok(()))),
            total_unread: Cell::new(None),
            seek_start: Cell::new(false),
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_media_ready(move || {
            if let Some(w) = weak.upgrade() {
                s.media.borrow_mut().drain();
                s.chats.notify.reset();
                s.feed.notify.reset();
                s.sync_media(&w);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_chat_viewport_changed(move || {
            if let Some(w) = weak.upgrade() {
                s.sync_media(&w);
            }
        });
        let sink = EventSink::new(&window);
        let weak = window.as_weak();
        let s = state.clone();
        let drain = sink.clone();
        window.on_drain_events(move || {
            if let Some(w) = weak.upgrade() {
                match drain.drain() {
                    Ok(events) => {
                        for event in events {
                            s.event(&w, event);
                        }
                    }
                    Err(()) => {
                        let _ = (s.command)(Command::ShutDown);
                        w.set_blocked(true);
                        w.set_error(
                            "UI event capacity exceeded. Please close and restart the application."
                                .into(),
                        );
                    }
                }
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_width_changed(move || {
            if let Some(w) = weak.upgrade() {
                let width = w.get_feed_width();
                let scale = w.window().scale_factor();
                let changed_width = s.feed.width.replace(width) != width;
                let changed_scale = s.feed.scale.replace(scale) != scale;
                if changed_width || changed_scale {
                    s.feed.invalidate();
                    s.refresh(&w);
                }
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_open_chat(move |index| {
            if let Some(w) = weak.upgrade() {
                s.open(&w, index.max(0) as usize);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_auth_submit(move || {
            if let Some(w) = weak.upgrade() {
                let input = w.get_auth_input().to_string();
                if input.is_empty() || input.len() > 4096 {
                    return;
                }
                let secret = Secret::new(input);
                let command = match w.get_auth_step() {
                    2 => Command::SubmitPhoneNumber(secret),
                    3 => Command::SubmitCode(secret),
                    4 => Command::SubmitPassword(secret),
                    _ => return,
                };
                w.set_auth_input("".into());
                s.send(&w, command);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_request_qr(move || {
            if let Some(w) = weak.upgrade() {
                w.set_auth_step(1);
                s.send(&w, Command::RequestQrSignIn);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_load_chats(move || {
            if let Some(w) = weak.upgrade()
                && s.chats.rows.borrow().len() < 2048
                && !s.loading_chats.replace(true)
                && !s.send(&w, Command::LoadMoreChats)
            {
                s.loading_chats.set(false);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_viewport_changed(move || {
            if let Some(w) = weak.upgrade()
                && !s.refreshing.get()
            {
                let at_bottom =
                    (-w.get_feed_y() + w.get_feed_height()) >= w.get_feed_content_height() - 2.0;
                w.set_follow_tail(at_bottom && s.feed.history.borrow().at_tail());
                s.refresh(&w);
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_latest(move || {
            if let Some(w) = weak.upgrade() {
                s.seek_start.set(false);
                s.feed.jump_unread.set(false);
                if s.feed.history.borrow().at_tail() {
                    w.set_follow_tail(true);
                    s.refresh(&w);
                } else {
                    s.feed.history.borrow_mut().latest_requested = true;
                    s.refresh(&w);
                }
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_send(move || {
            if let Some(w) = weak.upgrade() {
                let chat = s.feed.history.borrow().chat;
                let text = w.get_draft().to_string();
                if text.len() > 64 * 1024 {
                    w.set_error("Message exceeds the 64 KiB text limit.".into());
                    return;
                }
                if let Some(chat) = chat
                    && !text.trim().is_empty()
                    && s.send(&w, Command::SendText { chat, text })
                {
                    w.set_draft("".into());
                }
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_demo_toggle(move || {
            if let Some(w) = weak.upgrade()
                && w.get_demo()
            {
                if w.get_ready() {
                    s.event(&w, Event::AuthStateChanged(AuthState::WaitPhoneNumber));
                } else {
                    s.event(&w, Event::AuthStateChanged(AuthState::Ready));
                }
            }
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.window().on_winit_window_event(move |_, event| {
            if let winit::event::WindowEvent::Focused(focused) = event {
                s.focused.set(*focused);
                if *focused && let Some(w) = weak.upgrade() {
                    s.view_visible(&w);
                }
            }
            slint::winit_030::EventResult::Propagate
        });
        let weak = window.as_weak();
        let s = state.clone();
        window.on_logout(move || {
            if let Some(w) = weak.upgrade() {
                s.send(&w, Command::LogOut);
            }
        });
        Ok(Self {
            window,
            sink,
            snapshot: options.snapshot,
            demo_scroll: options.demo_scroll,
            state,
        })
    }
    pub fn on_notification(
        &self,
        handler: impl Fn(Notification) -> Result<(), CommandError> + 'static,
    ) {
        *self.state.notifications.borrow_mut() = Box::new(handler);
    }
    pub fn event_sink(&self) -> EventSink {
        self.sink.clone()
    }
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let result = Rc::new(RefCell::new(None));
        let snapshot_requested = self.snapshot.is_some();
        let demo_timer = slint::Timer::default();
        if let Some(seconds) = self.demo_scroll {
            let weak = self.window.as_weak();
            let start = std::time::Instant::now();
            let previous = Cell::new(start);
            // Demo-only animation clock; normal operation has no repeating timer.
            demo_timer.start(
                slint::TimerMode::Repeated,
                Duration::from_millis(16),
                move || {
                    let now = std::time::Instant::now();
                    let dt = now.duration_since(previous.replace(now)).as_secs_f32();
                    if let Some(w) = weak.upgrade()
                        && start.elapsed() > Duration::from_millis(500)
                    {
                        w.set_feed_y((w.get_feed_y() + 900.0 * dt).min(0.0));
                    }
                    if start.elapsed() >= Duration::from_secs(u64::from(seconds))
                        && !snapshot_requested
                    {
                        let _ = slint::quit_event_loop();
                    }
                },
            );
        }
        if let Some(path) = self.snapshot {
            let weak = self.window.as_weak();
            let result = result.clone();
            // Single-shot render settling delay, not polling. File encoding/writing runs off-thread.
            slint::Timer::single_shot(
                Duration::from_secs(u64::from(self.demo_scroll.unwrap_or(2))),
                move || {
                    let saved = weak
                        .upgrade()
                        .ok_or_else(|| "Snapshot window was closed".to_owned())
                        .and_then(|w| {
                            // Slint 1.18.1's vector-text cache can leave missing glyphs on the
                            // first render into snapshot pixels (reproduced on the password
                            // screen). Prime that pixel format, then capture the complete frame.
                            drop(
                                w.window()
                                    .take_snapshot()
                                    .map_err(|_| "Could not capture the window".to_owned())?,
                            );
                            let pixels = w
                                .window()
                                .take_snapshot()
                                .map_err(|_| "Could not capture the window".to_owned())?;
                            Ok(std::thread::spawn(move || {
                                image::save_buffer(
                                    path,
                                    pixels.as_bytes(),
                                    pixels.width(),
                                    pixels.height(),
                                    image::ColorType::Rgba8,
                                )
                                .map_err(|_| "Could not save the snapshot".to_owned())
                            }))
                        });
                    *result.borrow_mut() = Some(saved);
                    let _ = slint::quit_event_loop();
                },
            );
        }
        self.window.run()?;
        if let Some(saved) = result.borrow_mut().take() {
            saved?.join().map_err(|_| "Snapshot worker failed")??;
        } else if snapshot_requested {
            return Err("Window closed before snapshot capture".into());
        }
        Ok(())
    }
}
