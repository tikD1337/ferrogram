# Performance measurements

Rule: measure first, then form a hypothesis about the cause, then make a change, then measure
again. Results are recorded here together with the conditions. Assumptions do not count as measurements.

## What we measure

| Metric | What it is | Why |
|---|---|---|
| **Private Bytes** | Private committed memory | Main metric: how much memory the process requested from the system |
| **Private Working Set** | Private pages actually resident in RAM | What Task Manager shows |
| **Working Set** | All pages in RAM, including shared DLLs | For reference; inflated by shared libraries |
| **CPU, % of one core** | CPU time over an interval | Usage when idle and under load |
| **Threads, handles** | Count | Indirect indication of leaks and unnecessary background tasks |

Actual heap allocations, GPU memory, frame time and startup time require separate
tools (WPR/WPA, ETW, GPU counters) — introduced at stage 3.

## Tool

```powershell
.\scripts\measure.ps1 -Name <process name> -DurationSec 300 -Csv .\bench\results\<date>-<scenario>.csv
```

## Scenarios

Each scenario is run on both clients, on the same account, after a restart.

1. Startup, before authentication.
2. 2 minutes after sign-in and synchronization, with the chat list open.
3. Idle for 10 minutes, window open.
4. Idle for 10 minutes, minimized to the tray.
5. Large chat open.
6. Scroll up through 500 messages with media and back down.
7. Receive a stream of incoming messages.
8. Run for 4 hours — memory growth.
9. Cold and warm startup until the window appears.

## Results

### 2026-11-15 — feed prototype on Slint 1.18.1

The [spikes/slint-feed](../spikes/slint-feed) prototype: 10 000 synthetic messages of variable
length (Cyrillic, Latin, ideographs, Hebrew, Arabic, emoji), formatted text
through `StyledText`, a 900×700 window, feed anchored to the bottom. Messages are not stored — the model
creates a row by index when requested. Release build, Rust 1.100.0.
Run: `spikes\slint-feed\bench.ps1`. Raw data: `bench/results/2026-11-15-slint-*`.

**Idle, 30 seconds**

| Renderer | Private Bytes | Private WS | Working Set | Threads | CPU | Color emoji |
|---|---|---|---|---|---|---|
| Slint software (`winit-software`) | **7.2 MB** | 6.3 MB | 24.3 MB | 10 | 0 ms | no |
| Skia software (`winit-skia-software`) | 54.1 MB | 52.8 MB | 114.9 MB | 10 | 0 ms | yes |
| FemtoVG, OpenGL (`winit-femtovg`) | 87.4 MB | 45.8 MB | 79.9 MB | 14 | 16 ms | yes |
| Hardware-accelerated Skia (`winit-skia`) | 277.2 MB | 140.8 MB | 231.6 MB | 21 | 16 ms | yes |

**Autoscroll at 3000 pixels per second, 20 seconds**

| Renderer | Private Bytes | CPU, % of one core |
|---|---|---|
| Slint software | 8.2 MB | 16.9 |
| Skia software | 56.3 MB | 16.4 |
| FemtoVG | 98.4 MB | 28.2 |
| Hardware-accelerated Skia | 278.7 MB | 28.7 |

With plain text instead of formatted text, the numbers are almost the same: Slint software — 6.9 MB
when idle and 18.2 % of a core while scrolling, FemtoVG — 86.8 MB and 22.8 %.

**What is confirmed**

- Virtualization works: memory does not depend on the number of messages in the model; while scrolling,
  it grows by 1–11 MB and does not accumulate.
- No CPU usage at all when idle — 0 ms over 30 seconds.
- Slint's software renderer uses 7–38 times less private memory than the others and is no
  worse in CPU usage while scrolling.
- Bottom anchoring and variable message heights work.

**What this measurement does not show**

- Frame time and dropped frames were not measured. Slint's counter showed about 240 frames per
  second at 119 scroll steps per second — frames are therefore not tied to the display refresh rate,
  creating extra work that needs to be limited in the product.
- GPU memory was not measured: some of FemtoVG's and Skia's usage is outside the process.
- The window is small. With the software renderer, frame cost grows with pixel count;
  a maximized window at 4K has not been tested.
- Messages contain no images. Earlier history is not loaded.

### 2026-11-15 — custom message text rendering

The same prototype in `--mode custom`: parley layout, swash rasterization, the result
shown in Slint as an image (code — `spikes/slint-feed/src/text.rs`). Slint's software
renderer, the same 10 000 messages; text additionally includes spoilers and inline images.

