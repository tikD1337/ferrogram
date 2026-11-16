//! Bounded attention state shared by the frontend and native notification adapter.
use crate::{Chat, ChatId, Message, MessageId};
use std::collections::HashMap;
#[derive(Debug, Default)]
pub struct ReadBatches {
    // At most 128 in-flight chats, one batch of at most 50 IDs each.
    pending: HashMap<ChatId, MessageId>,
    // At most the engine's 2048 chats; scalar acknowledgements only, no history cache.
    confirmed: HashMap<ChatId, MessageId>,
}
impl ReadBatches {
    pub fn begin(
        &mut self,
        chat: ChatId,
        read: MessageId,
        visible: impl IntoIterator<Item = MessageId>,
    ) -> Option<Vec<MessageId>> {
        if self.pending.contains_key(&chat) || self.pending.len() >= 128 {
            return None;
        }
        let read = read.max(self.confirmed.get(&chat).copied().unwrap_or(MessageId(0)));
        let mut ids = Vec::with_capacity(50);
        for id in visible {
            if id > read && id.0 > 0 && !ids.contains(&id) {
                ids.push(id);
            }
            if ids.len() == 50 {
                break;
            }
        }
        let last = ids.iter().max().copied()?;
        self.pending.insert(chat, last);
        Some(ids)
    }
    pub fn complete(&mut self, chat: ChatId, success: bool) {
        if let Some(last) = self.pending.remove(&chat)
            && success
        {
            if !self.confirmed.contains_key(&chat)
                && self.confirmed.len() >= 2048
                && let Some(evict) = self.confirmed.keys().next().copied()
            {
                self.confirmed.remove(&evict);
            }
            self.confirmed
                .entry(chat)
                .and_modify(|id| *id = (*id).max(last))
                .or_insert(last);
        }
    }
}
/// Never format notification content for diagnostics.
#[derive(Clone)]
pub struct Notification {
    pub chat: ChatId,
    pub title: String,
    pub body: String,
}
impl std::fmt::Debug for Notification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Notification(<redacted>)")
    }
}
impl Notification {
    pub fn incoming(
        chat: &Chat,
        message: &Message,
        open: Option<ChatId>,
        focused: bool,
        now: i64,
    ) -> Option<Self> {
        if message.id <= chat.last_read_inbox
            || message.outgoing
            || message.chat != chat.id
            || chat.is_muted(now)
            || (focused && open == Some(chat.id))
        {
            return None;
        }
        let title = bounded(&chat.title, 192);
        let body = if chat.show_notification_preview {
            let prefix = message.sender.as_deref().unwrap_or("");
            let text = if message.text.is_empty() {
                "New message"
            } else {
                &message.text
            };
            bounded(
                &format!(
                    "{}{}{}",
                    bounded(prefix, 128),
                    if prefix.is_empty() { "" } else { ": " },
                    bounded(text, 512)
                ),
                768,
            )
        } else {
            "New message".into()
        };
        Some(Self {
            chat: chat.id,
            title,
            body,
        })
    }
}
fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
