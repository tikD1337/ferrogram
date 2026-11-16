use tg_core::{AuthState, ChatId, ChatListChange, EntityKind, Event, MessageId};
use tg_telegram::Decoder;

fn decode(decoder: &mut Decoder, json: &str) -> Vec<Event> {
    // Fixtures produce at most three events per update.
    let mut events = Vec::new();
    let result = decoder.decode(json, |event| events.push(event));
    assert_eq!(result, Ok(()));
    assert!(events.len() <= 3);
    events
}

#[test]
fn qr_confirmation_exposes_a_link_without_leaking_it_in_debug() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"updateAuthorizationState","authorization_state":{"@type":"authorizationStateWaitOtherDeviceConfirmation","link":"tg://synthetic"}}"#,
    );
    assert_eq!(
        events,
        [Event::AuthStateChanged(AuthState::WaitOtherDevice {
            link: "tg://synthetic".into()
        })]
    );
    assert!(!format!("{:?}", events[0]).contains("tg://synthetic"));
}

#[test]
fn non_bmp_text_keeps_utf16_entity_offsets() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"updateNewMessage","message":{"id":12,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"A😀bold","entities":[{"offset":3,"length":4,"type":{"@type":"textEntityTypeBold"}}]}}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("expected message")
    };
    assert_eq!(message.chat, ChatId(7));
    assert_eq!(message.id, MessageId(12));
    assert_eq!(message.text, "A😀bold");
    assert_eq!(message.entities[0].offset, 3);
    assert_eq!(message.entities[0].length, 4);
    assert_eq!(message.entities[0].kind, EntityKind::Bold);
}

#[test]
fn main_list_orders_by_int64_then_chat_id_and_emits_positions() {
    let mut decoder = Decoder::default();
    let first = decode(
        &mut decoder,
        r#"{"@type":"updateNewChat","chat":{"id":1,"title":"One","positions":[{"list":{"@type":"chatListMain"},"order":"9007199254740993"}]}}"#,
    );
    assert!(
        matches!(&first[0], Event::ChatListChanged(ChatListChange::Insert { position: 0, chat }) if chat.id == ChatId(1))
    );
    let second = decode(
        &mut decoder,
        r#"{"@type":"updateNewChat","chat":{"id":2,"title":"Two","positions":[{"list":{"@type":"chatListMain"},"order":"9007199254740993"}]}}"#,
    );
    assert!(
        matches!(&second[0], Event::ChatListChanged(ChatListChange::Insert { position: 0, chat }) if chat.id == ChatId(2))
    );
    let moved = decode(
        &mut decoder,
        r#"{"@type":"updateChatPosition","chat_id":1,"position":{"list":{"@type":"chatListMain"},"order":"9007199254740994"}}"#,
    );
    assert_eq!(
        moved,
        [Event::ChatListChanged(ChatListChange::Move {
            from: 1,
            to: 0
        })]
    );
    let removed = decode(
        &mut decoder,
        r#"{"@type":"updateChatPosition","chat_id":1,"position":{"list":{"@type":"chatListMain"},"order":"0"}}"#,
    );
    assert_eq!(
        removed,
        [Event::ChatListChanged(ChatListChange::Remove {
            position: 0
        })]
    );
    let archived = decode(
        &mut decoder,
        r#"{"@type":"updateChatPosition","chat_id":2,"position":{"list":{"@type":"chatListArchive"},"order":"0"}}"#,
    );
    assert!(archived.is_empty());
}

#[test]
fn unknown_error_text_is_not_forwarded() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"error","code":400,"message":"private server detail","@extra":{"command":"send_text"}}"#,
    );
    let Event::CommandFailed { error, .. } = &events[0] else {
        panic!("expected failure")
    };
    assert_eq!(error.code, 400);
    assert!(!error.message.contains("private server detail"));
}

#[test]
fn older_page_excludes_the_anchor_and_advances_across_non_text_messages() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"messages","@extra":{"command":"history","chat":7,"before":31457280},"messages":[{"id":31457280,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"anchor","entities":[]}}},{"id":20971520,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messagePhoto"}},{"id":10485760,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messagePhoto"}}]}"#,
    );
    let Event::MessagesLoaded {
        messages,
        next_before,
        ..
    } = &events[0]
    else {
        panic!("expected page")
    };
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|m| m.text == "Photo"));
    assert_eq!(*next_before, Some(MessageId(10485760)));
}

