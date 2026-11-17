//! Streaming metadata and bounded download-range coalescing; no media bytes are cached here.
use std::path::PathBuf;
pub const STREAM_PART: u64 = 128 * 1024;
pub const STREAM_PRELOAD: u64 = 1024 * 1024;
#[derive(Clone, PartialEq, Eq)]
pub struct StreamFile {
    pub id: i32,
    pub size: u64,
    pub path: Option<PathBuf>,
    pub offset: u64,
    pub prefix: u64,
    pub completed: bool,
}
impl std::fmt::Debug for StreamFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamFile")
            .field("id", &self.id)
            .field("offset", &self.offset)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamStatus {
    pub file: i32,
    pub token: u64,
    pub needed: Option<u64>,
    pub waiting: bool,
}
/// Playback time loaded ahead of the position, as Telegram Desktop's `kLoadInAdvanceForRemote`.
pub const STREAM_AHEAD_SECONDS: u64 = 32;
/// One TDLib download request: `limit` bytes from `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamWindow {
    pub offset: u64,
    pub limit: u64,
}
/// Plans TDLib downloads as a window that slides ahead of playback. TDLib sends every part of
/// a request to the server at once, so at most about `STREAM_PRELOAD` is ever in flight: a seek
/// or a read of the index at the end of the file then waits behind that much, not behind the
/// whole file (Telegram Desktop keeps four 128 KiB requests in flight for the same reason).
#[derive(Debug, Default)]
pub struct StreamRanges {
    /// Start of the current window.
    requested: Option<u64>,
    /// End of the current window, exclusive.
    end: u64,
    /// Last download offset and prefix reported by TDLib.
    offset: u64,
    prefix: u64,
}
fn align_up(offset: u64) -> u64 {
    offset.div_ceil(STREAM_PART).saturating_mul(STREAM_PART)
}
impl StreamRanges {
    pub fn update(&mut self, file: &StreamFile) {
        if self.requested.is_none() || self.requested == Some(file.offset) {
            self.offset = file.offset;
            self.prefix = file.prefix;
        }
    }
    /// The player waits for `offset`. Returns the request to send, or `None` when the current
    /// window already covers it.
    pub fn need(&mut self, offset: u64, size: u64) -> Option<StreamWindow> {
        if offset >= size {
            return None;
        }
        if let Some(start) = self.requested
            && offset >= start
        {
            if offset < self.end {
                return None;
            }
            // Just past the window: grow it rather than restart the download.
            if offset < self.end.saturating_add(STREAM_PRELOAD) {
                return self.grow(align_up(offset + 1).saturating_add(STREAM_PRELOAD), size);
            }
        }
        let aligned = offset / STREAM_PART * STREAM_PART;
        self.requested = Some(aligned);
        self.end = aligned.saturating_add(STREAM_PRELOAD).min(size);
        Some(StreamWindow {
            offset: aligned,
            limit: self.end - aligned,
        })
    }
    /// Playback should have data up to `target`. Extends the window once at most half of it is
    /// still in flight, by no more than one window past what TDLib has downloaded.
    pub fn advance(&mut self, target: u64, size: u64) -> Option<StreamWindow> {
        let start = self.requested?;
        if self.offset != start {
            return None;
        }
        let downloaded = start.saturating_add(self.prefix);
        let target = target.min(size);
        if self.end >= target || downloaded.saturating_add(STREAM_PRELOAD / 2) < self.end {
            return None;
        }
        self.grow(
            align_up(downloaded.saturating_add(STREAM_PRELOAD)).min(target),
            size,
        )
    }
    fn grow(&mut self, end: u64, size: u64) -> Option<StreamWindow> {
        let start = self.requested?;
        let end = end.min(size);
        if end <= self.end {
            return None;
        }
        self.end = end;
        Some(StreamWindow {
            offset: start,
            limit: end - start,
        })
    }
}
/// Most separate loaded ranges remembered for the seek bar.
pub const MAX_LOADED_RANGES: usize = 64;
/// Byte ranges TDLib reported as downloaded across all windows of one file, merged and bounded.
/// TDLib reports only the current window's prefix, so without this the seek bar forgets bytes
/// an earlier window loaded (the start of a file whose index is read from its end first).
#[derive(Debug, Default)]
pub struct LoadedRanges {
    ranges: Vec<(u64, u64)>,
}
impl LoadedRanges {
    pub fn add(&mut self, file: &StreamFile) {
        if file.completed {
            self.ranges = vec![(0, file.size)];
            return;
        }
        let start = file.offset.min(file.size);
        let end = file.offset.saturating_add(file.prefix).min(file.size);
        if end <= start {
            return;
        }
        self.ranges.push((start, end));
        self.ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.ranges.len());
        for &(a, b) in &self.ranges {
            if let Some(last) = merged.last_mut()
                && last.1 >= a
            {
                last.1 = last.1.max(b);
            } else {
                merged.push((a, b));
            }
        }
        if merged.len() > MAX_LOADED_RANGES
            && let Some(drop) = merged.iter().position(|&(a, b)| !(a <= start && start < b))
        {
            merged.remove(drop);
        }
        self.ranges = merged;
    }
    /// The loaded range that contains `offset`.
    pub fn around(&self, offset: u64) -> Option<(u64, u64)> {
        self.ranges
            .iter()
            .copied()
            .find(|&(a, b)| a <= offset && offset < b)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn at(offset: u64, prefix: u64, size: u64) -> StreamFile {
        StreamFile {
            id: 1,
            size,
            path: None,
            offset,
            prefix,
            completed: false,
        }
    }
    fn window(offset: u64, end: u64) -> Option<StreamWindow> {
        Some(StreamWindow {
            offset,
            limit: end - offset,
        })
    }
    #[test]
    fn seek_coalesces_one_request_and_aligns_parts() {
        let mut r = StreamRanges::default();
        let size = 32 * STREAM_PRELOAD;
        assert_eq!(r.need(0, size).map(|w| w.offset), Some(0));
        assert_eq!(r.need(10, size), None);
        assert_eq!(
            r.need(3 * STREAM_PRELOAD + 17, size).map(|w| w.offset),
            Some(3 * STREAM_PRELOAD)
        );
        assert_eq!(r.need(3 * STREAM_PRELOAD + 18, size), None);
        assert_eq!(r.need(size, size), None);
        r.update(&at(0, 1, size));
        assert_eq!(r.need(3 * STREAM_PRELOAD + 19, size), None);
        assert_eq!(r.need(0, size).map(|w| w.offset), Some(0));
    }
    #[test]
    fn a_request_asks_for_one_window_not_the_whole_file() {
        // Asking TDLib for the whole file queues every part on the server at once, so a later
        // request for the index at the end of the file waits behind the entire file.
        let mut r = StreamRanges::default();
        let size = 11 * STREAM_PRELOAD;
        assert_eq!(r.need(0, size), window(0, STREAM_PRELOAD));
        let tail = size - 2 * STREAM_PART;
        assert_eq!(r.need(tail + 5, size), window(tail, size));
    }
    #[test]
    fn the_window_slides_while_little_is_in_flight_and_stops_at_the_target() {
        let mut r = StreamRanges::default();
        let size = 64 * STREAM_PRELOAD;
        let target = 3 * STREAM_PRELOAD;
        assert_eq!(r.need(0, size), window(0, STREAM_PRELOAD));
        // More than half of the window still in flight: wait.
        r.update(&at(0, STREAM_PRELOAD / 4, size));
        assert_eq!(r.advance(target, size), None);
        // Half of it arrived: extend by at most one window past what is downloaded.
        r.update(&at(0, STREAM_PRELOAD / 2, size));
        assert_eq!(r.advance(target, size), window(0, 3 * STREAM_PRELOAD / 2));
        r.update(&at(0, 3 * STREAM_PRELOAD / 2, size));
        assert_eq!(r.advance(target, size), window(0, 5 * STREAM_PRELOAD / 2));
        r.update(&at(0, 5 * STREAM_PRELOAD / 2, size));
        assert_eq!(r.advance(target, size), window(0, target));
        r.update(&at(0, target, size));
        assert_eq!(r.advance(target, size), None, "enough is loaded ahead");
        // Playback moved on: continue from where the download stopped.
        assert_eq!(
            r.advance(target + STREAM_PRELOAD, size),
            window(0, target + STREAM_PRELOAD)
        );
    }
    #[test]
    fn a_read_just_past_the_window_extends_it_instead_of_restarting() {
        let mut r = StreamRanges::default();
        let size = 64 * STREAM_PRELOAD;
        assert_eq!(r.need(0, size), window(0, STREAM_PRELOAD));
        r.update(&at(0, STREAM_PRELOAD, size));
        assert_eq!(
            r.need(STREAM_PRELOAD + 3, size),
            window(0, 2 * STREAM_PRELOAD + STREAM_PART)
        );
    }
    #[test]
    fn the_loaded_bar_keeps_bytes_downloaded_by_earlier_windows() {
        // The index at the end is fetched first, then playback continues at 256 KiB: the first
        // 256 KiB downloaded before must still show as loaded.
        let mut loaded = LoadedRanges::default();
        let size = 11 * STREAM_PRELOAD;
        loaded.add(&at(0, 2 * STREAM_PART, size));
        loaded.add(&at(size - STREAM_PART, STREAM_PART, size));
        loaded.add(&at(2 * STREAM_PART, 3 * STREAM_PART, size));
        assert_eq!(loaded.around(2 * STREAM_PART), Some((0, 5 * STREAM_PART)));
        assert_eq!(loaded.around(size - 1), Some((size - STREAM_PART, size)));
        assert_eq!(loaded.around(6 * STREAM_PART), None);
        let mut done = at(0, 0, size);
        done.completed = true;
        loaded.add(&done);
        assert_eq!(loaded.around(6 * STREAM_PART), Some((0, size)));
    }
    #[test]
    fn loaded_ranges_stay_bounded() {
        let mut loaded = LoadedRanges::default();
        let size = 1024 * STREAM_PRELOAD;
        for n in 0..200 {
            loaded.add(&at(n * 4 * STREAM_PART, STREAM_PART, size));
        }
        assert!(loaded.ranges.len() <= MAX_LOADED_RANGES);
        assert_eq!(
            loaded.around(199 * 4 * STREAM_PART),
            Some((199 * 4 * STREAM_PART, 199 * 4 * STREAM_PART + STREAM_PART))
        );
    }
    #[test]
    fn advance_waits_for_tdlib_to_report_the_new_window() {
        let mut r = StreamRanges::default();
        let size = 64 * STREAM_PRELOAD;
        r.need(0, size);
        r.update(&at(0, STREAM_PRELOAD, size));
        r.need(40 * STREAM_PRELOAD, size);
        // A late update for the old window must not move the new one.
        r.update(&at(0, 2 * STREAM_PRELOAD, size));
        assert_eq!(r.advance(48 * STREAM_PRELOAD, size), None);
    }
}
