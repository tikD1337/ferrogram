//! Bounded command serialization; no queues, waits or I/O.

use serde_json::{Value, json};
use std::ffi::CString;
use tg_core::{ChatId, Command, CommandError, MessageId};

use crate::mapping::{MAX_LINK_BYTES, MAX_TEXT_BYTES, PAGE_SIZE, error};

pub(crate) fn encode(command: Command) -> Result<Value, CommandError> {
    let (mut request, tag) = match command {
        Command::DownloadRange {
            file,
            offset,
            limit,
        } => {
            if file <= 0
                || offset
                    .checked_add(limit)
                    .is_none_or(|end| end >= (1u64 << 53))
            {
                return Err(error(-2, "Invalid streaming range."));
            }
            return Ok(
                json!({"@type":"downloadFile","file_id":file,"priority":32,"offset":offset,"limit":limit,"synchronous":false,"@extra":{"command":"download","file":file}}),
            );
        }
        Command::OpenMessageContent { chat, message } => {
            valid_id(message)?;
            (
                json!({"@type":"openMessageContent","chat_id":chat.0,"message_id":message.0}),
                "open_content",
            )
        }
        Command::DownloadFileUrgent(id) => {
            if id <= 0 {
                return Err(error(-2, "Invalid media file ID."));
            }
            return Ok(
                json!({"@type":"downloadFile","file_id":id,"priority":32,"offset":0,"limit":0,"synchronous":false,
                "@extra":{"command":"download","file":id}}),
            );
        }
        Command::ReplyText { chat, reply, text } => {
            valid_id(reply)?;
            check_input(&text, MAX_TEXT_BYTES)?;
            (
                json!({"@type":"sendMessage","chat_id":chat.0,
                "reply_to":{"@type":"inputMessageReplyToMessage","message_id":reply.0},
                "input_message_content":input_text(text)}),
                "send_text",
            )
        }
        Command::EditText {
            chat,
            message,
            text,
        } => {
            valid_id(message)?;
            check_input(&text, MAX_TEXT_BYTES)?;
            (
                json!({"@type":"editMessageText","chat_id":chat.0,"message_id":message.0,
                "input_message_content":input_text(text)}),
                "edit_text",
            )
        }
        Command::DeleteMessages {
            chat,
            messages,
            revoke,
        } => {
            let ids = album_ids(messages)?;
            (
                json!({"@type":"deleteMessages","chat_id":chat.0,"message_ids":ids,"revoke":revoke}),
                "delete",
            )
        }
        Command::ForwardMessages { from, to, messages } => {
            let ids = album_ids(messages)?;
            (
                json!({"@type":"forwardMessages","from_chat_id":from.0,"chat_id":to.0,
                "message_ids":ids,"send_copy":false,"remove_caption":false}),
                "forward",
            )
        }
        Command::GetRepliedMessage { chat, message } => {
            valid_id(message)?;
            return Ok(
                json!({"@type":"getRepliedMessage","chat_id":chat.0,"message_id":message.0,
                "@extra":{"command":"replied","chat":chat.0,"message":message.0}}),
            );
        }
        Command::GetMessageProperties { chat, message } => {
            valid_id(message)?;
            return Ok(
                json!({"@type":"getMessageProperties","chat_id":chat.0,"message_id":message.0,
                "@extra":{"command":"properties","chat":chat.0,"message":message.0}}),
            );
        }
        Command::ViewMessages { chat, messages } => {
            if messages.is_empty() || messages.len() > 50 || messages.iter().any(|id| id.0 <= 0) {
                return Err(error(-2, "Invalid visible-message batch."));
            }
            let ids: Vec<i64> = messages.into_iter().map(|id| id.0).collect();
            return Ok(
                json!({"@type":"viewMessages","chat_id":chat.0,"message_ids":ids,
                "source":{"@type":"messageSourceChatHistory"},"force_read":true,
                "@extra":{"command":"view","chat":chat.0}}),
            );
        }
        Command::OpenChatAt { chat, anchor } => {
            if anchor.0 < 0 {
                return Err(error(-2, "Invalid unread history anchor."));
            }
            return Ok(
                json!({"@type":"openChat","chat_id":chat.0,"@extra":{"command":"open_around","chat":chat.0,"anchor":anchor.0}}),
            );
        }
        Command::DownloadFile(id) | Command::CancelDownload(id) if id <= 0 => {
            return Err(error(-2, "Invalid media file ID."));
        }
        Command::DownloadFile(id) => (
            json!({"@type":"downloadFile", "file_id":id, "priority":16, "offset":0, "limit":0, "synchronous":false}),
            "download",
        ),
        Command::CancelDownload(id) => (
            json!({"@type":"cancelDownloadFile", "file_id":id, "only_if_pending":false}),
            "cancel_download",
        ),
        Command::RequestQrSignIn => (
            json!({"@type":"requestQrCodeAuthentication", "other_user_ids":[]}),
            "qr",
        ),
        Command::SubmitPhoneNumber(value) => {
            check_input(value.expose(), MAX_LINK_BYTES)?;
            (
                json!({"@type":"setAuthenticationPhoneNumber", "phone_number":value.expose()}),
                "phone",
            )
        }
        Command::SubmitCode(value) => {
            check_input(value.expose(), MAX_LINK_BYTES)?;
            (
                json!({"@type":"checkAuthenticationCode", "code":value.expose()}),
                "code",
            )
        }
        Command::SubmitPassword(value) => {
            check_input(value.expose(), MAX_LINK_BYTES)?;
            (
                json!({"@type":"checkAuthenticationPassword", "password":value.expose()}),
                "password",
            )
        }
        Command::LoadMoreChats => (
            json!({"@type":"loadChats", "chat_list":{"@type":"chatListMain"}, "limit":PAGE_SIZE}),
            "chats",
        ),
        Command::OpenChat(chat) => (json!({"@type":"openChat", "chat_id":chat.0}), "open"),
        Command::CloseChat(chat) => (json!({"@type":"closeChat", "chat_id":chat.0}), "close_chat"),
        Command::LoadOlderMessages { chat, before } => {
            if before.0 < 0 {
                return Err(error(-2, "The history cursor must not be negative."));
            }
            return Ok(history(chat, before));
        }
        Command::LoadNewerMessages { chat, after } => {
            if after.0 <= 0 {
                return Err(error(-2, "The newer history cursor must be positive."));
            }
            return Ok(json!({"@type":"getChatHistory", "chat_id":chat.0,
                "from_message_id":after.0, "offset":-49, "limit":PAGE_SIZE, "only_local":false,
                "@extra":{"command":"history_newer", "chat":chat.0, "after":after.0}}));
        }
        Command::SendText { chat, text } => {
            check_input(&text, MAX_TEXT_BYTES)?;
            (
                json!({"@type":"sendMessage", "chat_id":chat.0,
                "input_message_content":{"@type":"inputMessageText", "text":{"@type":"formattedText", "text":text, "entities":[]}, "clear_draft":true}}),
                "send_text",
            )
        }
        Command::LogOut => (json!({"@type":"logOut"}), "logout"),
        Command::ShutDown => (json!({"@type":"close"}), "shutdown"),
        _ => return Err(error(-2, "Unsupported command.")),
    };
    request["@extra"] = json!({"command":tag});
    if tag == "open" || tag == "close_chat" {
        request["@extra"]["chat"] = request["chat_id"].clone();
    }
    Ok(request)
}