#[test]
fn invalid_utf16_ranges_are_rejected_without_splitting_a_surrogate_pair() {
    let mut decoder = Decoder::default();
    assert!(decoder.decode(r#"{"@type":"updateNewMessage","message":{"id":12,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"A😀bold","entities":[{"offset":2,"length":1,"type":{"@type":"textEntityTypeBold"}}]}}}}"#, |_| {}).is_err());
}

#[test]
fn title_read_status_and_last_message_update_the_current_row() {
    let mut decoder = Decoder::default();
    decode(
        &mut decoder,
        r#"{"@type":"updateNewChat","chat":{"id":1,"title":"One","positions":[{"list":{"@type":"chatListMain"},"order":"100"}]}}"#,
    );
    let title = decode(
        &mut decoder,
        r#"{"@type":"updateChatTitle","chat_id":1,"title":"Renamed"}"#,
    );
    assert!(
        matches!(&title[0], Event::ChatListChanged(ChatListChange::Update { position: 0, chat }) if chat.title == "Renamed")
    );
    let read = decode(
        &mut decoder,
        r#"{"@type":"updateChatReadInbox","chat_id":1,"unread_count":4,"last_read_inbox_message_id":42}"#,
    );
    assert!(
        matches!(&read[0], Event::ChatListChanged(ChatListChange::Update { position: 0, chat }) if chat.unread_count == 4 && chat.last_read_inbox == MessageId(42))
    );
    let last = decode(
        &mut decoder,
        r#"{"@type":"updateChatLastMessage","chat_id":1,"last_message":{"id":50,"content":{"@type":"messageText","text":{"text":"preview"}}},"positions":[{"list":{"@type":"chatListMain"},"order":"200"}]}"#,
    );
    assert!(
        matches!(&last[0], Event::ChatListChanged(ChatListChange::Update { position: 0, chat }) if chat.last_message.as_ref().is_some_and(|last| last.id == MessageId(50) && last.text == "preview"))
    );
    let removed = decode(
        &mut decoder,
        r#"{"@type":"updateChatLastMessage","chat_id":1,"last_message":null,"positions":[]}"#,
    );
    assert_eq!(
        removed,
        [Event::ChatListChanged(ChatListChange::Remove {
            position: 0
        })]
    );
    let inserted = decode(
        &mut decoder,
        r#"{"@type":"updateChatPosition","chat_id":1,"position":{"list":{"@type":"chatListMain"},"order":"100"}}"#,
    );
    assert!(
        matches!(&inserted[0], Event::ChatListChanged(ChatListChange::Insert { position: 0, chat }) if chat.title == "Renamed" && chat.unread_count == 4 && chat.last_message.is_none())
    );
}

#[test]
fn send_completion_replaces_the_temporary_id_and_sanitizes_failure() {
    let mut decoder = Decoder::default();
    let success = decode(
        &mut decoder,
        r#"{"@type":"updateMessageSendSucceeded","old_message_id":-1,"message":{"id":12,"chat_id":7,"date":1,"is_outgoing":true,"content":{"@type":"messageText","text":{"text":"sent","entities":[]}}}}"#,
    );
    assert!(
        matches!(&success[0], Event::MessageUpdated { old_id: MessageId(-1), message } if message.id == MessageId(12) && message.send_state == tg_core::SendState::Sent)
    );
    let failed = decode(
        &mut decoder,
        r#"{"@type":"updateMessageSendFailed","old_message_id":-2,"error":{"code":400,"message":"private server detail"},"message":{"id":-3,"chat_id":7,"date":1,"is_outgoing":true,"content":{"@type":"messageText","text":{"text":"unsent","entities":[]}}}}"#,
    );
    assert!(
        matches!(&failed[0], Event::MessageUpdated { old_id: MessageId(-2), message } if matches!(&message.send_state, tg_core::SendState::Failed(error) if error.code == 400 && !error.message.contains("private server detail")))
    );
}

#[test]
fn every_required_entity_kind_is_mapped_without_changing_utf16_units() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"updateNewMessage","message":{"id":12,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"😀x","entities":[{"offset":0,"length":2,"type":{"@type":"textEntityTypeBold"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeItalic"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeUnderline"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeStrikethrough"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeCode"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypePre"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeSpoiler"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeUrl"}},{"offset":0,"length":2,"type":{"@type":"textEntityTypeTextUrl","url":"https://example.invalid/"}},{"offset":2,"length":1,"type":{"@type":"textEntityTypeMention"}}]}}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("expected message")
    };
    // Fixed list of ten expected entity kinds; no dynamically growing expected data.
    let expected = [
        EntityKind::Bold,
        EntityKind::Italic,
        EntityKind::Underline,
        EntityKind::Strikethrough,
        EntityKind::Code,
        EntityKind::Pre,
        EntityKind::Spoiler,
        EntityKind::Url,
        EntityKind::TextUrl {
            target: "https://example.invalid/".into(),
        },
        EntityKind::Mention,
    ];
    assert_eq!(message.entities.len(), expected.len());
    for (entity, expected) in message.entities.iter().zip(expected) {
        assert_eq!(entity.kind, expected);
    }
    assert_eq!(message.entities[9].offset, 2);
}

#[test]
fn only_an_explicit_load_chats_response_reports_list_exhaustion() {
    let mut decoder = Decoder::default();
    assert_eq!(
        decode(
            &mut decoder,
            r#"{"@type":"error","code":404,"message":"synthetic","@extra":{"command":"chats"}}"#
        ),
        [Event::ChatsLoaded { has_more: false }]
    );
    assert_eq!(
        decode(
            &mut decoder,
            r#"{"@type":"ok","@extra":{"command":"chats"}}"#
        ),
        [Event::ChatsLoaded { has_more: true }]
    );
    assert!(matches!(
        &decode(
            &mut decoder,
            r#"{"@type":"error","code":404,"message":"synthetic","@extra":{"command":"open"}}"#
        )[0],
        Event::CommandFailed {
            command: tg_core::CommandKind::OpenChat,
            ..
        }
    ));
}

#[test]
fn oversized_pages_and_malformed_json_return_sanitized_errors() {
    let mut decoder = Decoder::default();
    assert!(decoder.decode("not json", |_| {}).is_err());
    // Exactly 51 tiny synthetic values, bounded below 1 KiB total.
    let source = serde_json::json!({"@type":"messages", "@extra":{"command":"history", "chat":7, "before":0}, "messages":vec![serde_json::Value::Null; 51]});
    assert!(decoder.decode(&source.to_string(), |_| {}).is_err());
}

#[test]
fn chat_metadata_limit_is_explicit_and_reported() {
    let mut decoder = Decoder::default();
    // Fixed 2,049 synthetic rows; no collected events or unbounded fixture storage.
    for id in 1..=2048 {
        let source = serde_json::json!({"@type":"updateNewChat", "chat":{"id":id, "title":"row", "positions":[]}});
        assert_eq!(decoder.decode(&source.to_string(), |_| {}), Ok(()));
    }
    assert!(matches!(
        decoder.decode(
            r#"{"@type":"updateNewChat","chat":{"id":2049,"title":"overflow","positions":[]}}"#,
            |_| {}
        ),
        Err(tg_core::CommandError { code: -3, .. })
    ));
}

#[test]
fn newer_page_filters_anchor_and_uses_maximum_raw_id_for_cursor() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"messages","@extra":{"command":"history_newer","chat":7,"after":104857600},"messages":[{"id":157286400,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messagePhoto"}},{"id":125829120,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"newer","entities":[]}}},{"id":104857600,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messagePhoto"}}]}"#,
    );
    let Event::NewerMessagesLoaded {
        after,
        next_after,
        messages,
        ..
    } = &events[0]
    else {
        panic!("expected newer history");
    };
    assert_eq!(*after, MessageId(104857600));
    assert_eq!(*next_after, Some(MessageId(157286400)));
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].id, MessageId(125829120));
}

