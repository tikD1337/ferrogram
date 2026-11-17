//! Main-list metadata. No full message is retained here.

use serde_json::Value;
use std::collections::HashMap;
use tg_core::{Chat, ChatId, ChatListChange, CommandError, Event, MessageId, MessagePreview};

use crate::mapping::{error, integer, malformed, string};

pub(crate) const MAX_CHATS: usize = 2048;
const MAX_TITLE_BYTES: usize = 1024;
const MAX_PREVIEW_BYTES: usize = 512;

#[derive(Debug)]
struct Entry {
    chat: Chat,
    order: i64,
    scope: usize,
    settings: Settings,
}

#[derive(Debug, Default)]
pub(crate) struct Chats {
    // At most MAX_CHATS entries, including hidden chats. Each owns <=1,536 string bytes:
    // <=3 MiB string data, plus <=8 KiB encoded mini and <=4 KiB path per avatar.
    // No decoded pixels, full messages or file bytes are retained here.
    entries: HashMap<ChatId, Entry>,
    // At most MAX_CHATS IDs (16 KiB of ID data).
    visible: Vec<ChatId>,
    scopes: [Scope; 3],
}

#[derive(Debug, Clone, Copy)]
struct Scope {
    until: i64,
    preview: bool,
}
impl Default for Scope {
    fn default() -> Self {
        Self {
            until: i64::MAX,
            preview: false,
        }
    }
}
#[derive(Debug)]
struct Settings {
    default_mute: bool,
    until: i64,
    default_preview: bool,
    preview: bool,
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
fn deadline(value: &Value) -> i64 {
    let seconds = value.as_i64().unwrap_or(0).max(0);
    if seconds == 0 {
        0
    } else {
        now().saturating_add(seconds)
    }
}
impl Settings {
    fn parse(v: &Value) -> Self {
        Self {
            default_mute: v["use_default_mute_for"].as_bool().unwrap_or(false),
            until: deadline(&v["mute_for"]),
            default_preview: v["use_default_show_preview"].as_bool().unwrap_or(false),
            preview: v["show_preview"].as_bool().unwrap_or(true),
        }
    }
    fn resolve(&self, scope: Scope) -> (i64, bool) {
        (
            if self.default_mute {
                scope.until
            } else {
                self.until
            },
            if self.default_preview {
                scope.preview
            } else {
                self.preview
            },
        )
    }
}
fn chat_scope(v: &Value) -> usize {
    match v["@type"].as_str() {
        Some("chatTypeBasicGroup") => 1,
        Some("chatTypeSupergroup") => {
            if v["is_channel"] == true {
                2
            } else {
                1
            }
        }
        _ => 0,
    }
}

fn main_order(positions: &Value) -> Result<i64, CommandError> {
    let positions = positions.as_array().ok_or_else(malformed)?;
    // TDLib positions are transient and bounded by the enclosing 8 MiB JSON limit.
    for position in positions {
        if position["list"]["@type"] == "chatListMain" {
            return integer(&position["order"]);
        }
    }
    Ok(0)
}

fn preview(value: &Value) -> Result<Option<MessagePreview>, CommandError> {
    if value.is_null() {
        return Ok(None);
    }
    let content = &value["content"];
    // A photo, video or album item without a caption has an empty caption, not a missing one:
    // show the content label ("Photo") instead of an empty line, as Telegram does.
    let attachment = crate::mapping::attachment_preview(content);
    let text = attachment
        .as_deref()
        .or_else(|| {
            content["text"]["text"]
                .as_str()
                .or_else(|| content["caption"]["text"].as_str())
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_else(|| crate::mapping::content_label(content));
    let mut end = text.len().min(MAX_PREVIEW_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(Some(MessagePreview {
        id: MessageId(integer(&value["id"])?),
        text: text[..end].into(),
        date: value["date"]
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0),
        outgoing: value["is_outgoing"].as_bool().unwrap_or(false),
    }))
}

impl Chats {
    pub(crate) fn scope(
        &mut self,
        value: &Value,
        mut emit: impl FnMut(Event),
    ) -> Result<(), CommandError> {
        let scope = match value["scope"]["@type"].as_str() {
            Some("notificationSettingsScopePrivateChats") => 0,
            Some("notificationSettingsScopeGroupChats") => 1,
            Some("notificationSettingsScopeChannelChats") => 2,
            _ => value["scope"]["number"]
                .as_u64()
                .filter(|n| *n < 3)
                .ok_or_else(malformed)? as usize,
        };
        self.scopes[scope] = Scope {
            until: deadline(&value["notification_settings"]["mute_for"]),
            preview: value["notification_settings"]["show_preview"]
                .as_bool()
                .unwrap_or(true),
        };
        for (id, entry) in &mut self.entries {
            if entry.scope != scope {
                continue;
            }
            let (until, preview) = entry.settings.resolve(self.scopes[scope]);
            if (until, preview) == (entry.chat.muted_until, entry.chat.show_notification_preview) {
                continue;
            }
            entry.chat.muted_until = until;
            entry.chat.show_notification_preview = preview;
            if let Some(position) = self.visible.iter().position(|v| v == id) {
                emit(Event::ChatListChanged(ChatListChange::Update {
                    position,
                    chat: entry.chat.clone(),
                }));
            }
        }
        Ok(())
    }
    pub(crate) fn title(&self, id: ChatId) -> Option<&str> {
        self.entries.get(&id).map(|entry| entry.chat.title.as_str())
    }

    pub(crate) fn apply(
        &mut self,
        value: &Value,
        mut emit: impl FnMut(Event),
    ) -> Result<(), CommandError> {
        let new_chat = value["@type"] == "updateNewChat";
        let id = ChatId(integer(if new_chat {
            &value["chat"]["id"]
        } else {
            &value["chat_id"]
        })?);
        let old_position = self.visible.iter().position(|item| *item == id);
        let mut changed = false;
        if new_chat {
            if !self.entries.contains_key(&id) && self.entries.len() == MAX_CHATS {
                return Err(error(
                    -3,
                    "The engine's chat-list memory limit has been reached.",
                ));
            }
            let chat = &value["chat"];
            let scope = chat_scope(&chat["type"]);
            let settings = Settings::parse(&chat["notification_settings"]);
            let (muted_until, show_notification_preview) = settings.resolve(self.scopes[scope]);
            let entry = Entry {
                scope,
                settings,
                chat: Chat {
                    unread_mentions: chat["unread_mention_count"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0),
                    muted_until,
                    show_notification_preview,
                    avatar: crate::mapping::avatar(&chat["photo"])?,
                    id,
                    title: string(&chat["title"], MAX_TITLE_BYTES)?,
                    last_message: preview(&chat["last_message"])?,
                    unread_count: chat["unread_count"]
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .unwrap_or(0),
                    last_read_inbox: MessageId(
                        integer(&chat["last_read_inbox_message_id"]).unwrap_or(0),
                    ),
                    last_read_outbox: MessageId(
                        integer(&chat["last_read_outbox_message_id"]).unwrap_or(0),
                    ),
                },
                order: main_order(&chat["positions"])?,
            };
            self.entries.insert(id, entry);
            changed = true;
        } else if let Some(entry) = self.entries.get_mut(&id) {
            match value["@type"].as_str() {
                Some("updateChatPosition") => {
                    if value["position"]["list"]["@type"] != "chatListMain" {
                        return Ok(());
                    }
                    entry.order = integer(&value["position"]["order"])?;
                }
                Some("updateChatLastMessage") => {
                    let last_message = preview(&value["last_message"])?;
                    let order = main_order(&value["positions"])?;
                    changed = entry.chat.last_message != last_message;
                    entry.chat.last_message = last_message;
                    entry.order = order;
                }
                Some("updateChatPhoto") => {
                    let avatar = crate::mapping::avatar(&value["photo"])?;
                    changed = entry.chat.avatar != avatar;
                    entry.chat.avatar = avatar;
                }
                Some("updateChatTitle") => {
                    let title = string(&value["title"], MAX_TITLE_BYTES)?;
                    changed = entry.chat.title != title;
                    entry.chat.title = title;
                }
                Some("updateChatReadInbox") => {
                    let unread =
                        u32::try_from(integer(&value["unread_count"])?).map_err(|_| malformed())?;
                    let last_read = MessageId(integer(&value["last_read_inbox_message_id"])?);
                    changed = entry.chat.unread_count != unread
                        || entry.chat.last_read_inbox != last_read;
                    entry.chat.unread_count = unread;
                    entry.chat.last_read_inbox = last_read;
                }
                Some("updateChatUnreadMentionCount") => {
                    let count = u32::try_from(integer(&value["unread_mention_count"])?)
                        .map_err(|_| malformed())?;
                    changed = entry.chat.unread_mentions != count;
                    entry.chat.unread_mentions = count;
                }
                Some("updateChatNotificationSettings") => {
                    entry.settings = Settings::parse(&value["notification_settings"]);
                    let (until, preview) = entry.settings.resolve(self.scopes[entry.scope]);
                    changed = entry.chat.muted_until != until
                        || entry.chat.show_notification_preview != preview;
                    entry.chat.muted_until = until;
                    entry.chat.show_notification_preview = preview;
                }
                Some("updateChatReadOutbox") => {
                    let last_read = MessageId(integer(&value["last_read_outbox_message_id"])?);
                    changed = entry.chat.last_read_outbox != last_read;
                    entry.chat.last_read_outbox = last_read;
                }
                _ => {}
            }
        } else {
            return Ok(());
        }
        let Some(entry) = self.entries.get(&id) else {
            return Ok(());
        };
        if let Some(position) = old_position {
            self.visible.remove(position);
        }
        let new_position = if entry.order > 0 {
            let position = self.visible.partition_point(|other| {
                self.entries
                    .get(other)
                    .is_some_and(|other| (other.order, other.chat.id) > (entry.order, id))
            });
            self.visible.insert(position, id);
            Some(position)
        } else {
            None
        };
        match (old_position, new_position) {
            (None, Some(position)) => emit(Event::ChatListChanged(ChatListChange::Insert {
                position,
                chat: entry.chat.clone(),
            })),
            (Some(position), None) => {
                emit(Event::ChatListChanged(ChatListChange::Remove { position }))
            }
            (Some(from), Some(to)) => {
                if from != to {
                    emit(Event::ChatListChanged(ChatListChange::Move { from, to }));
                }
                if changed {
                    emit(Event::ChatListChanged(ChatListChange::Update {
                        position: to,
                        chat: entry.chat.clone(),
                    }));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
