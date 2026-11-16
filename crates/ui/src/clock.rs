//! Local-time labels for message times, date separators and the chat list.
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone};

fn local(date: i32) -> Option<DateTime<Local>> {
    Local.timestamp_opt(i64::from(date), 0).single()
}

/// Local calendar day, used to place date separators.
pub(crate) fn day(date: i32) -> Option<NaiveDate> {
    local(date).map(|t| t.date_naive())
}

/// "14:05".
pub(crate) fn time(date: i32) -> String {
    local(date).map_or_else(String::new, |t| t.format("%H:%M").to_string())
}

/// "November 16", with the year when it is not the current one.
pub(crate) fn day_label(date: i32) -> String {
    let (Some(t), now) = (local(date), Local::now()) else {
        return String::new();
    };
    if t.year() == now.year() {
        t.format("%B %-d").to_string()
    } else {
        t.format("%B %-d, %Y").to_string()
    }
}

/// Chat list: time today, weekday within a week, otherwise a short date.
pub(crate) fn list_label(date: i32) -> String {
    if date <= 0 {
        return String::new();
    }
    let (Some(t), now) = (local(date), Local::now()) else {
        return String::new();
    };
    let days = (now.date_naive() - t.date_naive()).num_days();
    if days == 0 {
        t.format("%H:%M").to_string()
    } else if (1..7).contains(&days) {
        t.format("%a").to_string()
    } else {
        t.format("%d.%m.%y").to_string()
    }
}

/// "0:42", "12:05", "1:02:03" for a duration in seconds.
pub(crate) fn duration(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_use_minutes_and_hours_only_when_needed() {
        assert_eq!(super::duration(0), "0:00");
        assert_eq!(super::duration(75), "1:15");
        assert_eq!(super::duration(3723), "1:02:03");
    }
}
