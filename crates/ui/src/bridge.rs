//! Event-driven bounded bridge. One Slint wakeup drains a batch; no timer or polling.
use crate::AppWindow;
use slint::ComponentHandle;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tg_core::Event;
const MAX_EVENTS: usize = 128;
const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    bytes: usize,
    scheduled: bool,
    failed: bool,
    paused: bool,
}
impl Queue {
    fn enqueue(&mut self, event: Event) -> bool {
        if self.failed {
            return false;
        }
        let bytes = event_bytes(&event);
        if self.events.len() == MAX_EVENTS || self.bytes.saturating_add(bytes) > MAX_BYTES {
            self.failed = true;
            self.events.clear();
            self.bytes = 0;
        } else {
            self.bytes += bytes;
            self.events.push_back(event);
        }
        if self.scheduled || self.paused {
            false
        } else {
            self.scheduled = true;
            true
        }
    }
}
#[derive(Clone)]
pub struct EventSink {
    queue: Arc<Mutex<Queue>>,
    window: slint::Weak<AppWindow>,
}
impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventSink(<bounded>)")
    }
}
impl EventSink {
    pub(crate) fn new(window: &AppWindow) -> Self {
        Self {
            queue: Arc::default(),
            window: window.as_weak(),
        }
    }
    /// Never blocks for UI progress. Overflow fails closed instead of dropping incremental updates silently.
    pub fn post(&self, event: Event) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        if !queue.enqueue(event) {
            return;
        }
        drop(queue);
        self.wake();
    }
    /// Hold delivery during engine startup; the same count/byte limits still apply.
    /// Call before starting the engine, so authorization callbacks cannot race handle publication.
    pub fn pause(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.paused = true;
        }
    }
    /// Release queued startup events after the command target has been installed.
    pub fn resume(&self) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        queue.paused = false;
        if queue.scheduled || (queue.events.is_empty() && !queue.failed) {
            return;
        }
        queue.scheduled = true;
        drop(queue);
        self.wake();
    }
    fn wake(&self) {
        // upgrade_in_event_loop uses slint::invoke_from_event_loop internally.
        let _ = self
            .window
            .upgrade_in_event_loop(|window| window.invoke_drain_events());
    }
    pub(crate) fn drain(&self) -> Result<VecDeque<Event>, ()> {
        let mut queue = self.queue.lock().map_err(|_| ())?;
        queue.scheduled = false;
        queue.bytes = 0;
        if queue.failed {
            return Err(());
        }
        Ok(std::mem::take(&mut queue.events))
    }
}
fn message_bytes(m: &tg_core::Message) -> usize {
    m.photo.as_ref().map_or(0, photo_bytes)
        + 256
        + match &m.send_state {
            tg_core::SendState::Failed(error) => error.message.capacity(),
            _ => 0,
        }
        + m.reply
            .as_ref()
            .map_or(0, |r| r.sender.capacity() + r.preview.capacity() + 128)
        + m.forwarded.as_ref().map_or(0, String::capacity)
        + m.text.capacity()
        + m.entities.capacity() * std::mem::size_of::<tg_core::TextEntity>()
        + m.entities
            .iter()
            .map(|e| match &e.kind {
                tg_core::EntityKind::TextUrl { target } => target.capacity(),
                _ => 0,
            })
            .sum::<usize>()
}
fn event_bytes(e: &Event) -> usize {
    256 + match e {
        Event::MessagesAroundLoaded { messages, .. }
        | Event::MessagesLoaded { messages, .. }
        | Event::NewerMessagesLoaded { messages, .. } => messages.iter().map(message_bytes).sum(),
        Event::IncomingMessage(m)
        | Event::NewMessage(m)
        | Event::MessageUpdated { message: m, .. } => message_bytes(m),
        Event::ChatListChanged(
            tg_core::ChatListChange::Insert { chat, .. }
            | tg_core::ChatListChange::Update { chat, .. },
        ) => {
            chat.avatar.as_ref().map_or(0, photo_bytes)
                + chat.title.capacity()
                + chat.last_message.as_ref().map_or(0, |m| m.text.capacity())
        }
        Event::AuthStateChanged(tg_core::AuthState::WaitOtherDevice { link }) => link.capacity(),
        Event::AuthStateChanged(tg_core::AuthState::WaitPassword { hint }) => hint.capacity(),
        Event::FileUpdated(file) => file.path.as_ref().map_or(0, |p| p.as_os_str().len() * 2),
        Event::CommandFailed { error, .. } => error.message.capacity(),
        Event::ReplyResolved { reply, .. } => reply
            .as_ref()
            .map_or(0, |r| r.sender.capacity() + r.preview.capacity() + 128),
        Event::MessagesDeleted { messages, .. } => {
            messages.capacity() * std::mem::size_of::<tg_core::MessageId>()
        }
        Event::MessageContentChanged {
            text,
            entities,
            photo,
            ..
        } => {
            text.capacity()
                + entities.capacity() * std::mem::size_of::<tg_core::TextEntity>()
                + entities
                    .iter()
                    .map(|e| match &e.kind {
                        tg_core::EntityKind::TextUrl { target } => target.capacity(),
                        _ => 0,
                    })
                    .sum::<usize>()
                + photo.as_ref().map_or(0, photo_bytes)
        }
        _ => 0,
    }
}

fn photo_bytes(p: &tg_core::Photo) -> usize {
    p.minithumbnail.capacity() + p.file.path.as_ref().map_or(0, |p| p.as_os_str().len() * 2) + 128
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_overflow_fails_closed_with_one_wakeup() {
        let mut queue = Queue::default();
        assert!(queue.enqueue(Event::ChatsLoaded { has_more: true }));
        for _ in 1..MAX_EVENTS {
            assert!(!queue.enqueue(Event::ChatsLoaded { has_more: true }));
        }
        assert_eq!(queue.events.len(), MAX_EVENTS);
        assert!(!queue.enqueue(Event::ChatsLoaded { has_more: true }));
        assert!(queue.failed && queue.events.is_empty());
        assert!(!queue.enqueue(Event::ChatsLoaded { has_more: false }));
    }
    #[test]
    fn startup_hold_does_not_schedule_early_authentication() {
        let mut queue = Queue {
            paused: true,
            ..Default::default()
        };
        assert!(!queue.enqueue(Event::AuthStateChanged(tg_core::AuthState::Ready)));
        assert!(!queue.scheduled);
        assert_eq!(queue.events.len(), 1);
        queue.paused = false;
        assert!(queue.enqueue(Event::ChatsLoaded { has_more: true }));
        assert!(queue.scheduled);
    }
    #[test]
    fn byte_budget_also_bounds_large_events() {
        let mut queue = Queue::default();
        assert!(queue.enqueue(Event::CommandFailed {
            command: tg_core::CommandKind::Initialize,
            error: tg_core::CommandError {
                code: -1,
                message: "x".repeat(MAX_BYTES)
            }
        }));
        assert!(queue.failed && queue.events.is_empty() && queue.bytes == 0);
    }
}
