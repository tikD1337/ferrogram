//! Message actions retain IDs only; the open history remains the sole message cache.
use super::*;
use slint::ModelNotify;
use std::collections::HashSet;
use tg_core::{ChatId, Message, Reply};

#[derive(Clone)]
struct Target {
    chat: ChatId,
    ids: Vec<MessageId>, // One album, at most ten.
}
enum Compose {
    Reply(Target),
    Edit(Target, String), // Restore the bounded pre-edit draft on cancel/save.
}
pub(super) struct Actions {
    pub destinations: Rc<models::Destinations>,
    target: RefCell<Option<Target>>,
    compose: RefCell<Option<Compose>>,
    properties: RefCell<HashSet<(ChatId, MessageId)>>,
    replies: RefCell<HashSet<(ChatId, MessageId)>>,
    highlight_timer: slint::Timer,
    pending_edit: Cell<Option<(ChatId, MessageId)>>,
}
impl Actions {
    pub fn new(chats: Rc<Chats>) -> Self {
        Self {
            destinations: Rc::new(models::Destinations {
                chats,
                query: RefCell::default(),
                notify: ModelNotify::default(),
            }),
            target: RefCell::default(),
            compose: RefCell::default(),
            properties: RefCell::default(),
            replies: RefCell::default(),
            highlight_timer: slint::Timer::default(),
            pending_edit: Cell::new(None),
        }
    }
}
impl State {
    pub(super) fn query_properties(&self, w: &AppWindow) {
        if !w.get_ready() {
            return;
        }
        let ids = self.feed.visible_ids();
        let requests = {
            let h = self.feed.history.borrow();
            let mut pending = self.actions.properties.borrow_mut();
            let mut replies = self.actions.replies.borrow_mut();
            let mut requests = Vec::new();
            for m in h
                .messages
                .iter()
                .filter(|m| ids.contains(&m.id) && m.id.0 > 0)
            {
                if pending.len() < 8 && m.permissions.is_none() && pending.insert((m.chat, m.id)) {
                    requests.push(Command::GetMessageProperties {
                        chat: m.chat,
                        message: m.id,
                    });
                }
                if replies.len() < 8
                    && m.reply.as_ref().is_some_and(|r| !r.resolved)
                    && replies.insert((m.chat, m.id))
                {
                    requests.push(Command::GetRepliedMessage {
                        chat: m.chat,
                        message: m.id,
                    });
                }
            }
            requests
        };
        for request in requests {
            // A local capacity error is surfaced; do not spin retrying while idle.
            let (chat, id, replied) = match &request {
                Command::GetMessageProperties { chat, message } => (*chat, *message, false),
                Command::GetRepliedMessage { chat, message } => (*chat, *message, true),
                _ => continue,
            };
            if !self.send(w, request) {
                if replied {
                    self.actions.replies.borrow_mut().remove(&(chat, id));
                    if let Some(m) = self
                        .feed
                        .history
                        .borrow_mut()
                        .messages
                        .iter_mut()
                        .find(|m| m.chat == chat && m.id == id)
                        && let Some(r) = &mut m.reply
                    {
                        r.resolved = true;
                    }
                } else {
                    self.actions.properties.borrow_mut().remove(&(chat, id));
                    if let Some(m) = self
                        .feed
                        .history
                        .borrow_mut()
                        .messages
                        .iter_mut()
                        .find(|m| m.chat == chat && m.id == id)
                    {
                        m.permissions = Some(Default::default());
                    }
                }
            }
        }
    }
    pub(super) fn cancel_action(&self, w: &AppWindow) {
        self.clear_file_confirmation();
        w.set_action_dialog(0);
        self.actions.pending_edit.set(None);
        w.set_edit_saving(false);
        self.actions.target.borrow_mut().take();
        if let Some(Compose::Edit(_, draft)) = self.actions.compose.borrow_mut().take() {
            w.set_draft(draft.into());
        }
        w.set_composer_title("".into());
        w.set_composer_preview("".into());
        w.set_editing(false);
    }
    fn target_message(&self, target: &Target) -> Option<Message> {
        self.feed
            .history
            .borrow()
            .messages
            .iter()
            .find(|m| m.chat == target.chat && target.ids.first() == Some(&m.id))
            .cloned()
    }
    pub(super) fn message_action(&self, w: &AppWindow, key: &str, action: i32) {
        if action == 6 {
            self.attachment_click(w, key, 2, 0.0);
            return;
        }
        let Ok(key) = key.parse::<i64>() else { return };
        let ids = self.feed.action_members(MessageId(key));
        let Some(chat) = self.feed.history.borrow().chat else {
            return;
        };
        let target = Target { chat, ids };
        let Some(message) = self.target_message(&target) else {
            return;
        };
        let permissions = message.permissions.unwrap_or_default();
        match action {
            0 if permissions.reply => {
                self.cancel_action(w);
                w.set_composer_title(
                    format!(
                        "Reply to {}",
                        message.sender.as_deref().unwrap_or(if message.outgoing {
                            "You"
                        } else {
                            "Message"
                        })
                    )
                    .into(),
                );
                w.set_composer_preview(preview(&message.text).into());
                *self.actions.compose.borrow_mut() = Some(Compose::Reply(target));
                w.invoke_focus_composer();
            }
            1 if target.ids.len() == 1
                && message.outgoing
                && message.is_text
                && permissions.edit =>
            {
                self.cancel_action(w);
                let draft = w.get_draft();
                if draft.len() > 64 * 1024 {
                    w.set_error("Draft exceeds the 64 KiB text limit.".into());
                    return;
                }
                let draft = draft.to_string();
                w.set_composer_title("Edit message".into());
                w.set_composer_preview(preview(&message.text).into());
                w.set_draft(message.text.into());
                w.set_editing(true);
                *self.actions.compose.borrow_mut() = Some(Compose::Edit(target, draft));
                w.invoke_focus_composer();
            }
            2 => {
                // One album caption. Never concatenate retained histories into clipboard storage.
                let h = self.feed.history.borrow();
                let text = h
                    .messages
                    .iter()
                    .filter(|m| target.ids.contains(&m.id))
                    .find(|m| !m.text.is_empty())
                    .map_or("", |m| m.text.as_str());
                w.invoke_copy_text(text.into());
            }
            3 | 4 => {
                let h = self.feed.history.borrow();
                let allowed = target.ids.iter().all(|id| {
                    h.messages
                        .iter()
                        .find(|m| m.id == *id)
                        .and_then(|m| m.permissions)
                        .is_some_and(|p| {
                            if action == 3 {
                                p.forward
                            } else {
                                p.delete_self || p.delete_all
                            }
                        })
                });
                let revoke = target.ids.iter().all(|id| {
                    h.messages
                        .iter()
                        .find(|m| m.id == *id)
                        .and_then(|m| m.permissions)
                        .is_some_and(|p| p.delete_all)
                });
                let self_delete = target.ids.iter().all(|id| {
                    h.messages
                        .iter()
                        .find(|m| m.id == *id)
                        .and_then(|m| m.permissions)
                        .is_some_and(|p| p.delete_self)
                });
                drop(h);
                if !allowed || (action == 4 && !self_delete && !revoke) {
                    return;
                }
                *self.actions.target.borrow_mut() = Some(target);
                w.set_revoke(revoke && !self_delete);
                w.set_can_revoke(revoke);
                w.set_must_revoke(revoke && !self_delete);
                w.set_forward_query("".into());
                self.actions.destinations.notify.reset();
                w.set_action_dialog(if action == 4 { 1 } else { 2 });
                self.sync_media(w);
            }
            5 => {
                if let Some(reply) = message.reply {
                    self.open_quote(w, reply);
                }
            }
            _ => {}
        }
    }
    pub(super) fn open_quote(&self, w: &AppWindow, reply: Reply) {
        if reply.chat.0 == 0 || reply.message.0 <= 0 {
            w.set_error("The original message is unavailable.".into());
            return;
        }
        if self.chats.selected.get() != Some(reply.chat) {
            let index = self
                .chats
                .rows
                .borrow()
                .iter()
                .position(|c| c.id == reply.chat);
            let Some(index) = index else {
                w.set_error("The original chat is not loaded.".into());
                return;
            };
            self.open(w, index);
        }
        let loaded = self
            .feed
            .history
            .borrow()
            .messages
            .iter()
            .any(|m| m.id == reply.message);
        if !loaded {
            if !self.send(
                w,
                Command::OpenChatAt {
                    chat: reply.chat,
                    anchor: reply.message,
                },
            ) {
                return;
            }
            self.feed
                .history
                .borrow_mut()
                .open_around(reply.chat, reply.message);
        }
        self.feed.jump_target.set(Some(reply.message));
        self.feed.unread_after.set(None);
        self.feed.jump_unread.set(false);
        self.seek_start.set(false);
        w.set_follow_tail(false);
        self.feed.highlight.set(Some(reply.message));
        self.feed.invalidate();
        self.actions.highlight_timer.stop();
        if loaded {
            self.refresh(w);
        }
    }
    pub(super) fn start_highlight(&self, w: &AppWindow) {
        let weak = w.as_weak();
        self.actions.highlight_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(2),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_clear_highlight();
                }
            },
        );
    }
    pub(super) fn finish_edit(&self, w: &AppWindow, chat: ChatId, id: MessageId) {
        if self.actions.pending_edit.get() == Some((chat, id)) {
            self.cancel_action(w);
        }
    }
    pub(super) fn edit_failed(&self, w: &AppWindow) {
        self.actions.pending_edit.set(None);
        w.set_edit_saving(false);
    }
    pub(super) fn send_composer(&self, w: &AppWindow) {
        if self.actions.pending_edit.get().is_some() {
            return;
        }
        let text = w.get_draft().to_string();
        if text.len() > 64 * 1024 {
            w.set_error("Message exceeds the 64 KiB text limit.".into());
            return;
        }
        if text.trim().is_empty() {
            return;
        }
        let Some(chat) = self.feed.history.borrow().chat else {
            return;
        };
        let command = match self.actions.compose.borrow().as_ref() {
            Some(Compose::Reply(t)) if t.chat == chat => Command::ReplyText {
                chat,
                reply: t.ids[0],
                text,
            },
            Some(Compose::Edit(t, _)) if t.chat == chat => Command::EditText {
                chat,
                message: t.ids[0],
                text,
            },
            _ => Command::SendText { chat, text },
        };
        if self.send(w, command) {
            if w.get_editing() {
                if let Some(Compose::Edit(t, _)) = self.actions.compose.borrow().as_ref() {
                    self.actions.pending_edit.set(Some((t.chat, t.ids[0])));
                    w.set_edit_saving(true);
                }
            } else {
                self.cancel_action(w);
                w.set_draft("".into());
            }
        }
    }
    pub(super) fn action_event(&self, w: &AppWindow, event: Event) {
        match event {
            Event::MessageProperties {
                chat,
                message,
                permissions,
            } => {
                self.actions
                    .properties
                    .borrow_mut()
                    .remove(&(chat, message));
                if let Some(m) = self
                    .feed
                    .history
                    .borrow_mut()
                    .messages
                    .iter_mut()
                    .find(|m| m.chat == chat && m.id == message)
                {
                    m.permissions = Some(permissions);
                }
                self.feed.invalidate_message(message);
            }
            Event::ReplyResolved {
                chat,
                message,
                reply,
            } => {
                self.actions.replies.borrow_mut().remove(&(chat, message));
                if let Some(m) = self
                    .feed
                    .history
                    .borrow_mut()
                    .messages
                    .iter_mut()
                    .find(|m| m.chat == chat && m.id == message)
                {
                    if let Some(reply) = reply {
                        m.reply = Some(reply)
                    } else if let Some(r) = &mut m.reply {
                        r.resolved = true;
                    }
                }
                self.feed.invalidate_message(message);
            }
            Event::MessageContentChanged {
                chat,
                message,
                text,
                entities,
                photo,
                attachment,
                is_text,
            } => {
                if let Some(m) = self
                    .feed
                    .history
                    .borrow_mut()
                    .messages
                    .iter_mut()
                    .find(|m| m.chat == chat && m.id == message)
                {
                    m.text = text;
                    m.entities = entities;
                    m.photo = photo;
                    m.attachment = attachment;
                    m.is_text = is_text;
                    m.permissions = None;
                }
                self.feed.invalidate_message(message);
                self.finish_edit(w, chat, message);
            }
            Event::MessageEdited {
                chat,
                message,
                edited,
            } => {
                if let Some(m) = self
                    .feed
                    .history
                    .borrow_mut()
                    .messages
                    .iter_mut()
                    .find(|m| m.chat == chat && m.id == message)
                {
                    m.edited = edited;
                }
                self.feed.invalidate_message(message);
            }
            Event::MessagesDeleted { chat, messages } => {
                self.round_removed(w, chat, &messages);
                // Geometry still contains the old row positions until refresh restores its anchor.
                self.feed.history.borrow_mut().remove(chat, &messages);
                for id in &messages {
                    self.feed.invalidate_message(*id);
                }
                let cancel = match self.actions.compose.borrow().as_ref() {
                    Some(Compose::Reply(t) | Compose::Edit(t, _)) => {
                        t.chat == chat && t.ids.iter().any(|id| messages.contains(id))
                    }
                    None => false,
                };
                if cancel {
                    self.cancel_action(w);
                }
            }
            _ => return,
        }
        self.refresh(w);
    }
}
pub(super) fn preview(text: &str) -> String {
    let mut s = text.replace(['\r', '\n'], " ");
    let mut end = s.len().min(256);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s
}
pub(super) fn bind(window: &AppWindow, state: &Rc<State>) {
    window.set_destinations(ModelRc::from(state.actions.destinations.clone()));
    let s = state.clone();
    let weak = window.as_weak();
    window.on_message_action(move |key, action| {
        if let Some(w) = weak.upgrade() {
            s.message_action(&w, &key, action);
        }
    });
    let s = state.clone();
    let weak = window.as_weak();
    window.on_cancel_action(move || {
        if let Some(w) = weak.upgrade() {
            s.cancel_action(&w);
        }
    });
    let s = state.clone();
    let weak = window.as_weak();
    window.on_filter_destinations(move |query| {
        *s.actions.destinations.query.borrow_mut() = query.chars().take(256).collect();
        s.actions.destinations.notify.reset();
        if let Some(w) = weak.upgrade() {
            w.set_destination_y(0.0);
            s.sync_media(&w);
        }
    });
    let s = state.clone();
    let weak = window.as_weak();
    window.on_confirm_delete(move || {
        if let Some(w) = weak.upgrade()
            && let Some(t) = s.actions.target.borrow().clone()
            && s.send(
                &w,
                Command::DeleteMessages {
                    chat: t.chat,
                    messages: t.ids,
                    revoke: w.get_can_revoke() && (w.get_revoke() || w.get_must_revoke()),
                },
            )
        {
            w.set_action_dialog(0);
        }
    });
    let s = state.clone();
    let weak = window.as_weak();
    window.on_forward_to(move |index| {
        if let Some(w) = weak.upgrade()
            && let Some(i) = s.actions.destinations.index(index.max(0) as usize)
            && let Some(to) = s.chats.rows.borrow().get(i).map(|c| c.id)
            && let Some(t) = s.actions.target.borrow().clone()
            && s.send(
                &w,
                Command::ForwardMessages {
                    from: t.chat,
                    to,
                    messages: t.ids,
                },
            )
        {
            w.set_action_dialog(0);
        }
    });
    let s = state.clone();
    let weak = window.as_weak();
    window.on_clear_highlight(move || {
        if let Some(w) = weak.upgrade() {
            s.feed.highlight.set(None);
            s.feed.invalidate();
            s.refresh(&w);
        }
    });
}
