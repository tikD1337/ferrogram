//! Interface scale, as Telegram Desktop's `style::Scale`: a percentage of the base size from 50 to
//! 300 in steps of 5, or "auto" to follow the system's DPI. Changing it takes a restart.

/// Follow the system's DPI (Telegram Desktop's `kScaleAuto`).
pub const SCALE_AUTO: u16 = 0;
pub const SCALE_MIN: u16 = 50;
pub const SCALE_MAX: u16 = 300;
pub const SCALE_STEP: u16 = 5;

/// A valid stored value: `SCALE_AUTO`, or a multiple of `SCALE_STEP` within the limits.
pub fn check_scale(value: u16) -> u16 {
    if value == SCALE_AUTO {
        return SCALE_AUTO;
    }
    let clamped = value.clamp(SCALE_MIN, SCALE_MAX);
    (clamped + SCALE_STEP / 2) / SCALE_STEP * SCALE_STEP
}

/// Reads the stored setting; anything unreadable means auto.
pub fn parse_scale(text: &str) -> u16 {
    let text = text.trim();
    if text.len() > 8 {
        return SCALE_AUTO;
    }
    text.parse::<u16>().map_or(SCALE_AUTO, check_scale)
}

/// Logical-to-physical pixel factor for a stored setting; `None` leaves it to the system.
pub fn scale_factor(value: u16) -> Option<f32> {
    match check_scale(value) {
        SCALE_AUTO => None,
        percent => Some(f32::from(percent) / 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_are_bounded_stepped_or_auto() {
        assert_eq!(check_scale(0), SCALE_AUTO);
        assert_eq!(check_scale(120), 120);
        assert_eq!(check_scale(122), 120);
        assert_eq!(check_scale(123), 125);
        assert_eq!(check_scale(10), SCALE_MIN);
        assert_eq!(check_scale(999), SCALE_MAX);
        assert_eq!(parse_scale(" 120\r\n"), 120);
        for bad in ["", "abc", "-5", "120%", "123456789"] {
            assert_eq!(parse_scale(bad), SCALE_AUTO, "{bad:?}");
        }
        assert_eq!(scale_factor(120), Some(1.2));
        assert_eq!(scale_factor(SCALE_AUTO), None);
    }
}