#[test]
fn unsupported_content_is_a_visible_row() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"updateNewMessage","message":{"id":104857600,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageVideo"}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("placeholder row")
    };
    assert_eq!(message.text, "Video");
}

#[test]
fn photo_caption_and_display_variant_survive_mapping() {
    let events = decode(
        &mut Decoder::default(),
        r#"{"@type":"updateNewMessage","message":{"id":104857600,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messagePhoto","caption":{"text":"A caption","entities":[{"offset":2,"length":7,"type":{"@type":"textEntityTypeBold"}}]},"photo":{"minithumbnail":{"data":"AQID"},"sizes":[{"width":90,"height":60,"photo":{"id":1}},{"width":800,"height":600,"photo":{"id":2}},{"width":4000,"height":3000,"photo":{"id":3}}]}}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("photo row")
    };
    assert_eq!(message.text, "A caption");
    assert_eq!(message.entities.len(), 1);
    let photo = message.photo.as_ref().unwrap();
    assert_eq!(photo.file.id, 2);
    assert_eq!((photo.width, photo.height), (800, 600));
    assert_eq!(photo.minithumbnail, [1, 2, 3]);
}
#[test]
fn avatars_change_and_file_completion_uses_domain_metadata() {
    let mut decoder = Decoder::default();
    let events = decode(
        &mut decoder,
        r#"{"@type":"updateNewChat","chat":{"id":7,"title":"Synthetic","photo":{"small":{"id":9}},"positions":[{"list":{"@type":"chatListMain"},"order":"100"}]}}"#,
    );
    let Event::ChatListChanged(ChatListChange::Insert { chat, .. }) = &events[0] else {
        panic!("chat")
    };
    assert_eq!(chat.avatar.as_ref().unwrap().file.id, 9);
    let events = decode(
        &mut decoder,
        r#"{"@type":"updateChatPhoto","chat_id":7,"photo":null}"#,
    );
    assert!(
        matches!(&events[0],Event::ChatListChanged(ChatListChange::Update {chat,..}) if chat.avatar.is_none())
    );
    let events = decode(
        &mut decoder,
        r#"{"@type":"updateFile","file":{"id":9,"local":{"is_downloading_completed":true,"path":"C:/synthetic/photo.jpg"}}}"#,
    );
    let Event::FileUpdated(file) = &events[0] else {
        panic!("file")
    };
    assert_eq!(file.id, 9);
    assert!(file.path.is_some());
    assert!(!format!("{file:?}").contains("photo.jpg"));
}
#[test]
fn oversized_minithumbnails_and_paths_are_rejected() {
    let oversized = "A".repeat(12289);
    let json = format!(
        r#"{{"@type":"updateNewChat","chat":{{"id":7,"title":"Synthetic","photo":{{"small":{{"id":9}},"minithumbnail":{{"data":"{oversized}"}}}},"positions":[]}}}}"#
    );
    assert!(Decoder::default().decode(&json, |_| {}).is_err());
    let json = format!(
        r#"{{"@type":"updateFile","file":{{"id":9,"local":{{"is_downloading_completed":true,"path":"{}"}}}}}}"#,
        "x".repeat(4097)
    );
    assert!(Decoder::default().decode(&json, |_| {}).is_err());
}

