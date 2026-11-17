# Implementation plan

A stage is complete only when its exit criterion is met. A feature is not declared
supported until it has been implemented and tested.

## Stage 0. Environment and hypothesis validation

- [x] Install Rust, verify that the skeleton builds — 2026-11-15: Rust 1.100.0, MSVC Build Tools 2026;
      `fmt`, `clippy -D warnings`, `test`, `doc` and the release build pass without warnings.
- [x] Obtain `api_id` / `api_hash` at my.telegram.org — 2026-11-15, stored in `.env` (outside git).
- [ ] Baseline measurement of the official Telegram Desktop on this machine following [BENCHMARKS.md](BENCHMARKS.md).
      Partly done 2026-11-16: 5 minutes idle, 656 MB private bytes, 124 threads, 0.88 % of one core;
      uncontrolled state, the scripted scenarios are still to be run.
- [x] Build TDLib from official source — 2026-11-15, `scripts/build-tdlib.ps1`: TDLib 1.8.70,
      commit `c8828d5` (rebuilt at `5aef7d4` on 2026-11-16), 34 minutes, `tdjson.dll` 28.7 MB (details in [BENCHMARKS.md](BENCHMARKS.md)).
- [x] Headless TDLib measurement: authentication, synchronization, 10 minutes of idle time —
      2026-11-15, `spikes/tdlib-probe`: 28.7 MB private bytes, 0.16 % of one core. TDLib confirmed.
- [x] Slint feed prototype: 10 000 synthetic messages, memory, CPU and scrolling measurements —
      2026-11-15, `spikes/slint-feed`. The memory and CPU hypothesis is confirmed; questions remain
      about color emoji and text selection (see [ARCHITECTURE.md](ARCHITECTURE.md), section 3).
- [ ] Resolve the color emoji question: renderer or custom rendering.
- [ ] Extend the prototype: load earlier history without jumps, images in messages, a large window (4K).

**Exit:** the choice of TDLib and Slint is confirmed by measurements or reconsidered.

## Stage 1. Research — completed 2026-11-15

Result: [ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md).

## Stage 2. Minimal working client

- [ ] Engine thread: start TDLib, `td_receive` loop, domain events.
- [ ] Database key protection: TPM + DPAPI (after approval of [SECURITY.md](SECURITY.md)).
- [ ] Authentication: phone number, code, 2FA password.
- [ ] Virtualized chat list.
- [ ] Message viewing: history window, loading by page.
- [ ] Send text.
- [ ] Network and authentication errors are shown to the user rather than lost.

**Exit:** users can sign in, read and write text; all checks (`fmt`, `clippy`, `test`) pass.

## Stage 3. Performance

- [ ] Run all scenarios in [BENCHMARKS.md](BENCHMARKS.md) against the official client.
- [ ] Tray mode that unloads the UI.
- [ ] Cache budgets, event coalescing.
- [ ] Startup time: cold and warm.
- [ ] Memory growth test: 4 hours of operation.

**Exit:** every metric is better than the official client's, and results are recorded.

## Stage 4. Features

Ordered by frequency of use:

1. Images and documents, downloading and sending.
2. Replies, forwarding, editing, deletion.
3. Windows notifications, tray, counters.
4. Reactions, stickers, GIF.
5. Search.
6. Voice messages.
7. Folders, archive, settings, themes.
8. Multiple accounts.
9. Master password, Windows Hello, automatic locking.
10. Privacy settings, secret chats.
11. Calls — a separate decision after everything else.

Each feature comes with a measurement of what it adds to memory and CPU usage.

## Stage 5. Systematic optimization

Profiling (WPR/WPA, ETW, allocation counters), finding bottlenecks, fixing them with
before and after measurements. Build TDLib from source at a pinned commit. Prepare for release.

## User requirement (2026-11-16): seamless history

No "Earlier messages", "Latest messages" or "Load more chats" buttons. History and the chat list
load when the user scrolls near the edge and old pages are dropped from memory when they are far
from the viewport, without visible jumps, flicker or loading pauses. The scroll position must
stay on the same message while pages are added above or removed. Check memory with a scripted
scroll through a long history: it must plateau, not grow.

## Known gap (2026-11-16): albums

A post with several photos (a media album, messages sharing one `media_album_id`) is shown as one
message with the first photo and the caption, followed by the other photos as separate messages
without text. Telegram shows them as one bubble with a photo grid and one caption. To do: group
consecutive messages with the same non-zero `media_album_id` into one feed row, lay the photos
out as a grid like Telegram, keep the caption once, and keep paging and the 300-message window
correct when an album is split across a page boundary.

Done on 2026-11-16: one row
per album of up to 10 items, a Telegram-style grid with centre-cropped cells, one caption, and
an anchor that survives an album split across pages. Not yet checked on a live account.

## User requirement (2026-11-16): full feature set later

The user confirmed the albums and the new look in the demo and asked to record these as future
work, so the client eventually covers what Telegram Desktop offers:

- Video with a built-in player: inline preview in the feed, playback with controls, and a
  full-screen viewer, plus GIF/animations and round video messages. Decoding off the UI thread,
  memory bounded like the image cache; the decoder and its cost must be measured before it is
  chosen.
- Settings: account and profile, privacy and security, notifications, data and storage
  (cache limits), appearance (themes, font size), language, active sessions, folders.
- Gifts (Telegram gifts and Stars), Premium features, reactions, stickers and emoji packs.
- "And all that kind of thing": everything else Telegram Desktop has (polls, voice and video
  calls, channels and comments, bots and inline buttons, forwarding, search, pinned messages,
  scheduled messages), each with the same RAM and CPU discipline.

## Progress (2026-11-16): messages and video

- Reply, edit, delete and forward from a context menu, with reply quotes, "edited" marks and
  forwarded headers.
- Video playback through the Media Foundation Media Engine (GPU decoding, audio, a native child
  window), opened from a preview with a play button and the duration. Next for video: play while
  downloading, GIFs and round video messages, inline autoplay of short muted clips.

## Progress (2026-11-17): media messages

- Voice messages: waveform, seeking and playback through the Media Engine in audio-only mode
  (the system Opus decoder handles OGG/Opus voice notes; no extra library). One voice or round
  message plays at a time, and playback marks it listened.
- Round video messages play inside their circle in the feed: the video child window gets an
  elliptic region clipped to the feed and follows the row while scrolling.
- Files: download with progress and cancel, then open with the default application. Opened files
  get the internet-zone mark (SmartScreen, Protected View), and programs and scripts need a
  confirmation first. Downloaded files can be shown in their folder.
- Static WebP stickers and single animated emoji are drawn without a bubble. Animated (TGS/WebM)
  stickers show a still frame or the emoji until animation lands.
- Service messages (joins, added and removed members, pins, title and photo changes, timers,
  topics, video chats, gifts) read as in Telegram Desktop and are drawn as centred pills, also
  in the chat list preview.
- Release builds no longer open a console window; startup errors show a message box.
- Sign-in asks for the phone number first and requests a QR code only on demand. TDLib rejects
  a phone number once a QR code was requested, so switching back from the QR screen needs a
  logOut and an engine restart; that is the next sign-in task.
- The interface tokens (colours, type sizes, radii) live in the `Theme` global and are recorded
  in `.interface-design/system.md`.

Next: reactions, pinning, message links and selection; a clear recovery screen when the
database key cannot be unlocked; switching from QR back to phone sign-in.

