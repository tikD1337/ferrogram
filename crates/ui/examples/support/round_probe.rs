//! Native clipping/visibility evidence; hardware video pixels are not read back.
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};

pub fn visible_region(parent: isize) -> Result<bool, Box<dyn std::error::Error>> {
    // SAFETY: the reference owns parent on this UI thread; all GDI handles are released below.
    unsafe {
        let child = FindWindowExW(
            parent as HWND,
            std::ptr::null_mut(),
            windows_sys::w!("FerrogramVideo"),
            std::ptr::null(),
        );
        if child.is_null() {
            return Err("No native round window".into());
        }
        let mut bounds = RECT::default();
        GetClientRect(child, &mut bounds);
        let region = CreateRectRgn(0, 0, 0, 0);
        let kind = GetWindowRgn(child, region);
        let mut box_ = RECT::default();
        GetRgnBox(region, &mut box_);
        DeleteObject(region);
        let mut location = RECT::default();
        GetWindowRect(child, &mut location);
        println!(
            "native visible={} rect={:?} region_kind={kind} region_box={:?}",
            IsWindowVisible(child),
            (location.left, location.top, location.right, location.bottom),
            (box_.left, box_.top, box_.right, box_.bottom)
        );
        Ok(IsWindowVisible(child) != 0
            && kind != NULLREGION
            && box_.right > box_.left
            && box_.bottom > box_.top)
    }
}
