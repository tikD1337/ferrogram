//! Lifecycle wiring. Slint stays in tg-ui; TDLib and JSON stay in tg-telegram.
// Release builds are GUI applications: no console window opens next to the client.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod config;
mod demo;
mod options;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
};
use tg_core::{CommandError, CommandKind, Event, SecurityWarning};
use tg_telegram::{Engine, EngineParameters};
use tg_ui::{EventSink, Ui};
fn main() -> std::process::ExitCode {
    // Started from a terminal: print there. Started from Explorer: errors go to a message box.
    let console = tg_platform::attach_parent_console();
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // Errors produced here are fixed application text, never credential/native payloads.
        Err(error) => {
            if console {
                eprintln!("{error}");
            } else {
                tg_platform::show_error(&error.to_string());
            }
            std::process::ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = options::Options::parse(std::env::args().skip(1))?;
    if options.help {
        println!(
            "ferrogram [--demo | --demo-text-only] [--theme dark|oled] [--snapshot <png>] [--demo-scroll <seconds>]\n--demo runs synthetic conversations without TDLib, credentials or key files."
        );
        return Ok(());
    }
    if options.demo {
        let media_path = std::thread::spawn(demo::prepare_media)
            .join()
            .map_err(|_| "Demo media worker failed")??;
        let backend = Rc::new(RefCell::new(demo::Demo::new(
            media_path.clone(),
            options.demo_text_only,
        )));
        let commands = backend.clone();
        // Demo never resolves TG_DATA_DIR. An explicitly configured emoji font is optional.
        let font = std::thread::spawn(|| config::emoji_font(None))
            .join()
            .map_err(|_| "Font loader failed")??;
        let ui = Ui::new(
            tg_ui::Options {
                oled: options.oled,
                demo: true,
                snapshot: options.snapshot,
                emoji_font: font,
                demo_scroll: options.demo_scroll,
                demo_video: options.demo_video,
            },
            move |command| commands.borrow_mut().command(command),
        )?;
        backend.borrow_mut().sink = Some(ui.event_sink());
        backend.borrow_mut().seed();
        let notifications = attach_notifications(&ui);
        let player = attach_video(&ui);
        backend.borrow().schedule_incoming();
        let result = ui.run();
        drop(player);
        drop(notifications);
        let _ = std::thread::spawn(move || {
            let _ = std::fs::remove_file(media_path.with_extension("mp4"));
            std::fs::remove_file(media_path)
        })
        .join();
        return result;
    }
    // Pre-window configuration reads are also off the UI thread. No keys or native engine yet.
    let (credentials, directory, font) = std::thread::spawn(|| {
        let credentials = config::credentials()?;
        let directory = config::plain_directory(
            tg_security::data_dir().map_err(|_| "Invalid application data directory")?,
        );
        let font = config::emoji_font(Some(&directory))?;
        Ok::<_, &'static str>((credentials, directory, font))
    })
    .join()
    .map_err(|_| "Configuration worker failed")??;
    let engine: Arc<Mutex<Option<Engine>>> = Arc::default();
    let dispatch = engine.clone();
    let ui = Ui::new(
        tg_ui::Options {
            oled: options.oled,
            demo: false,
            snapshot: options.snapshot,
            emoji_font: font,
            demo_scroll: None,
            demo_video: None,
        },
        move |command| {
            let slot = dispatch
                .try_lock()
                .map_err(|_| failure("The engine is busy. Please try again."))?;
            slot.as_ref()
                .ok_or_else(|| failure("The engine is still starting."))?
                .send(command)
        },
    )?;
    let notifications = attach_notifications(&ui);
    let player = attach_video(&ui);
    let sink = ui.event_sink();
    // Authorization can arrive before Engine::start returns. Hold delivery until
    // the handle is installed, otherwise the UI's default QR request can race it.
    sink.pause();
    // Exactly one shutdown token. Startup and shutdown/join never run on the UI thread.
    let (close, closed) = mpsc::sync_channel::<()>(1);
    let worker = std::thread::Builder::new()
        .name("tg-app-lifecycle".into())
        .spawn(move || {
            let started = (|| {
                let protection = tg_security::load_or_create(&directory).map_err(|_| {
                    failure("Could not unlock the database key. Existing key files were preserved.")
                })?;
                if !protection.tpm_protected {
                    sink.post(Event::SecurityWarning(SecurityWarning::TpmUnavailable));
                }
                let events = sink.clone();
                let parameters = EngineParameters {
                    api_id: credentials.id,
                    api_hash: credentials.hash,
                    database_encryption_key: *protection.key.expose(),
                    data_directory: directory,
                };
                drop(protection);
                Engine::start(parameters, move |event| events.post(event))
            })();
            match started {
                Ok(started) => {
                    if let Ok(mut slot) = engine.lock() {
                        *slot = Some(started);
                    }
                    sink.resume();
                    let _ = closed.recv();
                    let owned = engine.lock().ok().and_then(|mut slot| slot.take());
                    if let Some(owned) = owned {
                        owned.shutdown().map_err(|_| "Engine shutdown failed")?;
                    }
                }
                Err(error) => {
                    startup_error(&sink, error);
                    sink.resume();
                }
            }
            Ok::<_, &'static str>(())
        })
        .map_err(|_| "Could not start the lifecycle worker")?;
    let result = ui.run();
    drop(player);
    drop(notifications);
    let _ = close.try_send(());
    let shutdown = worker.join().map_err(|_| "Lifecycle worker failed")?;
    result?;
    shutdown?;
    Ok(())
}
/// Desktop notifications are optional: without a tray (restricted sessions, sandboxes)
/// the client keeps running and shows a warning instead of exiting.
fn attach_notifications(ui: &Ui) -> Option<Arc<tg_platform::Notifications>> {
    let events = ui.event_sink();
    let started = std::thread::spawn(move || {
        tg_platform::Notifications::start(move |chat| {
            events.post(Event::NotificationActivated(chat))
        })
    })
    .join();
    let Ok(Ok(service)) = started else {
        startup_error(
            &ui.event_sink(),
            failure("Desktop notifications are unavailable in this session."),
        );
        return None;
    };
    let service = Arc::new(service);
    let send = service.clone();
    ui.on_notification(move |notification| {
        send.show(notification)
            .map_err(|_| failure("Could not show a desktop notification."))
    });
    Some(service)
}

/// The video player is optional too: without Media Foundation the client keeps running and
/// the player reports that playback is unavailable.
fn attach_video(ui: &Ui) -> Option<Rc<tg_platform::Player>> {
    let events = ui.event_sink();
    let player = Rc::new(
        tg_platform::Player::start(move |state| events.post(Event::VideoState(state))).ok()?,
    );
    let requests = player.clone();
    ui.on_video(move |request| {
        requests
            .request(request)
            .map_err(|_| failure("Could not play the video."))
    });
    Some(player)
}

fn failure(message: &str) -> CommandError {
    CommandError {
        code: -4,
        message: message.to_owned(),
    }
}
fn startup_error(sink: &EventSink, error: CommandError) {
    sink.post(Event::CommandFailed {
        command: CommandKind::Initialize,
        error,
    });
}