| Mode | Private Bytes when idle | Private Bytes while scrolling | CPU while scrolling, % of one core |
|---|---|---|---|
| Slint `StyledText` | 7.2 MB | 8.3 MB | 14.4 |
| Custom rendering | **7.7 MB** | **8.7 MB** | **14.2** |
| Custom rendering with a 6 MB image cache (discarded) | 8.2 MB | 14.4 MB | 14.7 |

Idle CPU in all modes — 0 ms. Files: `*-styled2-*`, `*-custom-final-*`, `*-custom-*`.

**An example of “measure first.”** The first version had a cache of rendered text images. Measurement showed
0 hits across 891 renders over 28 seconds of scrolling and a 6 MB memory increase — exactly the cache
limit: Slint requests a row once when a message appears and keeps the image itself while
it is visible. A control run without the cache (`*-custom-nocache-*`) yielded 8.7 MB at the same CPU usage,
after which the cache was removed. The final table entry already excludes it.

Verified by the prototype's self-check (`--demo-select`): selection returns the message text,
link hit-testing succeeds inside its text and fails outside it.

### 2026-11-15 — reference point, not a baseline measurement

AyuGram (a Telegram Desktop fork), installed on the development machine. 60 seconds,
53 samples, 12 logical cores, 31.6 GB RAM, Windows 11 Pro 26300.
[Raw data](../bench/results/2026-11-15-ayugram-idle.csv).

| Metric | Mean | Min. | Max. |
|---|---|---|---|
| Private Bytes | 858.1 MB | 858.0 | 858.4 |
| Private Working Set | 405.8 MB | 405.5 | 406.1 |
| Working Set | 517.6 MB | 517.4 | 518.0 |
| Threads | 96 | 96 | 97 |
| Handles | 1076 | 1075 | 1077 |
| CPU | 0.18 % of one core | — | — |

**Why this is not a baseline measurement:** this is a fork, rather than the official client; the process started
at 14:03 and was in an unknown state (which chats had been opened, whether the window was minimized);
the scenario is not reproducible. Baseline numbers are collected at stage 0 using the scenarios above.

**What can still be inferred:**

- Memory is on the order of hundreds of megabytes. This is the main opportunity for savings.
- Idle CPU is already low: 109 ms of CPU time per minute. There is little to gain
  here; the task is to avoid doing worse.
- 96 idle threads is a notable reference point for comparison.

## TDLib build

Built on 2026-11-15 with `scripts/build-tdlib.ps1` from the official repository
`github.com/tdlib/td`; dependencies (gperf, OpenSSL, zlib) come from vcpkg.

| Item | Value |
|---|---|
| TDLib version | 1.8.70 |
| Commit | `c8828d50f5eac2fc7089ce1089d7ce9095c61b5e` |
| Compiler | MSVC 14.51 (Build Tools 2026), Release, x64 |
| Build time | 34 minutes: 10 for dependencies, 24 for TDLib, 6 parallel jobs |
| `tdjson.dll` | 28.7 MB, SHA-256 `D3BF5BD97CC5027428DABFEDC1DF8F976FE0C119E64C0E2402F894B463566E39` |
| Runtime DLLs alongside | `libcrypto-3-x64.dll` 5.3 MB, `libssl-3-x64.dll` 0.9 MB, `libz.dll` 0.1 MB |
| Disk usage | 2.8 GB for sources and build tree, plus the vcpkg tree |

The hash identifies this particular build; MSVC builds are not bit-for-bit reproducible,
so another machine will produce a different hash from the same commit. Memory and CPU
of the running library have not been measured yet.

## TDLib running footprint

Measured on 2026-11-15 with `spikes/tdlib-probe`: a console process that contains TDLib
1.8.70 and almost nothing else. Signed in to a real account by QR code, main chat list
loaded (`loadChats`, limit 100), then left idle. Sampled once a second for 10 minutes,
540 samples. Raw data: `bench/results/2026-11-15-tdlib-probe-idle.csv`.

| Metric | Average | Min | Max |
|---|---|---|---|
| Private Bytes | 28.7 MB | 24.3 | 29.6 |
| Private Working Set | 27.4 MB | 23.2 | 28.3 |
| Working Set | 74.9 MB | 70.7 | 75.8 |
| Threads | 7 | 6 | 8 |
| Handles | 134 | 134 | 136 |
| CPU | 0.16 % of one core | — | — |

CPU over the interval: 984 ms of processor time in 601 s. Sign-in to `READY` took 28 s,
most of it the person scanning the code. The encrypted database was 7.2 MB on disk right
after sign-in.