#[test]
fn group_sender_name_album_and_private_chat_sender() {
    let mut decoder = Decoder::default();
    assert!(
        decode(
            &mut decoder,
            r#"{"@type":"updateUser","user":{"id":5,"first_name":"Anna","last_name":"Petrova"}}"#
        )
        .is_empty()
    );
    let events = decode(
        &mut decoder,
        r#"{"@type":"updateNewMessage","message":{"id":1048576,"chat_id":-1001,"date":1,"is_outgoing":false,"media_album_id":"13887604000000001","sender_id":{"@type":"messageSenderUser","user_id":5},"content":{"@type":"messageText","text":{"text":"hi","entities":[]}}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("expected message")
    };
    assert_eq!(message.sender.as_deref(), Some("Anna Petrova"));
    assert_eq!((message.sender_id, message.album), (5, 13887604000000001));
    // In a private chat the sender is the chat itself: no name above the bubble.
    let events = decode(
        &mut decoder,
        r#"{"@type":"updateNewMessage","message":{"id":2097152,"chat_id":5,"date":1,"is_outgoing":false,"sender_id":{"@type":"messageSenderUser","user_id":5},"content":{"@type":"messageText","text":{"text":"hi","entities":[]}}}}"#,
    );
    let (Event::NewMessage(message) | Event::IncomingMessage(message)) = &events[0] else {
        panic!("expected message")
    };
    assert_eq!((message.sender.as_deref(), message.album), (None, 0));
}

