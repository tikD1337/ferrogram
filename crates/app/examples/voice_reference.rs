//! Account-free end-to-end voice playback/listen receipt check using the production UI/demo/player.
#[path = "../src/demo.rs"]
#[allow(dead_code)]
mod demo;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};
use tg_core::{Command, Event};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let media = demo::prepare_media()?;
    let demo = Rc::new(RefCell::new(demo::Demo::new(media.clone(), false)));
    let backend = demo.clone();
    let marks = Rc::new(Cell::new(0u32));
    let receipts = marks.clone();
    let ui = tg_ui::Ui::new(
        tg_ui::Options {
            demo: true,
            demo_voice: Some(16),
            ..Default::default()
        },
        move |command| {
            if matches!(command, Command::OpenMessageContent { .. }) {
                receipts.set(receipts.get() + 1);
            }
            backend.borrow_mut().command(command)
        },
    )?;
    demo.borrow_mut().sink = Some(ui.event_sink());
    demo.borrow_mut().seed();
    let ended = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let position = Arc::new(AtomicU32::new(0));
    let (e, f, p) = (ended.clone(), failed.clone(), position.clone());
    let sink = ui.event_sink();
    let player = Rc::new(tg_platform::Player::start(move |state| {
        if state.ended {
            e.store(true, Ordering::Release);
        }
        if state.failed {
            f.store(true, Ordering::Release);
        }
        p.fetch_max(state.position_ms, Ordering::AcqRel);
        sink.post(Event::VideoState(state));
    })?);
    let requests = player.clone();
    ui.on_video(move |request| {
        requests
            .request(request)
            .map_err(|_| tg_core::CommandError {
                code: -4,
                message: "Voice review playback failed.".into(),
            })
    });
    ui.run()?;
    drop(player);
    println!(
        "voice_ended={} voice_failed={} maximum_position_ms={} content_open_receipts={}",
        ended.load(Ordering::Acquire),
        failed.load(Ordering::Acquire),
        position.load(Ordering::Acquire),
        marks.get()
    );
    demo::cleanup_media(&media)?;
    if !ended.load(Ordering::Acquire)
        || failed.load(Ordering::Acquire)
        || position.load(Ordering::Acquire) < 11_500
        || marks.get() != 1
    {
        return Err("Voice playback/listen receipt assertion failed".into());
    }
    Ok(())
}
