//! Commands: from the UI to the engine.

use crate::{ChatId, MessageId, Secret};

/// Non-secret command identity for associating an error with a UI action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    ViewMessages,
    DownloadFile,
    CancelDownload,
    RequestQrSignIn,
    SubmitPhoneNumber,
    SubmitCode,
    SubmitPassword,
    LoadMoreChats,
    OpenChat,
    CloseChat,
    LoadOlderMessages,
    LoadNewerMessages,
    SendText,
    LogOut,
    ShutDown,
    Initialize,
}

/// A user action for the engine to perform.
///
/// Commands are moved rather than copied: `Clone` is deliberately absent so that secrets
/// inside them exist in a single instance.
#[derive(Debug)]
#[non_exhaustive]
pub enum Command {
    /// Report only actually visible incoming IDs; at most 50 per request.
    ViewMessages {
        chat: ChatId,
        messages: Vec<MessageId>,
    },
    /// Open around a real last-read ID; zero is valid for an entirely unread chat.
    OpenChatAt {
        chat: ChatId,
        anchor: MessageId,
    },
    /// Only the viewport media controller requests files.
    DownloadFile(i32),
    CancelDownload(i32),
    /// Request a QR link to confirm on another signed-in device.
    RequestQrSignIn,
    /// Phone number for sign-in.
    SubmitPhoneNumber(Secret),
    /// Verification code from Telegram or SMS.
    SubmitCode(Secret),
    /// Two-factor authentication password.
    SubmitPassword(Secret),
    /// Load the next 50 main-list chats. No automatic prefetching.
    LoadMoreChats,
    /// Open a chat and request its latest page of messages.
    OpenChat(ChatId),
    CloseChat(ChatId),
    /// Load up to 50 messages before this ID (zero starts at the newest message).
    LoadOlderMessages {
        chat: ChatId,
        before: MessageId,
    },
    /// Load messages strictly newer than a real, positive message ID.
    LoadNewerMessages {
        chat: ChatId,
        after: MessageId,
    },
    /// Send a text message.
    SendText {
        chat: ChatId,
        text: String,
    },
    LogOut,
    /// Start closing; joining is a separate, blocking engine operation.
    ShutDown,
}
