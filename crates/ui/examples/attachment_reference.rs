// Account-free attachment review: real UI/demo, with shell execution replaced at its boundary.
extern crate self as tg_ui;
include!("../src/lib.rs");
#[path = "../../app/src/demo.rs"]
#[allow(dead_code)]
mod demo_backend;
use slint::Model;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("Supply a review mode")?;
    let path = args.next().ok_or("Supply a PNG path")?;
    let media = demo_backend::prepare_media()?;
    let backend = Rc::new(RefCell::new(demo_backend::Demo::new(media.clone(), false)));
    let commands = backend.clone();
    let opened: Rc<Cell<u32>> = Rc::default();
    let mark = opened.clone();
    let ui = Ui::new(
        Options {
            demo: true,
            demo_stickers: mode == "stickers",
            ..Default::default()
        },
        move |command| {
            if matches!(command, Command::OpenMessageContent { .. }) {
                mark.set(mark.get() + 1);
            }
            commands.borrow_mut().command(command)
        },
    )?;
    backend.borrow_mut().sink = Some(ui.event_sink());
    backend.borrow_mut().seed();
    let shell: Rc<RefCell<Vec<tg_core::FileRequest>>> = Rc::default();
    let requests = shell.clone();
    ui.on_file(move |request| {
        if requests.borrow().len() < 8 {
            requests.borrow_mut().push(request);
        }
        Ok(())
    });
    let events = ui.event_sink();
    let state = ui.state.clone();
    let observed = ui.window.clone_strong();
    let weak = ui.window.as_weak();
    let action = mode.clone();
    let sink = events.clone();
    slint::Timer::single_shot(Duration::from_millis(900), move || {
        if let Some(w) = weak.upgrade() {
            let id = match action.as_str() {
                "file-confirm" | "file-cancel-confirm" => 19997,
                "file" | "file-progress" | "folder" | "file-cancel" | "file-menu" => 19988,
                _ => 19984,
            } * 1048576_i64;
            if action != "stickers" {
                w.set_follow_tail(false);
                state.feed.jump_target.set(Some(MessageId(id)));
                state.refresh(&w);
                state.attachment_click(&w, &id.to_string(), 0, 0.0);
            }
            if action.starts_with("file-") || action == "folder" {
                let weak = w.as_weak();
                let state = state.clone();
                slint::Timer::single_shot(Duration::from_millis(400), move || {
                    if let Some(w) = weak.upgrade() {
                        match action.as_str() {
                            "file-confirm" | "file-cancel-confirm" => {
                                state.attachment_click(&w, &id.to_string(), 0, 0.0);
                                if action == "file-cancel-confirm" {
                                    w.invoke_cancel_action();
                                }
                            }
                            "file-menu" => {
                                if let Some(row) = (0..state.feed.row_count())
                                    .filter_map(|i| state.feed.row_data(i))
                                    .find(|r| r.key == id.to_string())
                                {
                                    let x = if row.outgoing {
                                        1040.0 - row.bubble_width + 20.0
                                    } else {
                                        340.0
                                    };
                                    let y = 94.0
                                        + row.row_y
                                        + w.get_feed_y()
                                        + (w.get_feed_height() - w.get_feed_content_height())
                                            .max(0.0)
                                        + row.bubble_y
                                        + row.attachment_y
                                        + 16.0;
                                    use slint::platform::{PointerEventButton, WindowEvent};
                                    for event in [
                                        WindowEvent::PointerPressed {
                                            position: slint::LogicalPosition::new(x, y),
                                            button: PointerEventButton::Right,
                                        },
                                        WindowEvent::PointerReleased {
                                            position: slint::LogicalPosition::new(x, y),
                                            button: PointerEventButton::Right,
                                        },
                                    ] {
                                        w.window().dispatch_event(event);
                                    }
                                }
                            }
                            "folder" => state.attachment_click(&w, &id.to_string(), 2, 0.0),
                            "file-progress" => sink.post(Event::FileUpdated(tg_core::MediaFile {
                                partial: None,
                                id: 3_000_000 + 19988,
                                path: None,
                                size: 1000,
                                downloaded: 540,
                                downloading: true,
                            })),
                            "file-cancel" => {
                                {
                                    let mut h = state.feed.history.borrow_mut();
                                    if let Some(m) =
                                        h.messages.iter_mut().find(|m| m.id == MessageId(id))
                                        && let Some(tg_core::Attachment::Document(d)) =
                                            &mut m.attachment
                                    {
                                        d.file.path = None;
                                        d.file.downloading = true;
                                    }
                                }

                                state.attachment_click(&w, &id.to_string(), 0, 0.0);
                            }
                            _ => {}
                        }
                    }
                });
            }
        }
    });
    let captured = Rc::new(RefCell::new(None));
    let pixels = captured.clone();
    let weak = ui.window.as_weak();
    slint::Timer::single_shot(Duration::from_secs(2), move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.window().take_snapshot();
            *pixels.borrow_mut() = Some(w.window().take_snapshot());
        }
        let _ = slint::quit_event_loop();
    });
    let state = ui.state.clone();
    ui.run()?;
    let valid=match mode.as_str() {
        "file-confirm"=>observed.get_action_dialog()==3 && shell.borrow().is_empty(),
        "file-cancel-confirm"=>observed.get_action_dialog()==0 && shell.borrow().is_empty(),
        "folder"=>matches!(shell.borrow().first(),Some(tg_core::FileRequest::ShowInFolder(_))),
        "stickers"=>state.feed.row_data(0).is_some_and(|r|r.sticker),
        "file-progress"=>state.feed.history.borrow().messages.iter().any(|m|matches!(&m.attachment,Some(tg_core::Attachment::Document(d)) if d.file.downloaded==540 && d.file.downloading)),
        "file-menu"=>observed.get_menu_open() && observed.get_menu_can_reveal(),
        "file"=>shell.borrow().is_empty() && state.feed.history.borrow().messages.iter().any(|m|matches!(&m.attachment,Some(tg_core::Attachment::Document(d)) if d.file.path.is_some())),
        "file-cancel"=>shell.borrow().is_empty() && state.feed.history.borrow().messages.iter().any(|m|m.id==MessageId(19988*1048576) && matches!(&m.attachment,Some(tg_core::Attachment::Document(d)) if d.file.path.is_none() && !d.file.downloading)),
        _=>false,
    };
    let pixels = captured.borrow_mut().take().ok_or("No snapshot")??;
    image::save_buffer(
        path,
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )?;
    demo_backend::cleanup_media(&media)?;
    if !valid {
        eprintln!(
            "menu_open={} can_reveal={} feed_y={}",
            observed.get_menu_open(),
            observed.get_menu_can_reveal(),
            observed.get_feed_y()
        );
        return Err("Attachment review assertion failed".into());
    }
    Ok(())
}
