//! Applies the interface scale to the GUI toolkit before its first window.

/// Sets the logical-to-physical pixel factor of every window of this process. Slint's winit
/// backend reads `SLINT_SCALE_FACTOR` once, when the first window opens, and then ignores the
/// system's DPI changes, as Telegram Desktop's fixed scale does.
///
/// Call it from `main` before the first window and before any other thread is started.
pub fn set_interface_scale(factor: f32) {
    let value = format!("{factor:.3}");
    // SAFETY: `set_var` races only with concurrent environment access. The caller runs this on
    // the main thread before starting any thread or loading any library that reads it.
    unsafe { std::env::set_var("SLINT_SCALE_FACTOR", value) };
}
