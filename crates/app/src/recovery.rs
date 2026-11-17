//! "Start fresh" after the database key could not be unlocked: the old data directory is moved
//! aside under a dated name, never deleted, so a later fix (or support) can still read it.
use std::path::{Path, PathBuf};

/// Renames `directory` to a sibling `<name>-backup-<YYYY-MM-DD-HHMMSS>` (UTC), adding `-2`, `-3`
/// and so on if that name is taken. Returns the new path. Fails without touching anything when
/// the directory cannot be renamed (for example while another process holds a file in it).
pub(crate) fn move_aside(directory: &Path) -> std::io::Result<PathBuf> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("The data directory has no usable name"))?;
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let base = format!("{name}-backup-{}", stamp(seconds));
    for attempt in 1..=100 {
        let candidate = if attempt == 1 {
            directory.with_file_name(&base)
        } else {
            directory.with_file_name(format!("{base}-{attempt}"))
        };
        if !candidate.exists() {
            std::fs::rename(directory, &candidate)?;
            return Ok(candidate);
        }
    }
    Err(std::io::Error::other(
        "Too many backup folders with the same time",
    ))
}

/// `YYYY-MM-DD-HHMMSS` in UTC from Unix seconds (civil-from-days, proleptic Gregorian).
fn stamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_calendar_dates() {
        assert_eq!(stamp(0), "1970-01-01-000000");
        // 2024-02-29 12:34:56 UTC, a leap day.
        assert_eq!(stamp(1_709_210_096), "2024-02-29-123456");
        // 2000-03-01 00:00:00 UTC, the day after a century leap day.
        assert_eq!(stamp(951_868_800), "2000-03-01-000000");
    }

    #[test]
    fn the_old_data_is_moved_next_to_itself_and_kept() {
        let root = std::env::temp_dir().join(format!("ferrogram-recovery-{}", std::process::id()));
        let data = root.join("tg-rust");
        std::fs::create_dir_all(&data).expect("create data");
        std::fs::write(data.join("database-key.bin"), b"old").expect("write key");
        let first = move_aside(&data).expect("move");
        assert!(!data.exists());
        assert_eq!(
            std::fs::read(first.join("database-key.bin")).expect("kept"),
            b"old"
        );
        // A second reset in the same second gets its own folder instead of failing.
        std::fs::create_dir_all(&data).expect("recreate");
        let second = move_aside(&data).expect("move again");
        assert_ne!(first, second);
        assert!(second.exists() && first.exists());
        std::fs::remove_dir_all(&root).expect("clean up");
    }
}
