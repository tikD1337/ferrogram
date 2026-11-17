//! `--check <json>`: a machine-readable account of what really happened during a run, written at
//! exit, so smoke tests assert facts (chats loaded, video reached two seconds) instead of only an
//! exit code. Holds a few counters; nothing is recorded unless the option is given.
use std::{cell::Cell, fmt::Write, path::Path};
use tg_core::VideoState;

#[derive(Default)]
pub(crate) struct Check {
    video: Track,
    audio: Track,
}

#[derive(Default)]
struct Track {
    events: Cell<u32>,
    max_position_ms: Cell<u32>,
    playing: Cell<bool>,
    failed: Cell<bool>,
}

impl Track {
    fn record(&self, state: &VideoState) {
        self.events.set(self.events.get().saturating_add(1));
        self.max_position_ms
            .set(self.max_position_ms.get().max(state.position_ms));
        self.playing.set(self.playing.get() || state.playing);
        self.failed.set(self.failed.get() || state.failed);
    }
    fn json(&self) -> String {
        format!(
            r#"{{"events":{},"max_position_ms":{},"playing":{},"failed":{}}}"#,
            self.events.get(),
            self.max_position_ms.get(),
            self.playing.get(),
            self.failed.get()
        )
    }
}

/// The interface state at exit.
pub(crate) struct Screen<'a> {
    pub ready: bool,
    pub auth_step: i32,
    pub chats: usize,
    pub messages: usize,
    pub error: &'a str,
}

impl Check {
    /// Voice and round messages carry an audio token; the viewer video does not.
    pub(crate) fn record(&self, state: &VideoState) {
        if state.audio_token.is_some() {
            self.audio.record(state);
        } else {
            self.video.record(state);
        }
    }

    pub(crate) fn write(&self, path: &Path, screen: &Screen<'_>) -> std::io::Result<()> {
        std::fs::write(path, self.json(screen))
    }

    fn json(&self, screen: &Screen<'_>) -> String {
        format!(
            r#"{{"ready":{},"auth_step":{},"chats":{},"messages":{},"error":"{}","video":{},"audio":{}}}"#,
            screen.ready,
            screen.auth_step,
            screen.chats,
            screen.messages,
            escape(screen.error),
            self.video.json(),
            self.audio.json()
        )
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().take(512) {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(token: Option<u64>, position_ms: u32, failed: bool) -> VideoState {
        VideoState {
            audio_token: token,
            position_ms,
            duration_ms: 4_000,
            playing: !failed,
            ended: false,
            failed,
            dismissed: false,
            ..Default::default()
        }
    }

    #[test]
    fn records_video_and_audio_apart_and_writes_valid_json() {
        let check = Check::default();
        check.record(&state(None, 1_200, false));
        check.record(&state(None, 900, false));
        check.record(&state(Some(7), 2_500, false));
        let json = check.json(&Screen {
            ready: true,
            auth_step: 0,
            chats: 5,
            messages: 50,
            error: "Desktop \"tray\"\nunavailable",
        });
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(value["video"]["max_position_ms"], 1_200);
        assert_eq!(value["video"]["events"], 2);
        assert_eq!(value["audio"]["max_position_ms"], 2_500);
        assert_eq!(value["chats"], 5);
        assert_eq!(value["error"], "Desktop \"tray\"\nunavailable");
    }
}
