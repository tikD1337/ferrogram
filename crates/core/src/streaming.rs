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
#[derive(Debug, Default)]
pub struct StreamRanges {
    requested: Option<u64>,
    offset: u64,
    prefix: u64,
}
impl StreamRanges {
    pub fn update(&mut self, file: &StreamFile) {
        if self.requested.is_none() || self.requested == Some(file.offset) {
            self.offset = file.offset;
            self.prefix = file.prefix;
        }
    }
    pub fn need(&mut self, offset: u64, size: u64) -> Option<u64> {
        if offset >= size {
            return None;
        }
        let aligned = offset / STREAM_PART * STREAM_PART;
        if let Some(requested) = self.requested {
            let prefix = if requested == self.offset {
                self.prefix
            } else {
                0
            };
            if offset >= requested
                && offset
                    < requested
                        .saturating_add(prefix)
                        .saturating_add(STREAM_PRELOAD)
            {
                return None;
            }
        }
        self.requested = Some(aligned);
        Some(aligned)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seek_coalesces_one_request_and_aligns_parts() {
        let mut r = StreamRanges::default();
        let size = 32 * STREAM_PRELOAD;
        assert_eq!(r.need(0, size), Some(0));
        assert_eq!(r.need(10, size), None);
        assert_eq!(
            r.need(3 * STREAM_PRELOAD + 17, size),
            Some(3 * STREAM_PRELOAD)
        );
        assert_eq!(r.need(3 * STREAM_PRELOAD + 18, size), None);
        assert_eq!(r.need(size, size), None);
        r.update(&StreamFile {
            id: 1,
            size,
            path: None,
            offset: 0,
            prefix: 1,
            completed: false,
        });
        assert_eq!(r.need(3 * STREAM_PRELOAD + 19, size), None);
        assert_eq!(r.need(0, size), Some(0));
    }
}
