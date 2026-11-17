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
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or("play".into());
    let png = args.next().ok_or("Supply a PNG path")?;
    let media = demo_backend::prepare_media()?;
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
    let ended = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let position = Arc::new(AtomicU32::new(0));
    let (e, f, p) = (ended.clone(), failed.clone(), position.clone());
    let sink = ui.event_sink();
    let player = Rc::new(tg_platform::Player::start(move |s| {
        if s.audio_token.is_some() {
            if s.ended {
                e.store(true, Ordering::Release);
            }
            if s.failed {
                f.store(true, Ordering::Release);
            }
            p.fetch_max(s.position_ms, Ordering::AcqRel);
        }
        sink.post(Event::VideoState(s));
    })?);
    let requests = Rc::new(RefCell::new(Vec::new()));
    let log = requests.clone();
    let native = player.clone();
    ui.on_video(move |r| {
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
    slint::Timer::single_shot(Duration::from_millis(900), move || {
        if let Some(w) = weak.upgrade() {
            let id = MessageId(if action == "emoji" { 19999 } else { 19996 } * 1048576);
            w.set_follow_tail(false);
            state.feed.jump_target.set(Some(id));
            state.refresh(&w);
            if action != "emoji" && action != "preview" {
                state.attachment_click(&w, &id.0.to_string(), 0, 0.0);
            }
            if action == "progress" || action == "cancel" {
                // Deterministic progress before the demo's completion event.
                state.attachment_file(
                    &w,
                    &tg_core::MediaFile {
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
                "offscreen" => w.set_feed_y(w.get_feed_y() + 900.0),
                "chat" => state.open(&w, 1),
                "menu" => w.set_menu_open(true),
                "pause" => state.attachment_click(&w, &(19996_i64 * 1048576).to_string(), 0, 0.0),
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
    if mode == "measure" {
        for millis in [4000, 7000, 10000] {
            let weak = ui.window.as_weak();
            let state = ui.state.clone();
            slint::Timer::single_shot(Duration::from_millis(millis), move || {
                if let Some(w) = weak.upgrade() {
                    state.video(&w, VideoRequest::Seek(0.0));
                }
            });
        }
    }
    let captured = Rc::new(RefCell::new(None));
    let pixels = captured.clone();
    let weak = ui.window.as_weak();
    let delay = if mode == "hold" || mode == "measure" {
        14000
    } else if mode == "ended" {
        6500
    } else if mode == "progress" || mode == "cancel" {
        1000
    } else {
        2400
    };
    slint::Timer::single_shot(Duration::from_millis(delay), move || {
        if let Some(w) = weak.upgrade() {
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
    let active = observed.feed.audio.get();
    let valid = match mode.as_str() {
        "emoji" => (0..observed.feed.row_count())
            .filter_map(|i| observed.feed.row_data(i))
            .any(|r| r.emoji && r.media_width <= 128.0),
        "preview" => !opened,
        "progress" => !opened,
        "cancel" => !opened && active.is_none(),
        "ended" => opened && ended.load(Ordering::Acquire) && receipts.get() == 1,
        "move" => opened && req.iter().any(|r| matches!(r, VideoRequest::Move(_))),
        "chat" | "menu" | "offscreen" => {
            opened && active.is_none() && matches!(req.last(), Some(VideoRequest::Close))
        }
        "pause" => opened && active.is_some_and(|(_, s)| !s.playing),
        "shared" => {
            opened
                && req
                    .iter()
                    .any(|r| matches!(r, VideoRequest::OpenAudio { .. }))
                && receipts.get() == 0
        }
        _ => opened && position.load(Ordering::Acquire) > 500,
    } && !failed.load(Ordering::Acquire);
    println!(
        "mode={mode} inline_open={opened} ended={} failed={} position_ms={} receipts={} requests={} valid={valid}",
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
