//! Interface scale dialog, as Telegram Desktop's Settings > Interface scale: a default switch,
//! a slider in 5 % steps, and a restart to apply. tg-app stores the value and restarts.
use super::*;
pub(super) type ScaleFn = Box<dyn Fn(u16) -> Result<(), CommandError>>;
impl State {
    fn open_scale(&self, w: &AppWindow) {
        let applied = self.scale_applied.get();
        // Under a fixed scale the window reports that scale, not the system's: show the base.
        let system = if applied == tg_core::SCALE_AUTO {
            (w.window().scale_factor() * 100.0).round() as i32
        } else {
            100
        };
        w.set_scale_system(system);
        w.set_scale_applied(i32::from(applied));
        w.set_scale_auto(applied == tg_core::SCALE_AUTO);
        w.set_scale_percent(if applied == tg_core::SCALE_AUTO {
            i32::from(tg_core::check_scale(
                u16::try_from(system).unwrap_or(tg_core::SCALE_MIN),
            ))
        } else {
            i32::from(applied)
        });
        w.set_action_dialog(4);
    }
    fn apply_scale(&self, w: &AppWindow, value: i32) {
        let value = tg_core::check_scale(u16::try_from(value.max(0)).unwrap_or(tg_core::SCALE_MAX));
        if value == self.scale_applied.get() {
            w.set_action_dialog(0);
            return;
        }
        let saved = self
            .scale
            .borrow()
            .as_ref()
            .map_or(Ok(()), |store| store(value));
        match saved {
            // tg-app restarts the client once the engine has shut down.
            Ok(()) => {
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                w.set_action_dialog(0);
                w.set_error(error.message.into());
            }
        }
    }
}
pub(super) fn bind(w: &AppWindow, state: &Rc<State>) {
    let weak = w.as_weak();
    let s = state.clone();
    w.on_open_scale(move || {
        if let Some(w) = weak.upgrade() {
            s.open_scale(&w);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_apply_scale(move |value| {
        if let Some(w) = weak.upgrade() {
            s.apply_scale(&w, value);
        }
    });
}