pub(crate) fn history(chat: ChatId, before: MessageId) -> Value {
    // TDLib returns messages older than from_message_id when offset is 0, and rejects
    // identifiers that are not real message ids (code 400), so the cursor is passed as is.
    // The decoder still drops anything not older than the cursor. 0 means the newest page.
    let from = before.0.max(0);
    json!({"@type":"getChatHistory", "chat_id":chat.0, "from_message_id":from,
        "offset":0, "limit":PAGE_SIZE, "only_local":false,
        "@extra":{"command":"history", "chat":chat.0, "before":before.0}})
}

pub(crate) fn around(chat: ChatId, anchor: MessageId) -> Value {
    json!({"@type":"getChatHistory","chat_id":chat.0,"from_message_id":anchor.0,
        "offset":if anchor.0 == 0 {0}else{-25},"limit":PAGE_SIZE,"only_local":false,
        "@extra":{"command":"history_around","chat":chat.0,"anchor":anchor.0}})
}

fn input_text(text: String) -> Value {
    json!({"@type":"inputMessageText","text":{"@type":"formattedText","text":text,"entities":[]},"clear_draft":true})
}
fn valid_id(id: MessageId) -> Result<(), CommandError> {
    if id.0 <= 0 {
        return Err(error(-2, "Invalid message ID."));
    }
    Ok(())
}
fn album_ids(messages: Vec<MessageId>) -> Result<Vec<i64>, CommandError> {
    if messages.is_empty() || messages.len() > 10 || messages.windows(2).any(|p| p[0] >= p[1]) {
        return Err(error(-2, "Invalid message action batch."));
    }
    messages
        .into_iter()
        .map(|id| {
            valid_id(id)?;
            Ok(id.0)
        })
        .collect()
}
fn check_input(value: &str, limit: usize) -> Result<(), CommandError> {
    if value.is_empty() || value.len() > limit {
        return Err(error(-2, "Input is empty or exceeds the size limit."));
    }
    Ok(())
}

