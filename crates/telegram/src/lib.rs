//! Telegram engine.
//!
//! Owns a TDLib instance and one thread that sleeps in `td_receive` and
//! wakes only on an event. Converts TDLib responses into `tg_core::Event`
//! and accepts `tg_core::Command`. No other crate works with TDLib.
//!
//! Rationale for choosing TDLib: `docs/ARCHITECTURE.md`.

mod chats;
mod engine;
#[allow(unsafe_code)]
mod ffi;
mod mapping;
mod request;

pub use engine::{Engine, EngineParameters};
pub use mapping::Decoder;
