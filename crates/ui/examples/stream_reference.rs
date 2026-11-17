//! Account-free end-to-end viewer streaming and first-decoded-frame timing.
extern crate self as tg_ui;
include!("../src/lib.rs");
#[path = "../../app/src/demo.rs"]
#[allow(dead_code)]
mod demo_backend;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Measurements {
    click: Option<std::time::Instant>,
    first_ms: Option<u128>,
    first_progress_ms: Option<u128>,
    partial_position_ms: u32,
    max_position_ms: u32,
    last_native_playing: bool,
    partial_first: bool,
    ended: bool,
    failed: bool,
    waits: u32,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or("stream".into());
    let directory = PathBuf::from(args.next().ok_or("Supply output directory")?);
    std::fs::create_dir_all(&directory)?;
    let media = demo_backend::prepare_media()?;
    let backend = Rc::new(RefCell::new(demo_backend::Demo::new(media.clone(), false)));
    let commands = backend.clone();
    let ranges = Rc::new(RefCell::new(Vec::new()));
    let requested = ranges.clone();
    let range_mode = mode == "ranges";
    let ui = Ui::new(
        Options {
            demo: true,
            ..Default::default()
        },
        move |command| {
            if let Command::DownloadRange { file, offset } = &command {
                requested.borrow_mut().push((*file, *offset));
            }
            if range_mode && matches!(command, Command::DownloadRange { .. }) {
                return Ok(());
            }
            commands.borrow_mut().command(command)
        },
    )?;
    backend.borrow_mut().sink = Some(ui.event_sink());
    backend.borrow_mut().seed();
    let stats = Arc::new(Mutex::new(Measurements::default()));
    let completed = Arc::new(AtomicBool::new(false));
    let sampled = stats.clone();
    let downloaded = completed.clone();
    let sink = ui.event_sink();
    let player = Rc::new(tg_platform::Player::start(move |s| {
        if let Ok(mut m) = sampled.lock() {
            if s.first_frame && m.first_ms.is_none() {
                m.first_ms = m.click.map(|c| c.elapsed().as_millis());
                m.partial_first = !downloaded.load(Ordering::Acquire);
            }
            if s.position_ms >= 100 && m.first_progress_ms.is_none() {
                m.first_progress_ms = m.click.map(|c| c.elapsed().as_millis());
            }
            if !downloaded.load(Ordering::Acquire) {
                m.partial_position_ms = m.partial_position_ms.max(s.position_ms);
            }
            if s.stream_token.is_some() {
                m.last_native_playing = s.playing;
            }
            m.max_position_ms = m.max_position_ms.max(s.position_ms);
            m.ended |= s.ended;
            m.failed |= s.failed;
            if s.stream.is_some_and(|s| s.waiting) {
                m.waits += 1;
            }
        }
        sink.post(Event::VideoState(s));
    })?);
    let native = player.clone();
    let downloaded = completed.clone();
    let native_requests = Rc::new(Cell::new(0u32));
    let opened = native_requests.clone();
    let no_native = mode == "ranges";
    ui.on_video(move |request| {
        match &request {
            VideoRequest::Availability { file, .. } => {
                if file.completed {
                    downloaded.store(true, Ordering::Release);
                }
            }
            VideoRequest::Open { .. } => {
                downloaded.store(true, Ordering::Release);
            }
            VideoRequest::OpenStream { .. } => opened.set(opened.get() + 1),
            _ => {}
        }
        if no_native {
            return Ok(());
        }
        native.request(request).map_err(|_| CommandError {
            code: -4,
            message: "Streaming review player failed".into(),
        })
    });
    let weak = ui.window.as_weak();
    let state = ui.state.clone();
    let action = mode.clone();
    let start = stats.clone();
    slint::Timer::single_shot(Duration::from_millis(900), move || {
        if let Some(w) = weak.upgrade()
            && let Some(mut video) = state.feed.newest_video()
        {
            if action == "ranges" {
                video.file.size = 8 * tg_core::STREAM_PRELOAD;
                video.file.partial = None;
            }
            if action == "full" {
                video.supports_streaming = false;
            }
            if let Ok(mut s) = start.lock() {
                s.click = Some(std::time::Instant::now());
            }
            state.play(&w, video);
        }
    });
    let close_time = Rc::new(Cell::new(0u128));
    let measured_close = close_time.clone();
    let weak = ui.window.as_weak();
    let state = ui.state.clone();
    let action = mode.clone();
    slint::Timer::single_shot(
        Duration::from_millis(
            if mode == "close" || mode == "early-seek" || mode == "switch" || mode == "ranges" {
                1700
            } else {
                5500
            },
        ),
        move || {
            if let Some(w) = weak.upgrade() {
                if action == "ranges" {
                    let data = state
                        .streaming
                        .borrow()
                        .as_ref()
                        .map(|s| (s.file.id, s.token));
                    if let Some((file, token)) = data {
                        for _ in 0..2 {
                            state.stream_status(
                                &w,
                                tg_core::StreamStatus {
                                    file,
                                    token,
                                    needed: Some(3 * tg_core::STREAM_PRELOAD + 17),
                                    waiting: true,
                                },
                            );
                        }
                    }
                } else if action == "switch" {
                    if let Some(mut video) = state.feed.newest_video() {
                        video.file.id += 1;
                        state.play(&w, video);
                    }
                } else if action == "seek" || action == "early-seek" || action == "pause-seek" {
                    if action == "pause-seek" {
                        state.viewer.invoke_toggle();
                    }
                    state.viewer.invoke_seek(0.85);
                } else if action == "close" {
                    let start = std::time::Instant::now();
                    state.close_video(&w);
                    measured_close.set(start.elapsed().as_millis());
                }
            }
        },
    );
    let captured = Rc::new(RefCell::new(None));
    let pixels = captured.clone();
    let viewer = ui.state.viewer.as_weak();
    let main = ui.window.as_weak();
    let action = mode.clone();
    let snapshot_ms = match mode.as_str() {
        "buffer" => 1800,
        "close" => 2100,
        "seek" | "pause-seek" => 6500,
        "early-seek" => 4400,
        "ranges" => 2100,
        "full" => 9000,
        "stream" => 6500,
        _ => 4500,
    };
    slint::Timer::single_shot(Duration::from_millis(snapshot_ms), move || {
        if action == "close" {
            if let Some(w) = main.upgrade() {
                let _ = w.window().take_snapshot();
                *pixels.borrow_mut() = Some(w.window().take_snapshot());
            }
        } else if let Some(v) = viewer.upgrade() {
            let _ = v.window().take_snapshot();
            *pixels.borrow_mut() = Some(v.window().take_snapshot());
        }
    });
    let duration = if mode == "buffer" || mode == "close" || mode == "ranges" {
        2500
    } else {
        12000
    };
    slint::Timer::single_shot(Duration::from_millis(duration), || {
        let _ = slint::quit_event_loop();
    });
    let state = ui.state.clone();
    ui.run()?;
    drop(player);
    let m = stats.lock().map_err(|_| "Measurement lock")?;
    let valid = !m.failed
        && m.max_position_ms <= 4100
        && if mode == "full" {
            m.first_ms.is_some() && !m.partial_first && native_requests.get() == 0
        } else if mode == "buffer" {
            m.first_ms.is_none() && native_requests.get() == 1
        } else if mode == "ranges" {
            ranges.borrow().iter().map(|r| r.1).collect::<Vec<_>>()
                == [0, 3 * tg_core::STREAM_PRELOAD]
        } else if mode == "switch" {
            m.first_ms.is_some() && native_requests.get() == 2
        } else if mode == "pause-seek" {
            m.first_ms.is_some() && !m.last_native_playing && !m.ended
        } else if mode == "early-seek" {
            m.first_ms.is_some() && native_requests.get() == 1
        } else if mode == "close" {
            m.first_ms.is_none()
                && native_requests.get() == 1
                && close_time.get() < 500
                && state.streaming.borrow().is_none()
        } else {
            m.first_ms.is_some()
                && m.partial_first
                && m.partial_position_ms >= 100
                && native_requests.get() == 1
        };
    let summary = format!(
        "mode={mode} first_decoded_frame_ms={:?} before_complete={} ended={} failed={} waits={} range_requests={:?} close_ms={} first_progress_ms={:?} partial_position_ms={} max_position_ms={} valid={valid}",
        m.first_ms,
        m.partial_first,
        m.ended,
        m.failed,
        m.waits,
        ranges.borrow(),
        close_time.get(),
        m.first_progress_ms,
        m.partial_position_ms,
        m.max_position_ms
    );
    println!("{summary}");
    std::fs::write(directory.join(format!("{mode}.txt")), summary)?;
    let pixels = captured.borrow_mut().take().ok_or("No viewer snapshot")??;
    image::save_buffer(
        directory.join(format!("{mode}.png")),
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )?;
    demo_backend::cleanup_media(&media)?;
    if !valid {
        return Err("Streaming assertion failed".into());
    }
    Ok(())
}