**What this confirms.** The protocol engine costs about 29 MB, far below the 100 MB
threshold set for reconsidering TDLib. Memory grew by about 5 MB in the first minutes and
then stayed flat.

**What it does not show.**

- The account's size (number of chats and contacts) was not recorded, so the figure cannot
  be extrapolated to much larger accounts.
- No chat was opened, no history loaded and no media downloaded.
- The probe parses every event into a generic JSON tree, so its CPU figure is an upper
  bound for a client that reads only the fields it needs.
- Working Set includes the DLL images (TDLib is 28.7 MB on disk), which are shared pages,
  not private memory.
- This is a different process from the Slint prototype. Adding 28.7 MB to the prototype's
  7.7 MB gives a rough expectation of about 36 MB for an idle client with a feed on screen,
  but that sum is an estimate until both run in one process.

## Official Telegram Desktop

Measured on 2026-11-16: Telegram Desktop 7.4.4, started by the owner a few minutes before
the measurement and left running. Sampled once a second for 5 minutes, 260 samples.
Raw data: `bench/results/2026-11-16-tdesktop-idle.csv`.

| Metric | Average | Min | Max |
|---|---|---|---|
| Private Bytes | 655.8 MB | 639.1 | 679.8 |
| Private Working Set | 451.9 MB | 435.2 | 476.2 |
| Working Set | 570.9 MB | 554.1 | 594.7 |
| Threads | 124 | 118 | 128 |
| Handles | 1323 | 1295 | 1336 |
| CPU | 0.88 % of one core | — | — |

CPU over the interval: 2,641 ms of processor time in 301 s.

**How far this can be trusted.** It is the official client on the same machine, which the
earlier AyuGram figure was not. It is still not a controlled baseline: what was on screen,
whether the window was focused or minimized, and whether the owner used it during the
measurement were not recorded, and the scenarios listed above were not followed.

**Comparison, with that caveat.** Our two measured pieces — TDLib (28.7 MB) and the feed
prototype (7.7 MB) — add up to about 36 MB against 656 MB, and to 17 threads against 124.
The pieces have not run in one process and the prototype has no media, avatars, chat list
or stickers, all of which the official client holds in memory. The honest reading is that
the budget is large, not that the final client will be 18 times smaller.

## First live run of the client (2026-11-16)

`tg-app` release build from branch `stage2` (commit `a0bcd1d`), signed in by QR to a real
account, window open with the chat list. 60 s, `scripts/measure.ps1`, same machine. The state was
not controlled: the user may have been using the window during the interval.

| Metric | Avg | Min | Max |
|---|---|---|---|
| Private Bytes, MB | 40.3 | 39.5 | 40.4 |
| Private Working Set, MB | 38.7 | 37.7 | 38.8 |
| Working Set, MB | 128.9 | 127.8 | 129.0 |
| Threads | 9 | 9 | 9 |
| Handles | 340 | 339 | 340 |

CPU: 1062.5 ms in 60.6 s = 1.75 % of one core.

Compared with the official Telegram Desktop 7.4.4 measured earlier on this machine (655.8 MB
Private Bytes, 124 threads, 0.88 % of one core over 5 min): about 16 times less private memory.
CPU is higher here, but the two runs are not comparable (different duration, uncontrolled
activity); a controlled idle measurement is still to be done. The earlier 36 MB figure was an
estimate; 40.3 MB is the first measured value for the whole client.

## Three clients side by side, 10 minutes idle (2026-11-16)

All three signed in to the same account, started by the user and left untouched; measured at the
same time with `scripts/measure.ps1`, 545 samples each. `tg-app` is the release build of `main`
at commit `c05a642`. What each client had on screen was not controlled.

| Metric (average) | tg-app | Telegram Desktop | AyuGram |
|---|---|---|---|
| Private Bytes, MB | 57.5 (56.0-58.7) | 465.0 (445.2-483.5) | 541.6 (540.0-568.5) |
| Private Working Set, MB | 55.2 | 328.9 | 395.7 |
| Working Set, MB | 90.9 | 436.7 | 499.8 |
| Threads | 11 | 80 | 88 |
| Handles | 304 | 1072 | 1008 |
| CPU time in 600 s, ms | 1484 | 2203 | 3297 |
| CPU, % of one core | 0.25 | 0.37 | 0.55 |

Private memory is about 8 times lower than the official client and 9 times lower than AyuGram;
idle CPU is about a third lower than the official client.

`tg-app` started this run at 56 MB, not the 40 MB of the first live run. Its memory stayed flat
during the 10 minutes (56.0-58.7 MB), so nothing leaked while idle, but the higher baseline is
unexplained: a chat with loaded history was probably open. To be investigated together with the
scroll-through-history memory test.