#[test]
fn notification_scope_defaults_overrides_and_mentions_are_mapped() {
    use serde_json::json;
    let mut decoder = Decoder::default();
    let mut chat = json!({"id":7,"title":"Synthetic group","type":{"@type":"chatTypeSupergroup","is_channel":false},"last_message":null,"positions":[{"list":{"@type":"chatListMain"},"order":"100"}],"unread_count":3,"unread_mention_count":2,"notification_settings":{"use_default_mute_for":true,"use_default_show_preview":true}});
    let inserted = decode(
        &mut decoder,
        &json!({"@type":"updateNewChat","chat":chat}).to_string(),
    );
    assert!(
        matches!(&inserted[0],Event::ChatListChanged(ChatListChange::Insert {chat,..}) if chat.unread_mentions==2)
    );
    let changed = decode(
        &mut decoder,
        r#"{"@type":"updateScopeNotificationSettings","scope":{"@type":"notificationSettingsScopeGroupChats"},"notification_settings":{"mute_for":3600,"show_preview":false}}"#,
    );
    assert!(
        matches!(&changed[0],Event::ChatListChanged(ChatListChange::Update {chat,..}) if chat.muted_until>0 && !chat.show_notification_preview)
    );
    let changed = decode(
        &mut decoder,
        r#"{"@type":"updateChatNotificationSettings","chat_id":7,"notification_settings":{"use_default_mute_for":false,"mute_for":0,"use_default_show_preview":false,"show_preview":true}}"#,
    );
    assert!(
        matches!(&changed[0],Event::ChatListChanged(ChatListChange::Update {chat,..}) if chat.muted_until==0 && chat.show_notification_preview)
    );
    assert!(decode(&mut decoder,r#"{"@type":"updateScopeNotificationSettings","scope":{"@type":"notificationSettingsScopeGroupChats"},"notification_settings":{"mute_for":0,"show_preview":false}}"#).is_empty());
    let changed = decode(
        &mut decoder,
        r#"{"@type":"updateChatUnreadMentionCount","chat_id":7,"unread_mention_count":1}"#,
    );
    assert!(
        matches!(&changed[0],Event::ChatListChanged(ChatListChange::Update {chat,..}) if chat.unread_mentions==1)
    );
    chat["id"] = 8.into();
    chat["type"] = json!({"@type":"chatTypeSupergroup","is_channel":true});
    decode(
        &mut decoder,
        &json!({"@type":"updateNewChat","chat":chat}).to_string(),
    );
    assert!(decode(&mut decoder,r#"{"@type":"updateScopeNotificationSettings","scope":{"@type":"notificationSettingsScopePrivateChats"},"notification_settings":{"mute_for":0,"show_preview":true}}"#).is_empty());
    let changed = decode(
        &mut decoder,
        r#"{"@type":"updateScopeNotificationSettings","scope":{"@type":"notificationSettingsScopeChannelChats"},"notification_settings":{"mute_for":0,"show_preview":true}}"#,
    );
    assert!(
        matches!(&changed[0],Event::ChatListChanged(ChatListChange::Update {chat,..}) if chat.id==ChatId(8) && chat.muted_until==0)
    );
}
#[test]
fn around_history_live_origin_read_completion_and_global_unread_are_distinct() {
    let mut decoder = Decoder::default();
    let message = serde_json::json!({"id":2097152,"chat_id":7,"date":1,"is_outgoing":false,"content":{"@type":"messageText","text":{"text":"Synthetic","entities":[]}}});
    let live = decode(
        &mut decoder,
        &serde_json::json!({"@type":"updateNewMessage","message":message}).to_string(),
    );
    assert!(matches!(&live[0], Event::IncomingMessage(_)));
    let page=decode(&mut decoder,&serde_json::json!({"@type":"messages","messages":[message],"@extra":{"command":"history_around","chat":7,"anchor":1048576}}).to_string());
    assert!(
        matches!(&page[0],Event::MessagesAroundLoaded {anchor:MessageId(1048576),messages,..} if messages.len()==1)
    );
    for (kind, success) in [("ok", true), ("error", false)] {
        assert_eq!(
            decode(
                &mut decoder,
                &serde_json::json!({"@type":kind,"@extra":{"command":"view","chat":7}}).to_string()
            ),
            vec![Event::MessagesViewed {
                chat: ChatId(7),
                success
            }]
        );
    }
    assert_eq!(
        decode(
            &mut decoder,
            r#"{"@type":"updateUnreadMessageCount","chat_list":{"@type":"chatListMain"},"unread_count":123}"#
        ),
        vec![Event::TotalUnreadCount(123)]
    );
}

#[test]
fn actions_map_reply_forward_edit_and_permanent_deletions() {
    use serde_json::json;
    let mut d = Decoder::default();
    let value = json!({"@type":"updateNewMessage","message": {
        "id":20971520000_i64,"chat_id":7,"date":10,"edit_date":11,"is_outgoing":true,
        "reply_to":{"@type":"messageReplyToMessage","chat_id":7,"message_id":20968374272_i64,
            "origin":{"@type":"messageOriginHiddenUser","sender_name":"\u{754c}".repeat(100)},
            "quote":{"text":{"text":"\u{1f642}".repeat(100)}}},
        "forward_info":{"origin":{"@type":"messageOriginHiddenUser","sender_name":"e".repeat(200)}},
        "content":{"@type":"messageText","text":{"text":"updated","entities":[]}}
    }});
    let events = decode(&mut d, &value.to_string());
    let Event::IncomingMessage(m) = &events[0] else {
        panic!("message")
    };
    assert!(m.edited && m.is_text);
    let r = m.reply.as_ref().unwrap();
    assert_eq!(r.message, MessageId(20968374272));
    assert!(
        r.sender.len() <= 256
            && r.preview.len() <= 256
            && m.forwarded.as_ref().unwrap().len() <= 256
    );
    let events = decode(
        &mut d,
        &json!({"@type":"updateMessageContent","chat_id":7,"message_id":20971520000_i64,
        "new_content":{"@type":"messageText","text":{"text":"changed","entities":[]}}})
        .to_string(),
    );
    assert!(matches!(&events[0],Event::MessageContentChanged {text,..} if text=="changed"));
    let events=decode(&mut d,&json!({"@type":"updateMessageEdited","chat_id":7,"message_id":20971520000_i64,"edit_date":12}).to_string());
    assert!(matches!(
        events[0],
        Event::MessageEdited { edited: true, .. }
    ));
    let mut deletion = json!({"@type":"updateDeleteMessages","chat_id":7,"message_ids":[20971520000_i64],"is_permanent":false});
    assert!(decode(&mut d, &deletion.to_string()).is_empty());
    deletion["is_permanent"] = json!(true);
    let events = decode(&mut d, &deletion.to_string());
    assert!(
        matches!(&events[0],Event::MessagesDeleted {messages,..} if messages==&[MessageId(20971520000)])
    );
}
#[test]
fn replied_message_resolves_names_without_a_retained_engine_history() {
    use serde_json::json;
    let mut d = Decoder::default();
    let value = json!({"@type":"message","id":20968374272_i64,"chat_id":7,"date":10,"is_outgoing":true,
        "content":{"@type":"messageText","text":{"text":"Original\nmessage","entities":[]}},
        "@extra":{"command":"replied","chat":7,"message":20971520000_i64}});
    let events = decode(&mut d, &value.to_string());
    assert!(
        matches!(&events[0],Event::ReplyResolved {reply:Some(r),..} if r.sender=="You" && r.preview=="Original message" && r.resolved)
    );
}
