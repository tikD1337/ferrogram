//! Text of service messages ("Anna joined the group via invite link"), worded as in Telegram
//! Desktop. The feed draws them as centred pills instead of bubbles.
use crate::mapping::Users;
use serde_json::Value;

/// Bound of one service text: a few names and a quoted title.
pub(crate) const MAX_SERVICE_BYTES: usize = 1024;
/// Names listed in "Anna added Boris, Vera and 3 others".
const LISTED_NAMES: usize = 3;

/// `actor` is "You", the sender's name, or the chat title when the chat itself sent the message.
/// `channel` selects the wording for broadcast channels. Returns `None` for ordinary content.
pub(crate) fn text(
    content: &Value,
    actor: &str,
    channel: bool,
    sender_id: i64,
    users: &Users,
) -> Option<String> {
    let place = if channel { "channel" } else { "group" };
    let user = |field: &str| name(users, content[field].as_i64().unwrap_or(0));
    let quoted = |field: &str| one_line(content[field].as_str().unwrap_or(""));
    let flag = |field: &str| content[field].as_bool().unwrap_or(false);
    let text = match content["@type"].as_str()? {
        "messageChatJoinByLink" => format!("{actor} joined the {place} via invite link"),
        "messageChatJoinByRequest" => format!("{actor} joined the {place}"),
        "messageChatAddMembers" => {
            let ids = content["member_user_ids"].as_array()?;
            if ids.len() == 1 && ids[0].as_i64() == Some(sender_id) {
                format!("{actor} joined the {place}")
            } else {
                format!("{actor} added {}", list(ids, users))
            }
        }
        "messageChatDeleteMember" => {
            let id = content["user_id"].as_i64().unwrap_or(0);
            if id == sender_id {
                format!("{actor} left the {place}")
            } else {
                format!("{actor} removed {}", name(users, id))
            }
        }
        "messageBasicGroupChatCreate" => {
            format!("{actor} created the group «{}»", quoted("title"))
        }
        "messageSupergroupChatCreate" if channel => "Channel created".into(),
        "messageSupergroupChatCreate" => {
            format!("{actor} created the group «{}»", quoted("title"))
        }
        "messageChatChangeTitle" if channel => format!("Channel renamed to «{}»", quoted("title")),
        "messageChatChangeTitle" => {
            format!("{actor} changed the group name to «{}»", quoted("title"))
        }
        "messageChatChangePhoto" if channel => "Channel photo updated".into(),
        "messageChatChangePhoto" => format!("{actor} updated the group photo"),
        "messageChatDeletePhoto" if channel => "Channel photo removed".into(),
        "messageChatDeletePhoto" => format!("{actor} removed the group photo"),
        "messageChatUpgradeTo" | "messageChatUpgradeFrom" => {
            "The group was upgraded to a supergroup".into()
        }
        "messagePinMessage" => format!("{actor} pinned a message"),
        "messageScreenshotTaken" => format!("{actor} took a screenshot"),
        "messageChatSetTheme" => format!("{actor} changed the chat theme"),
        "messageChatSetBackground" => format!("{actor} changed the chat wallpaper"),
        "messageChatSetMessageAutoDeleteTime" => {
            match content["message_auto_delete_time"].as_i64().unwrap_or(0) {
                seconds if seconds <= 0 => format!("{actor} disabled the auto-delete timer"),
                seconds => format!("{actor} set messages to auto-delete in {}", period(seconds)),
            }
        }
        "messageChatOwnerChanged" => format!(
            "{actor} transferred ownership to {}",
            user("new_owner_user_id")
        ),
        "messageChatOwnerLeft" => match content["new_owner_user_id"].as_i64().unwrap_or(0) {
            0 => format!("{actor} left the {place}"),
            id => format!(
                "{actor} left the {place}, {} is the new owner",
                name(users, id)
            ),
        },
        "messageChatHasProtectedContentToggled" if flag("new_has_protected_content") => {
            format!("{actor} restricted saving content")
        }
        "messageChatHasProtectedContentToggled" => format!("{actor} allowed saving content"),
        "messageChatBoost" => match content["boost_count"].as_i64().unwrap_or(1) {
            count if count <= 1 => format!("{actor} boosted the {place}"),
            count => format!("{actor} boosted the {place} {count} times"),
        },
        "messageVideoChatScheduled" => format!("{actor} scheduled a video chat"),
        "messageVideoChatStarted" if channel => "Live stream started".into(),
        "messageVideoChatStarted" => format!("{actor} started a video chat"),
        "messageVideoChatEnded" => format!(
            "Video chat ended ({})",
            period(content["duration"].as_i64().unwrap_or(0))
        ),
        "messageInviteVideoChatParticipants" => format!(
            "{actor} invited {} to the video chat",
            list(content["user_ids"].as_array()?, users)
        ),
        "messageForumTopicCreated" => format!("{actor} created the topic «{}»", quoted("name")),
        "messageForumTopicEdited" if !quoted("name").is_empty() => {
            format!("{actor} renamed the topic to «{}»", quoted("name"))
        }
        "messageForumTopicEdited" => format!("{actor} changed the topic icon"),
        "messageForumTopicIsClosedToggled" if flag("is_closed") => {
            format!("{actor} closed the topic")
        }
        "messageForumTopicIsClosedToggled" => format!("{actor} reopened the topic"),
        "messageForumTopicIsHiddenToggled" if flag("is_hidden") => {
            format!("{actor} hid the topic")
        }
        "messageForumTopicIsHiddenToggled" => format!("{actor} unhid the topic"),
        "messageContactRegistered" => format!("{actor} joined Telegram"),
        "messageCustomServiceAction" => quoted("text"),
        "messageGameScore" => format!("{actor} scored {}", content["score"].as_i64().unwrap_or(0)),
        "messageGift" | "messageUpgradedGift" | "messageRefundedUpgradedGift" => {
            format!("{actor} sent a gift")
        }
        "messageGiftedPremium" | "messagePremiumGiftCode" => {
            format!("{actor} gifted Telegram Premium")
        }
        "messageGiftedStars" => format!(
            "{actor} gifted {} Stars",
            content["star_count"].as_i64().unwrap_or(0)
        ),
        "messageGiveawayCreated" => format!("{actor} launched a giveaway"),
        "messageGiveawayCompleted" => "Giveaway completed".into(),
        "messagePaymentSuccessful" | "messagePaymentSuccessfulBot" => "Payment completed".into(),
        "messageBotWriteAccessAllowed" => "You allowed this bot to message you".into(),
        "messageWebAppDataSent" => format!(
            "Data from the «{}» button was sent to the bot",
            quoted("button_text")
        ),
        "messageUsersShared" => "You shared a user".into(),
        "messageChatShared" => "You shared a chat".into(),
        "messageSuggestProfilePhoto" => format!("{actor} suggested a profile photo"),
        "messageSuggestBirthdate" => format!("{actor} suggested a birthday"),
        "messagePassportDataSent" => "Telegram Passport data sent".into(),
        "messageExpiredPhoto" => "Photo has expired".into(),
        "messageExpiredVideo" => "Video has expired".into(),
        "messageExpiredVideoNote" => "Video message has expired".into(),
        "messageExpiredVoiceNote" => "Voice message has expired".into(),
        _ => return None,
    };
    Some(crate::mapping::truncate(text, MAX_SERVICE_BYTES))
}

