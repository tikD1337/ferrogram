// Account-free review of the production inline player, measured feed and shared playback.
extern crate self as tg_ui;
include!("../src/lib.rs");
#[path = "../../app/src/demo.rs"]
#[allow(dead_code)]
mod demo_backend;
use slint::Model;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
// Compare pixels inside the circle against the decoded frame in the same Slint scene.
// The inner square avoids the antialiased mask and progress ring.
fn frame_samples(w: &AppWindow) -> Option<(Vec<u8>, f64, f64)> {
    let frame = w.get_round_frame().to_rgba8()?;
    let preview = w.get_round_preview().to_rgba8();
    let shot = w.window().take_snapshot().ok()?;
    let scale = w.window().scale_factor();
    let (x, y, size) = (
        w.get_round_x() * scale,
        w.get_round_y() * scale,
        w.get_round_size() * scale,
    );
    let mut samples = Vec::new();
    let (mut error, mut preview_error) = (0u64, 0u64);
    for j in 0..24 {
        for i in 0..24 {
            let (u, v) = (0.3 + i as f32 / 23.0 * 0.4, 0.3 + j as f32 / 23.0 * 0.4);
            let (sx, sy) = ((x + u * size) as u32, (y + v * size) as u32);
            if sx >= shot.width() || sy >= shot.height() {
                return None;
            }
            let actual = &shot.as_bytes()[((sy * shot.width() + sx) * 4) as usize..][..3];
            let (fx, fy) = (
                (u * frame.width() as f32) as u32,
                (v * frame.height() as f32) as u32,
            );
            let expected = &frame.as_bytes()[((fy * frame.width() + fx) * 4) as usize..][..3];
            for k in 0..3 {
                error += u64::from(actual[k].abs_diff(expected[k]));
            }
            if let Some(preview) = &preview {
                let (px, py) = (
                    (u * preview.width() as f32) as u32,
                    (v * preview.height() as f32) as u32,
                );
                let old = &preview.as_bytes()[((py * preview.width() + px) * 4) as usize..][..3];
                for k in 0..3 {
                    preview_error += u64::from(actual[k].abs_diff(old[k]));
                }
            }
            samples.extend_from_slice(actual);
        }
    }
    Some((
        samples,
        error as f64 / (24.0 * 24.0 * 3.0),
        preview_error as f64 / (24.0 * 24.0 * 3.0),
    ))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or("play".into());
    if !matches!(
        mode.as_str(),
        "play"
            | "preview"
            | "emoji"
            | "progress"
            | "cancel"
            | "move"
            | "hold"
            | "measure"
            | "floating-measure"
            | "offscreen"
            | "return"
            | "chat"
            | "chat-return"
            | "chat-return-pause"
            | "removed"
            | "other-round"
            | "resize"
            | "menu"
            | "menu-return"
            | "resume"
            | "occluded"
            | "pause"
            | "float-pause"
            | "drag"
            | "drag-close"
            | "shared"
            | "ended"
    ) {
        return Err("Unknown round reference mode".into());
    }
    let png = args.next().ok_or("Supply a PNG path")?;
    let media = demo_backend::prepare_media()?;
    if let Some(clip) = args.next() {
        std::fs::copy(clip, media.with_extension("round.mp4"))?;
    }
    let demo = Rc::new(RefCell::new(demo_backend::Demo::new(media.clone(), false)));
    let backend = demo.clone();
    let receipts = Rc::new(Cell::new(0));
    let count = receipts.clone();
    let hold_download = mode == "progress" || mode == "cancel";
    let ui = Ui::new(
        Options {
            demo: true,
            ..Default::default()
        },
        move |c| {
            if matches!(c, Command::OpenMessageContent { .. }) {
                count.set(count.get() + 1);
            }
            if hold_download && matches!(c, Command::DownloadFileUrgent(_)) {
                return Ok(());
            }
            backend.borrow_mut().command(c)
        },
    )?;
    demo.borrow_mut().sink = Some(ui.event_sink());
    demo.borrow_mut().seed();
    let first = Arc::new(AtomicBool::new(false));
    let first_frame = first.clone();
    let ended = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let position = Arc::new(AtomicU32::new(0));
    let (e, f, p) = (ended.clone(), failed.clone(), position.clone());
    let sink = ui.event_sink();
    let clicked = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    let first_ms = Arc::new(AtomicU32::new(0));
    let frame_count = Arc::new(AtomicU32::new(0));
    let (start_clock, frame_clock, count_frames) =
        (clicked.clone(), first_ms.clone(), frame_count.clone());
    let first_window = ui.window.as_weak();
    let frames = sink.clone();
    let player = Rc::new(tg_platform::Player::start_with_frames(
        move |s| {
            if s.audio_token.is_some() {
                if s.first_frame {
                    first_frame.store(true, Ordering::Release);
                }
                if s.ended {
                    e.store(true, Ordering::Release);
                }
                if s.failed {
                    f.store(true, Ordering::Release);
                }
                p.fetch_max(s.position_ms, Ordering::AcqRel);
            }
            sink.post(Event::VideoState(s));
        },
        move |frame| {
            count_frames.fetch_add(1, Ordering::AcqRel);
            frames.post(Event::RoundFrame(frame));
            if frame_clock.load(Ordering::Acquire) == 0 {
                let clock = frame_clock.clone();
                let start = start_clock.clone();
                let _ = first_window.upgrade_in_event_loop(move |w| {
                    if w.get_round_frame().size().width > 0
                        && let Ok(start) = start.lock()
                        && let Some(start) = *start
                    {
                        let _ = clock.compare_exchange(
                            0,
                            start.elapsed().as_millis().max(1) as u32,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                    }
                });
            }
        },
    )?);
    let startup_preview = Rc::new(Cell::new(false));
    let preview_ok = startup_preview.clone();
    let preview_window = ui.window.as_weak();
    let preview_png = format!("{png}.preview.png");
    let requests = Rc::new(RefCell::new(Vec::new()));
    let log = requests.clone();
    let native = player.clone();
    ui.on_video(move |r| {
        if matches!(r, VideoRequest::OpenRound { .. })
            && let Some(w) = preview_window.upgrade()
        {
            let preview_exists = (0..w.get_messages().row_count())
                .filter_map(|i| w.get_messages().row_data(i))
                .any(|row| {
                    row.round
                        && row
                            .cells
                            .row_data(0)
                            .is_some_and(|cell| cell.image.size().width > 0)
                });
            preview_ok.set(w.get_round_frame().size().width == 0 && preview_exists);
            if let Ok(shot) = w.window().take_snapshot() {
                let _ = image::save_buffer(
                    &preview_png,
                    shot.as_bytes(),
                    shot.width(),
                    shot.height(),
                    image::ColorType::Rgba8,
                );
            }
        }
        if log.borrow().len() < 128 {
            log.borrow_mut().push(r.clone());
        }
        native.request(r).map_err(|_| CommandError {
            code: -4,
            message: "Round playback review failed".into(),
        })
    });
    let weak = ui.window.as_weak();
    let state = ui.state.clone();
    let action = mode.clone();
    let start_clock = clicked.clone();
    slint::Timer::single_shot(Duration::from_millis(900), move || {
        if let Some(w) = weak.upgrade() {
            let id = MessageId(if action == "emoji" { 19999 } else { 19996 } * 1048576);
            w.set_follow_tail(false);
            state.feed.jump_target.set(Some(id));
            state.refresh(&w);
            if action != "emoji" && action != "preview" {
                if let Ok(mut start) = start_clock.lock() {
                    *start = Some(std::time::Instant::now());
                }
                state.attachment_click(&w, &id.0.to_string(), 0, 0.0);
            }
            if action == "progress" || action == "cancel" {
                // Deterministic progress before the demo's completion event.
                state.attachment_file(
                    &w,
                    &tg_core::MediaFile {
                        partial: None,
                        id: 1_000_000 + 19996,
                        path: None,
                        size: 1000,
                        downloaded: 620,
                        downloading: true,
                    },
                );
                if action == "cancel" {
                    state.attachment_click(&w, &id.0.to_string(), 0, 0.0);
                }
            }
        }
    });
    let state = ui.state.clone();
    let weak = ui.window.as_weak();
    let action = mode.clone();
    let log = requests.clone();
    slint::Timer::single_shot(Duration::from_millis(1800), move || {
        if let Some(w) = weak.upgrade() {
            match action.as_str() {
                "move" => w.set_feed_y(w.get_feed_y() + 45.0),
                "offscreen" | "return" | "drag" | "drag-close" | "float-pause"
                | "floating-measure" => w.set_feed_y(w.get_feed_y() + 900.0),
                "chat" | "chat-return" | "chat-return-pause" => state.open(&w, 1),
                "removed" => {
                    state.round_removed(&w, tg_core::ChatId(1), &[MessageId(19996_i64 * 1048576)])
                }
                "other-round" => {
                    state.attachment_click(&w, &(19995_i64 * 1048576).to_string(), 0, 0.0)
                }
                "resize" => w.window().set_size(slint::LogicalSize::new(900.0, 650.0)),
                "menu" | "menu-return" => w.set_menu_open(true),
                "pause" | "resume" => {
                    state.attachment_click(&w, &(19996_i64 * 1048576).to_string(), 0, 0.0)
                }
                "occluded" => {
                    w.window()
                        .with_winit_window(|native| native.set_minimized(true));
                }
                "shared" => {
                    let old = log.borrow().iter().find_map(|r| {
                        if let VideoRequest::OpenRound { token, .. } = r {
                            Some(*token)
                        } else {
                            None
                        }
                    });
                    let Some(old) = old else {
                        return;
                    };
                    let voice = state
                        .feed
                        .history
                        .borrow()
                        .messages
                        .iter()
                        .find(|m| matches!(m.attachment, Some(tg_core::Attachment::Voice(_))))
                        .cloned();
                    let Some(voice) = voice else {
                        return;
                    };
                    state.demo_play_voice(&w, &voice);
                    // Late completion from the replaced round must not send a receipt.
                    state.voice_state(
                        &w,
                        tg_core::VideoState {
                            audio_token: Some(old),
                            ended: true,
                            ..Default::default()
                        },
                    );
                }
                _ => {}
            }
        }
    });
    let weak = ui.window.as_weak();
    let action = mode.clone();
    slint::Timer::single_shot(Duration::from_millis(2300), move || {
        if let Some(w) = weak.upgrade() {
            if action == "return" || action == "chat-return" || action == "chat-return-pause" {
                w.invoke_round_return();
            }
            if action == "resume" {
                w.invoke_attachment_action((19996_i64 * 1048576).to_string().into(), 0, 0.0);
            }
            if action == "occluded" {
                w.window()
                    .with_winit_window(|native| native.set_minimized(false));
            }
            if action == "menu-return" {
                w.set_menu_open(false);
            }
            if action == "drag" || action == "drag-close" || action == "float-pause" {
                let (x, y) = (
                    w.get_round_x() + w.get_round_size() / 2.0,
                    w.get_round_y() + w.get_round_size() / 2.0,
                );
                let (to_x, to_y) = if action == "float-pause" {
                    (x, y)
                } else if action == "drag-close" {
                    (w.get_feed_left() + w.get_feed_view_width() + 200.0, y)
                } else {
                    (
                        w.get_feed_left() + 90.0,
                        w.get_feed_top() + w.get_feed_height() - 100.0,
                    )
                };
                w.invoke_round_pointer(0, x, y);
                w.invoke_round_pointer(1, to_x, to_y);
                w.invoke_round_pointer(2, to_x, to_y);
            }
        }
    });
    let weak = ui.window.as_weak();
    if mode == "chat-return-pause" {
        slint::Timer::single_shot(Duration::from_millis(2900), move || {
            if let Some(w) = weak.upgrade() {
                w.invoke_attachment_action((19996_i64 * 1048576).to_string().into(), 0, 0.0);
            }
        });
    }
    let captured = Rc::new(RefCell::new(None));
    let native_visible = Rc::new(Cell::new(false));
    let visible = native_visible.clone();
    let pixels = captured.clone();
    let weak = ui.window.as_weak();
    let delay = if mode == "hold" || mode == "measure" || mode == "floating-measure" {
        10000
    } else if mode == "ended" {
        14500
    } else if mode == "progress" || mode == "cancel" {
        1000
    } else {
        3200
    };
    let moving_pixels = matches!(
        mode.as_str(),
        "play"
            | "offscreen"
            | "measure"
            | "floating-measure"
            | "return"
            | "chat-return"
            | "resize"
            | "resume"
            | "occluded"
    );
    let earlier = Rc::new(RefCell::new(None));
    let prior = earlier.clone();
    let weak_before = ui.window.as_weak();
    if moving_pixels {
        slint::Timer::single_shot(Duration::from_millis(delay - 400), move || {
            if let Some(w) = weak_before.upgrade() {
                *prior.borrow_mut() = frame_samples(&w);
            }
        });
    }
    let final_samples = Rc::new(RefCell::new(None));
    let sample_copy = final_samples.clone();
    let stopped_pacing = Rc::new(Cell::new(0));
    let count_copy = stopped_pacing.clone();
    let frame_count_copy = frame_count.clone();
    let paused_mode = matches!(mode.as_str(), "pause" | "float-pause" | "menu");
    if paused_mode {
        slint::Timer::single_shot(Duration::from_millis(2600), move || {
            count_copy.set(frame_count_copy.load(Ordering::Acquire));
        });
    }
    let hidden_pacing = Rc::new(Cell::new(true));
    if mode == "occluded" {
        let count = frame_count.clone();
        let last = Rc::new(Cell::new(0));
        let saved = last.clone();
        slint::Timer::single_shot(Duration::from_millis(2050), move || {
            saved.set(count.load(Ordering::Acquire));
        });
        let count = frame_count.clone();
        let result = hidden_pacing.clone();
        slint::Timer::single_shot(Duration::from_millis(2250), move || {
            result.set(last.get() == count.load(Ordering::Acquire));
        });
    }
    let active_before_exit = Rc::new(Cell::new(None));
    let active_copy = active_before_exit.clone();
    let floating_before_exit = Rc::new(Cell::new(false));
    let floating_copy = floating_before_exit.clone();
    let state = ui.state.clone();
    slint::Timer::single_shot(Duration::from_millis(delay), move || {
        active_copy.set(state.feed.audio.get());
        if let Some(w) = weak.upgrade() {
            floating_copy.set(w.get_round_floating());
            if let Some(parent) = native_window(w.window()) {
                let capture = tg_platform::video_window_shown(parent);
                visible.set(capture.is_none());
                println!("native_region={capture:?}");
            }
            *sample_copy.borrow_mut() = frame_samples(&w);
            println!(
                "round_size={} frame_size={:?}",
                w.get_round_size(),
                w.get_round_frame().size()
            );
            let _ = w.window().take_snapshot();
            *pixels.borrow_mut() = Some(w.window().take_snapshot());
        }
        let _ = slint::quit_event_loop();
    });
    let observed = ui.state.clone();
    ui.run()?;
    let req = requests.borrow();
    let opened = req
        .iter()
        .any(|r| matches!(r, VideoRequest::OpenRound { .. }));
    let active = active_before_exit.get();
    let floating = floating_before_exit.get();
    let opens = req
        .iter()
        .filter(|r| matches!(r, VideoRequest::OpenRound { .. }))
        .count();
    let pixels_valid = if moving_pixels {
        let old = earlier.borrow();
        let new = final_samples.borrow();
        match (&*old, &*new) {
            (Some((before, a, pa)), Some((after, b, pb))) => {
                let changed = before
                    .iter()
                    .zip(after)
                    .filter(|(a, b)| a.abs_diff(**b) > 8)
                    .count();
                println!(
                    "decoded_error_before={a:.2} decoded_error_after={b:.2} preview_error_before={pa:.2} preview_error_after={pb:.2} changed_channels={changed}"
                );
                *a < 15.0 && *b < 15.0 && *pa > 15.0 && *pb > 15.0 && changed > 50
            }
            _ => false,
        }
    } else {
        true
    };
    let pacing_valid = hidden_pacing.get()
        && (!paused_mode || stopped_pacing.get() == frame_count.load(Ordering::Acquire));
    let valid = pixels_valid
        && pacing_valid
        && (!opened || startup_preview.get())
        && match mode.as_str() {
            "emoji" => (0..observed.feed.row_count())
                .filter_map(|i| observed.feed.row_data(i))
                .any(|r| r.emoji && r.media_width <= 128.0),
            "preview" => !opened,
            "progress" => !opened,
            "cancel" => !opened && active.is_none(),
            "ended" => opened && ended.load(Ordering::Acquire) && receipts.get() == 1,
            "move" => {
                opened
                    && req
                        .iter()
                        .any(|r| matches!(r, VideoRequest::RoundSurface { .. }))
            }
            "offscreen" | "chat" | "drag" | "floating-measure" => {
                opened && opens == 1 && floating && native_visible.get()
            }
            "return" | "chat-return" => opened && opens == 1 && !floating && native_visible.get(),
            "removed" => opened && active.is_none() && !floating,
            "other-round" => opens == 2 && native_visible.get(),
            "resize" => opened && opens == 1 && native_visible.get(),
            "drag-close" => opened && active.is_none() && !floating,
            "menu" => {
                opened
                    && active.is_some()
                    && req
                        .iter()
                        .any(|r| matches!(r, VideoRequest::RoundSurface { visible: false, .. }))
            }
            "pause" => opened && active.is_some_and(|(_, s)| !s.playing),
            "chat-return-pause" => {
                opened
                    && opens == 1
                    && !floating
                    && native_visible.get()
                    && active.is_some_and(|(_, s)| !s.playing)
            }
            "float-pause" => {
                opened && opens == 1 && floating && active.is_some_and(|(_, s)| !s.playing)
            }
            "menu-return" => opened && opens == 1 && active.is_some() && native_visible.get(),
            "shared" => {
                opened
                    && req
                        .iter()
                        .any(|r| matches!(r, VideoRequest::OpenAudio { .. }))
                    && receipts.get() == 0
            }
            _ => opened && position.load(Ordering::Acquire) > 500 && native_visible.get(),
        }
        && !failed.load(Ordering::Acquire)
        && (!opened || first.load(Ordering::Acquire));
    println!(
        "startup_preview={} pacing_stopped={pacing_valid}",
        startup_preview.get()
    );
    println!(
        "first_frame={} click_to_first_ui_frame_ms={} frame_count={}",
        first.load(Ordering::Acquire),
        first_ms.load(Ordering::Acquire),
        frame_count.load(Ordering::Acquire)
    );
    println!(
        "mode={mode} inline_open={opened} opens={opens} floating={floating} ended={} failed={} position_ms={} receipts={} requests={} valid={valid}",
        ended.load(Ordering::Acquire),
        failed.load(Ordering::Acquire),
        position.load(Ordering::Acquire),
        receipts.get(),
        req.len()
    );
    let pixels = captured.borrow_mut().take().ok_or("No snapshot")??;
    image::save_buffer(
        png,
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )?;
    drop(req);
    drop(observed);
    drop(player);
    demo_backend::cleanup_media(&media)?;
    if !valid {
        return Err("Round review assertion failed".into());
    }
    Ok(())
}
