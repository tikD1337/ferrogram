//! Captures the actual Slint declaration with a popup menu and multiline composer.
//! Uses runtime pointer events on its own synthetic window, with no account or native API.
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};
mod generated {
    #![allow(missing_debug_implementations, clippy::unwrap_used, clippy::todo)]
    slint::include_modules!();
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Supply a PNG output path")?;
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()?;
    }
    let ui = generated::AppWindow::new()?;
    ui.set_ready(true);
    ui.set_demo(true);
    ui.set_chat_open(true);
    ui.set_chat_title("Synthetic dependency review".into());
    ui.set_draft("A multiline draft\nShift+Enter keeps the second line".into());
    ui.set_chats(ModelRc::new(VecModel::from(vec![generated::ChatRow {
        title: "Synthetic dependency review".into(),
        initials: "DR".into(),
        preview: "Popup and composer review".into(),
        selected: true,
        ..Default::default()
    }])));
    let error = Rc::new(RefCell::new(None));
    let weak = ui.as_weak();
    let event_error = error.clone();
    slint::Timer::single_shot(Duration::from_millis(500), move || {
        if let Some(ui) = weak.upgrade() {
            use slint::platform::{PointerEventButton, WindowEvent};
            for event in [
                WindowEvent::PointerPressed {
                    position: slint::LogicalPosition::new(28.0, 67.0),
                    button: PointerEventButton::Left,
                },
                WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(28.0, 67.0),
                    button: PointerEventButton::Left,
                },
            ] {
                if ui.window().dispatch_event_with_result(event).is_err() {
                    *event_error.borrow_mut() = Some("Pointer event dispatch failed");
                }
            }
        }
    });
    let pixels = Rc::new(RefCell::new(None));
    let captured = pixels.clone();
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(1), move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.window().take_snapshot();
            *captured.borrow_mut() = Some(ui.window().take_snapshot());
        }
        let _ = slint::quit_event_loop();
    });
    ui.run()?;
    if let Some(error) = error.borrow_mut().take() {
        return Err(error.into());
    }
    let pixels = pixels.borrow_mut().take().ok_or("No window snapshot")??;
    // The event loop is stopped; PNG I/O cannot stall interactive rendering.
    image::save_buffer(
        path,
        pixels.as_bytes(),
        pixels.width(),
        pixels.height(),
        image::ColorType::Rgba8,
    )?;
    Ok(())
}