/// The display name of a user; deleted accounts have an empty name in TDLib.
pub(crate) fn name(users: &Users, id: i64) -> &str {
    match users.name(id) {
        Some("") => "Deleted Account",
        Some(name) => name,
        None => "Unknown user",
    }
}

fn list(ids: &[Value], users: &Users) -> String {
    let names: Vec<&str> = ids
        .iter()
        .take(LISTED_NAMES)
        .map(|id| name(users, id.as_i64().unwrap_or(0)))
        .collect();
    let rest = ids.len().saturating_sub(LISTED_NAMES);
    match (names.as_slice(), rest) {
        ([], _) => "nobody".into(),
        ([one], 0) => (*one).into(),
        (names, 0) => {
            let (last, head) = names.split_last().unwrap_or((&"", &[]));
            format!("{} and {last}", head.join(", "))
        }
        (names, 1) => format!("{} and 1 other", names.join(", ")),
        (names, rest) => format!("{} and {rest} others", names.join(", ")),
    }
}

fn one_line(text: &str) -> String {
    crate::mapping::truncate(text.replace(['\r', '\n'], " "), 256)
}

/// "5 minutes", "1 day", "2 weeks": the largest whole unit, as Telegram shows timers.
fn period(seconds: i64) -> String {
    let (count, unit) = [
        (365 * 86_400, "year"),
        (30 * 86_400, "month"),
        (7 * 86_400, "week"),
        (86_400, "day"),
        (3_600, "hour"),
        (60, "minute"),
    ]
    .into_iter()
    .find(|&(size, _)| seconds >= size)
    .map_or((seconds.max(0), "second"), |(size, unit)| {
        (seconds / size, unit)
    });
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn users() -> Users {
        let mut users = Users::default();
        for (id, first, last) in [
            (777_000_001, "Anna", "Petrova"),
            (777_000_002, "Boris", ""),
            (777_000_003, "", ""),
            (777_000_004, "Vera", ""),
            (777_000_005, "Gleb", ""),
        ] {
            users
                .apply(&json!({"id": id, "first_name": first, "last_name": last}))
                .expect("user");
        }
        users
    }

    fn say(content: Value, actor: &str, channel: bool, sender: i64) -> Option<String> {
        text(&content, actor, channel, sender, &users())
    }

    #[test]
    fn joins_and_leaves_name_the_actor() {
        let anna = 777_000_001;
        assert_eq!(
            say(
                json!({"@type": "messageChatJoinByLink"}),
                "Anna Petrova",
                false,
                anna
            )
            .as_deref(),
            Some("Anna Petrova joined the group via invite link")
        );
        let added_self = json!({"@type": "messageChatAddMembers", "member_user_ids": [anna]});
        assert_eq!(
            say(added_self, "Anna Petrova", false, anna).as_deref(),
            Some("Anna Petrova joined the group")
        );
        let left = json!({"@type": "messageChatDeleteMember", "user_id": anna});
        assert_eq!(
            say(left, "You", true, anna).as_deref(),
            Some("You left the channel")
        );
        let removed = json!({"@type": "messageChatDeleteMember", "user_id": 777_000_003});
        assert_eq!(
            say(removed, "You", false, 1).as_deref(),
            Some("You removed Deleted Account")
        );
    }

    #[test]
    fn added_members_are_listed_with_a_bounded_count() {
        let two = json!({"@type": "messageChatAddMembers",
            "member_user_ids": [777_000_002, 777_000_004]});
        assert_eq!(
            say(two, "Anna", false, 777_000_001).as_deref(),
            Some("Anna added Boris and Vera")
        );
        let many = json!({"@type": "messageChatAddMembers", "member_user_ids":
            [777_000_002, 777_000_004, 777_000_005, 777_000_009, 777_000_010]});
        assert_eq!(
            say(many, "Anna", false, 777_000_001).as_deref(),
            Some("Anna added Boris, Vera, Gleb and 2 others")
        );
    }

    #[test]
    fn channels_use_impersonal_wording() {
        let title = json!({"@type": "messageChatChangeTitle", "title": "News\nDaily"});
        assert_eq!(
            say(title.clone(), "News", true, -100).as_deref(),
            Some("Channel renamed to «News Daily»")
        );
        assert_eq!(
            say(title, "Anna", false, 1).as_deref(),
            Some("Anna changed the group name to «News Daily»")
        );
        assert_eq!(
            say(
                json!({"@type": "messageChatChangePhoto"}),
                "News",
                true,
                -100
            )
            .as_deref(),
            Some("Channel photo updated")
        );
    }

    #[test]
    fn timers_use_the_largest_whole_unit() {
        let timer = |seconds: i64| {
            say(
                json!({"@type": "messageChatSetMessageAutoDeleteTime",
                    "message_auto_delete_time": seconds}),
                "Anna",
                false,
                1,
            )
        };
        assert_eq!(
            timer(0).as_deref(),
            Some("Anna disabled the auto-delete timer")
        );
        assert_eq!(
            timer(86_400).as_deref(),
            Some("Anna set messages to auto-delete in 1 day")
        );
        assert_eq!(
            timer(14 * 86_400).as_deref(),
            Some("Anna set messages to auto-delete in 2 weeks")
        );
        assert_eq!(period(45), "45 seconds");
    }

    #[test]
    fn ordinary_and_unknown_content_is_not_a_service_message() {
        for kind in [
            "messageText",
            "messagePhoto",
            "messageUnsupported",
            "messageSomethingNew",
        ] {
            assert_eq!(say(json!({"@type": kind}), "Anna", false, 1), None);
        }
        assert_eq!(say(json!({}), "Anna", false, 1), None);
    }

    #[test]
    fn custom_text_is_bounded() {
        let long = "x".repeat(10_000);
        let action = json!({"@type": "messageCustomServiceAction", "text": long});
        assert!(say(action, "Anna", false, 1).is_some_and(|t| t.len() <= 256));
        let title = "й".repeat(2_000);
        let create = json!({"@type": "messageBasicGroupChatCreate", "title": title});
        assert!(
            say(create, &"Ж".repeat(600), false, 1).is_some_and(|t| t.len() <= MAX_SERVICE_BYTES)
        );
    }
}