pub(crate) fn serialize(value: &Value) -> Result<CString, CommandError> {
    // Requests contain a fixed number of fields and at most 64 KiB of input text.
    // Escaping can expand text sixfold; the serialized request is at most 400 KiB.
    let bytes =
        serde_json::to_vec(value).map_err(|_| error(-2, "Could not encode the request."))?;
    if bytes.len() > 400 * 1024 {
        return Err(error(-2, "The encoded request exceeds the size limit."));
    }
    CString::new(bytes).map_err(|_| error(-2, "Could not encode the request."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_requests_are_asynchronous_and_cancellable() {
        let request = encode(Command::DownloadFile(42)).unwrap();
        assert_eq!(request["@type"], "downloadFile");
        assert_eq!(request["synchronous"], false);
        assert_eq!(request["file_id"], 42);
        let request = encode(Command::CancelDownload(42)).unwrap();
        assert_eq!(request["@type"], "cancelDownloadFile");
        assert_eq!(request["only_if_pending"], false);
        assert!(encode(Command::DownloadFile(0)).is_err());
    }
    #[test]
    fn explicit_loads_are_bounded_and_history_carries_the_cursor() {
        let chats = encode(Command::LoadMoreChats).unwrap();
        assert_eq!(chats["@type"], "loadChats");
        assert_eq!(chats["limit"], 50);
        assert_eq!(chats["chat_list"]["@type"], "chatListMain");
        let history = encode(Command::LoadOlderMessages {
            chat: ChatId(7),
            before: MessageId(13631488),
        })
        .unwrap();
        assert_eq!(history["limit"], 50);
        assert_eq!(history["from_message_id"], 13631488);
        assert_eq!(history["@extra"]["chat"], 7);
        assert_eq!(history["@extra"]["before"], 13631488);
    }

    #[test]
    fn text_is_sent_as_formatted_plain_text_and_nul_is_json_escaped() {
        let request = encode(Command::SendText {
            chat: ChatId(7),
            text: "hello\0😀".into(),
        })
        .unwrap();
        assert_eq!(request["@type"], "sendMessage");
        assert_eq!(
            request["input_message_content"]["@type"],
            "inputMessageText"
        );
        let encoded = serialize(&request).unwrap();
        let decoded: Value = serde_json::from_slice(encoded.as_bytes()).unwrap();
        assert_eq!(
            decoded["input_message_content"]["text"]["text"],
            "hello\0😀"
        );
    }

    #[test]
    fn oversized_input_is_rejected_before_native_send() {
        assert!(
            encode(Command::SendText {
                chat: ChatId(7),
                text: "x".repeat(MAX_TEXT_BYTES + 1)
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod newer_tests {
    use super::*;
    #[test]
    fn newer_request_keeps_real_anchor_and_reserves_overlap() {
        let request = encode(Command::LoadNewerMessages {
            chat: ChatId(7),
            after: MessageId(104857600),
        })
        .unwrap();
        assert_eq!(request["from_message_id"], 104857600_i64);
        assert_eq!(request["offset"], -49);
        assert_eq!(request["limit"], 50);
        assert_eq!(request["@extra"]["after"], 104857600_i64);
        assert!(
            encode(Command::LoadNewerMessages {
                chat: ChatId(7),
                after: MessageId(0)
            })
            .is_err()
        );
    }
}

#[cfg(test)]
mod attention_tests {
    use super::*;
    #[test]
    fn views_use_chat_history_source_force_read_and_bounded_real_ids() {
        let view = encode(Command::ViewMessages {
            chat: ChatId(7),
            messages: (1..=50).map(|n| MessageId(n * 1048576)).collect(),
        })
        .unwrap();
        assert_eq!(view["@type"], "viewMessages");
        assert_eq!(view["message_ids"].as_array().unwrap().len(), 50);
        assert_eq!(view["source"]["@type"], "messageSourceChatHistory");
        assert_eq!(view["force_read"], true);
        for ids in [vec![], vec![MessageId(0)], vec![MessageId(1048576); 51]] {
            assert!(
                encode(Command::ViewMessages {
                    chat: ChatId(7),
                    messages: ids
                })
                .is_err()
            );
        }
    }
    #[test]
    fn opening_at_unread_uses_real_anchor_and_balanced_history() {
        let request = encode(Command::OpenChatAt {
            chat: ChatId(7),
            anchor: MessageId(20401094656),
        })
        .unwrap();
        assert_eq!(request["@extra"]["anchor"], 20401094656i64);
        let request = around(ChatId(7), MessageId(20401094656));
        assert_eq!(request["from_message_id"], 20401094656i64);
        assert_eq!(request["offset"], -25);
        assert_eq!(request["limit"], 50);
        assert_eq!(around(ChatId(7), MessageId(0))["from_message_id"], 0);
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    #[test]
    fn reply_edit_delete_forward_keep_real_ids_and_bound_albums() {
        let chat = ChatId(7);
        let id = MessageId(20971520000);
        let r = encode(Command::ReplyText {
            chat,
            reply: id,
            text: "reply".into(),
        })
        .unwrap();
        assert_eq!(r["reply_to"]["@type"], "inputMessageReplyToMessage");
        assert_eq!(r["reply_to"]["message_id"], id.0);
        let r = encode(Command::EditText {
            chat,
            message: id,
            text: "edit".into(),
        })
        .unwrap();
        assert_eq!(r["@type"], "editMessageText");
        assert_eq!(r["message_id"], id.0);
        let r = encode(Command::DeleteMessages {
            chat,
            messages: vec![id],
            revoke: true,
        })
        .unwrap();
        assert_eq!(r["@type"], "deleteMessages");
        assert_eq!(r["revoke"], true);
        let r = encode(Command::ForwardMessages {
            from: chat,
            to: ChatId(8),
            messages: vec![id, MessageId(id.0 + 1048576)],
        })
        .unwrap();
        assert_eq!(r["@type"], "forwardMessages");
        assert_eq!(r["from_chat_id"], 7);
        assert_eq!(r["message_ids"].as_array().unwrap().len(), 2);
        assert!(
            encode(Command::DeleteMessages {
                chat,
                messages: vec![id; 11],
                revoke: false
            })
            .is_err()
        );
        assert!(
            encode(Command::ForwardMessages {
                from: chat,
                to: chat,
                messages: vec![]
            })
            .is_err()
        );
    }
}
#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn explicit_download_is_higher_priority_and_content_open_uses_real_id() {
        let urgent = encode(Command::DownloadFileUrgent(42)).unwrap();
        let photo = encode(Command::DownloadFile(42)).unwrap();
        assert!(urgent["priority"].as_u64().unwrap() > photo["priority"].as_u64().unwrap());
        assert_eq!(urgent["synchronous"], false);
        let opened = encode(Command::OpenMessageContent {
            chat: ChatId(7),
            message: MessageId(20971520000),
        })
        .unwrap();
        assert_eq!(opened["@type"], "openMessageContent");
        assert_eq!(opened["message_id"], 20971520000_i64);
        assert!(encode(Command::DownloadFileUrgent(-1)).is_err());
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    #[test]
    fn seek_request_is_offset_based_asynchronous_and_validated() {
        let v = encode(Command::DownloadRange {
            file: 42,
            offset: 3 * 1024 * 1024,
            limit: 1024 * 1024,
        })
        .unwrap();
        assert_eq!(v["offset"], 3 * 1024 * 1024);
        assert_eq!(v["priority"], 32);
        assert_eq!(v["limit"], 1024 * 1024);
        assert_eq!(v["synchronous"], false);
        assert!(
            encode(Command::DownloadRange {
                file: 0,
                offset: 0,
                limit: 1
            })
            .is_err()
        );
        assert!(
            encode(Command::DownloadRange {
                file: 1,
                offset: 1 << 53,
                limit: 0
            })
            .is_err()
        );
        assert!(
            encode(Command::DownloadRange {
                file: 1,
                offset: (1 << 53) - 1,
                limit: 1
            })
            .is_err()
        );
        assert!(
            encode(Command::DownloadRange {
                file: 1,
                offset: u64::MAX,
                limit: 2
            })
            .is_err()
        );
    }
}
