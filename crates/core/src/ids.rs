//! Typed identifiers: the compiler prevents confusing a chat with a message.

/// Chat identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub i64);

/// Message identifier. Unique only within its chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub i64);

/// User identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub i64);