## Video decoding probe: Media Foundation, CPU vs GPU (2026-11-16)

Probe: `spikes/video-probe` (disposable). It encodes a synthetic 1280x720, 30 fps, 10 s H.264
clip with grain (8.15 MB, about 6.5 Mbit/s) through Media Foundation, then decodes it through
the Source Reader to RGB32 at the display size, paced at 30 fps, copying each frame into one
reusable buffer as the client would. "GPU" = a D3D11 device with video support shared through
a DXGI device manager (hardware decoder and scaler); "CPU" = Microsoft's software decoder.
Another build was running at the same time, so CPU figures are per-process CPU time, not wall time.
Media Foundation ships with Windows: no binary size cost.

| Mode, output size | CPU, % of one core | Private Bytes during playback | After release |
|---|---|---|---|
| CPU, 640x360 | 14.4-15.5 | peak 96 MB | 9-10 MB |
| GPU, 640x360 | 4.7-7.0 | peak 120-165 MB | 79-113 MB |
| CPU, 1280x720 | 18.1-21.4 | 99-105 MB, 31-32 threads | 5-27 MB |
| GPU, 1280x720 | 5.6-6.5 | 142-153 MB (private working set 64-68 MB) | 105-141 MB |

Findings:

- The GPU path uses about a third of the CPU time of the software decoder.
- Both paths cost about 100-150 MB of Private Bytes while a video plays, two to three times the
  client's idle footprint. With the GPU most of it is committed driver memory that is not
  resident (private working set about 65 MB).
- After the reader is released, the software path returns to a few MB; the GPU path keeps
  80-140 MB (driver heaps; a second run in the same process did not grow further, so it looks
  like a one-time cost rather than a leak, but this is one sample).
- The software decoder starts about 30 threads.

Consequences for the video milestone (proposal, to confirm when it starts): decode on the GPU
with the software decoder as a fallback; play one video at a time; create the device only for
playback and measure whether a separate player process is needed to give the driver memory
back; limit decoder threads in the software path. D3D12: Media Foundation's Source Reader works
with D3D11; the fixed-function decoder on the GPU is the same block either way. D3D12 Video
needs an own demuxer and bitstream parser, so no saving is expected (not measured).

## Dependency update (2026-11-16)

Slint 1.18.1 -> 1.19.0, parley/fontique 0.11 -> 0.12 (the version Slint 1.19 uses, one copy),
base64 0.22 -> 0.23.1 (one copy instead of two), getrandom 0.3 -> 0.4.3. libloading stays at 0.8:
Slint's GL backend already uses 0.8.9, and 0.9 would add a second copy. TDLib rebuilt at
`5aef7d4036be9bb18e3c78f6709da81f47bb7a70` (still 1.8.70, 39 commits newer); `tdjson.dll`
28,681,216 bytes, build 24 minutes with 4 jobs. Text rendering is byte-identical in demo snapshots.

| Metric (demo) | Before | After |
|---|---|---|
| `tg-app.exe` size | 11,322,880 bytes | 11,971,584 bytes (+5.7 %) |
| Idle 60 s, Private Bytes | 10.0 MB | 9.9 MB |
| Idle 60 s, CPU time | 0 ms | 0 ms |
| `--demo-scroll 120`, Private Bytes avg / max | 40.1 / 43.8 MB | 40.9 / 43.5 MB |
| `--demo-scroll 120`, CPU, % of one core | 10.4 | 9.5 |

The live client with the new TDLib build was not checked yet.

## Video playback in the client (2026-11-16)

The player uses the Media Foundation Media Engine in rendering mode: it decodes, plays the audio
and presents frames into a native child window, so no frame is copied into the UI. Checked with
`ferrogram.exe --demo-video 14` (opens the synthetic 426x240 clip, exits after 14 s), from outside
the process: `MFMediaEngine.dll`, the NVIDIA user-mode driver and the hardware-capable H.264
decoder `msmpeg2vdec.dll` are loaded; the video child window covers the area above the control bar
(1040x676 in a 1040x740 window).

| Metric (8 s sample while the player was open) | Value |
|---|---|
| Private Bytes | 93.7 MB avg, 94.0 MB max |
| Private working set | 46.8 MB avg |
| Threads | 45 avg, 49 max |
| CPU, % of one core | 1.6 |

The demo clip is 4 s long, so most of the sample is after the clip ended; decoding cost while
playing was measured by the probe above (5.6-6.5 % of one core for 720p on the GPU). The video
is downloaded completely before playback; streaming is a later step. Not verified by eye or on
a live account yet.
