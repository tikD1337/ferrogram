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
- [x] Resolve the color emoji question: renderer or custom rendering — 2026-11-15: custom message
      text rendering with parley and swash (ARCHITECTURE.md, section 3).
- [x] Extend the prototype: load earlier history without jumps, images in messages — done in the
      product instead (2026-11-16). A 4K window has not been measured.

**Exit:** the choice of TDLib and Slint is confirmed by measurements or reconsidered.

## Stage 1. Research — completed 2026-11-15

Result: [ARCHITECTURE.md](ARCHITECTURE.md), [SECURITY.md](SECURITY.md).

## Stage 2. Minimal working client

- [x] Engine thread: start TDLib, `td_receive` loop, domain events.
- [x] Database key protection: TPM + DPAPI (after approval of [SECURITY.md](SECURITY.md)).
- [x] Authentication: phone number, code, 2FA password, and QR code. Phone sign-in confirmed on a
      live account 2026-11-17.
- [x] Virtualized chat list.
- [x] Message viewing: history window, loading by page.
- [x] Send text.
- [x] Network and authentication errors are shown to the user rather than lost.

Done 2026-11-16, used daily on a live account since then.

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

1. Images and documents, downloading and sending. Downloading and viewing done (photos 2026-11-16,
   files 2026-11-17); sending media is not done.
2. Replies, forwarding, editing, deletion. Done 2026-11-16.
3. Windows notifications, tray, counters. Notifications and counters done 2026-11-16; a tray mode
   that unloads the UI is not done.
4. Reactions, stickers, GIF. Static stickers done 2026-11-17; reactions, animated stickers and GIF
   are not done.
5. Search.
6. Voice messages. Playback done 2026-11-17; recording is not done.
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


## Security work (2026-11-17)

A review of [SECURITY.md](SECURITY.md) against the code. Implemented today: the database key is
wrapped by a non-exportable TPM key and DPAPI (threat T1, copied files), and our copy of the key is
zeroed after it is handed to TDLib. The TPM key is not bound to boot state (PCRs), a PIN or a user
gesture, so any process running as the same Windows user can ask the TPM to unwrap it. Planned,
in order of value for cost:

1. **Process hardening (T3, client running).** A process DACL that denies memory reads and thread
   injection to other processes of the same user; `SetProcessMitigationPolicy` (no dynamic code,
   no extension points, signed DLLs only, as far as TDLib and the renderer allow);
   `SetDefaultDllDirectories` for a safe DLL search order. Measure that the client and TDLib still
   start and that RAM and CPU do not change.
2. **Lock the key page in memory** with `VirtualLock`, so the key never reaches the page file. The
   code only zeroes it today (SECURITY.md now says so).
3. **Recovery when the key cannot be unlocked.** Today a TPM refusal left a bare error code. Show a
   clear screen instead: move the old key and database to a dated backup folder and sign in again;
   the chats come back from the server. Nothing is deleted without the user's confirmation.
4. **Master password (T2, client closed).** Optional, off by default (decided 2026-11-16). Argon2id
   from an audited RustCrypto implementation, wrapping added as the third layer; asked at start.
   It is the only measure that stops malware running as the same user from unwrapping the key.
5. **Windows Hello as an alternative to the password (T2).** Research first: the Windows Hello key
   API signs and does not decrypt. Evaluate deriving a key from a signature of a fixed challenge, or
   a TPM key that requires user presence, against published attacks before choosing.
6. **Automatic locking.** An idle timer that removes keys from memory: interface-only lock by
   default (notifications keep working), full lock (TDLib shut down) as a setting.
7. **Copy test.** A repeatable test that a copied data folder does not open in another Windows
   profile or on another computer (T1), and that a wrong key never opens the database.

Out of scope, as decided in SECURITY.md: a Chrome-style SYSTEM service for application binding,
and anti-debugging or obfuscation. Malware with administrator or kernel rights (T4) cannot be
stopped by any user-mode client.

## Documented but not yet planned (2026-11-17)

Promises and open items found in the other documents that had no place in this plan.

**Text and emoji** (ARCHITECTURE.md, sections 3 and 7)

- Text selection inside messages and copying the selection. The prototype had it; the client
  only copies a whole message from the menu.
- The Apple emoji set, loaded from a local directory and never committed, plus a freely licensed
  fallback set for public builds. Today only an optional emoji font (`TG_EMOJI_FONT`) is supported.
- Custom emoji inside text (stickers in the line).
- Screen reader accessibility: message text is drawn as an image, so its text must be exposed
  separately.

**Themes** (ARCHITECTURE.md, section 7)

- Finish the OLED theme: chat list, panels, hover and selection states, and no large static bright
  areas that could burn in.

**Security** (SECURITY.md)

- Run `cargo audit` and `cargo deny` in CI.
- Recommend two-step verification in the interface, next to the list of active sessions.
- Check what happens to the key after a TPM reset or a firmware update, and on a machine without
  a TPM (the DPAPI-only fallback with its warning).
- TDLib does not encrypt downloaded files. Decide whether to accept and document this, or to keep
  the cache in a protected location and clear it on lock.

**Release** (ARCHITECTURE.md, section 6)

- How users get `tdjson.dll`: release builds from official TDLib source at a pinned commit, with
  the hash recorded and verified.
- Review the Telegram API terms for third-party clients before the first public release.
- A distributable package (an archive or an installer) and release notes.


## User review (2026-11-17): what the client lacks next to Telegram Desktop

The user compared Ferrogram with AyuGram (the same interface as Telegram Desktop) side by side on a
live account; the screenshot stays outside the repository because it shows private chats. The
client "feels very empty". Each item is designed from Telegram Desktop's own sources first (the
same behaviour or measurably better), with its RAM and CPU cost measured.

**Messages**

- Animated stickers. TGS (Lottie) and WebM stickers play; today only their first frame or
  thumbnail is drawn. Animation runs only for visible stickers in the active window.
- Working links. URLs, `t.me` links (open the chat or post inside the client), mentions and
  hashtags are clickable; link previews under messages.
- Group feeds: sender avatars beside messages, admin and custom title badges, reactions under
  messages, the pinned message bar at the top of the chat.

**Composer**

- A panel with emoji, stickers (installed sets and recent) and GIFs (saved and search), opened
  from the composer, as in Telegram.
- No input field where the user cannot write. In a channel without posting rights, show a Mute or
  Unmute bar, or Join when not subscribed. Groups with restricted rights show the restriction.

**Navigation**

- The main menu behind the three lines, as in Telegram: account header, My profile, Settings,
  Saved Messages, night mode and the other entries, leading to real settings screens.
- The folder bar (All chats and the user's folders) and a search field above the chat list.
- Chat list rows as in Telegram: "You:" before own last messages, read checks, pinned and muted
  marks, chat type and verified icons, drafts.

**Profiles**

- A click on the avatar or title in the chat header opens the profile: for a user, a group or a
  channel, with the description, members or subscribers, shared media and links. The header shows
  the member count or the online status under the title, plus search in chat and the other actions.
