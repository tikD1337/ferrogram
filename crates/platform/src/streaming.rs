//! Event-driven partial-file reader. Only availability metadata is retained, never media bytes.
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Condvar, Mutex},
};
use tg_core::{StreamFile, StreamStatus};
/// Upper bound for one read into Media Foundation's own buffer. The MP4 source reads a whole
/// sample at a time, and a high-bitrate keyframe can exceed the 1 MiB preload; nothing is copied
/// or kept here, so the bound only rejects nonsensical requests.
pub(crate) const MAX_READ: usize = 16 * 1024 * 1024;
pub(crate) type Report = Arc<dyn Fn(StreamStatus) + Send + Sync>;
pub(crate) struct Reader {
    state: Mutex<State>,
    // One open handle survives TDLib renaming the partial file on completion; no byte cache.
    disk: Mutex<Option<(std::path::PathBuf, File)>>,
    wake: Condvar,
    report: Report,
    // The last waiting state sent to the UI. Held while deciding and sending, so reports cannot
    // reorder. Lock order: `reported`, then `state`; never take `reported` while holding `state`.
    reported: Mutex<bool>,
    pub file: i32,
    pub token: u64,
    pub length: u64,
}
struct State {
    file: StreamFile,
    ranges: Vec<(u64, u64)>,
    position: u64,
    generation: u64,
    revision: u64,
    closed: bool,
}
impl Reader {
    pub fn new(file: StreamFile, token: u64, report: Report) -> Arc<Self> {
        let reader = Arc::new(Self {
            file: file.id,
            length: file.size,
            token,
            disk: Mutex::new(None),
            state: Mutex::new(State {
                file: file.clone(),
                ranges: vec![],
                position: 0,
                generation: 0,
                revision: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            report,
            reported: Mutex::new(false),
        });
        reader.update(file);
        reader
    }
    pub fn update(&self, mut file: StreamFile) {
        if file.id != self.file || file.size != self.length || file.offset > self.length {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let end = if file.completed {
                self.length
            } else {
                file.offset.saturating_add(file.prefix).min(self.length)
            };
            let start = if file.completed { 0 } else { file.offset };
            if end > start {
                state.ranges.push((start, end));
                state.ranges.sort_unstable();
                let mut merged: Vec<(u64, u64)> = Vec::with_capacity(65);
                for &(a, b) in &state.ranges {
                    if let Some(last) = merged.last_mut()
                        && last.1 >= a
                    {
                        last.1 = last.1.max(b);
                    } else {
                        merged.push((a, b));
                    }
                }
                // At most 64 metadata ranges. Preserve the container header at offset zero.
                if merged.len() > 64 {
                    let drop = merged
                        .iter()
                        .position(|&(a, b)| a != 0 && !(a <= start && start < b))
                        .unwrap_or(0);
                    merged.remove(drop);
                }
                state.ranges = merged;
            }
            if file.path.is_none() {
                file.path = state.file.path.clone();
            }
            state.file = file;
            state.revision = state.revision.wrapping_add(1);
            self.wake.notify_all();
        }
    }
    pub fn close(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.closed = true;
            self.wake.notify_all();
        }
    }
    pub fn closed(&self) -> bool {
        self.state.lock().map_or(true, |s| s.closed)
    }
    pub fn position(&self) -> Result<u64, ()> {
        self.state.lock().map(|s| s.position).map_err(|_| ())
    }
    pub fn seek(&self, offset: i64, current: bool) -> Result<u64, ()> {
        // Clear the waiting state in the same section that starts the new generation: a read of
        // the new generation can only report after this, so its request is never suppressed.
        let mut reported = self.reported.lock().map_err(|_| ())?;
        let mut s = self.state.lock().map_err(|_| ())?;
        if s.closed {
            return Err(());
        }
        let position = if current {
            s.position.checked_add_signed(offset)
        } else {
            u64::try_from(offset).ok()
        }
        .filter(|p| *p <= self.length)
        .ok_or(())?;
        s.position = position;
        s.generation = s.generation.wrapping_add(1);
        self.wake.notify_all();
        drop(s);
        if std::mem::replace(&mut *reported, false) {
            self.send(None, false);
        }
        Ok(position)
    }
    /// Publishes a read's waiting state. A read cancelled by a seek must not publish: its
    /// stale state would hide the request of the read that replaced it.
    /// `needed` is the first missing byte and the end of the blocked read: the whole read is
    /// requested, so a read larger than one download window cannot stay waiting.
    fn report_wait(&self, generation: u64, needed: Option<(u64, u64)>, waiting: bool) {
        let Ok(mut reported) = self.reported.lock() else {
            return;
        };
        if self
            .state
            .lock()
            .map_or(true, |s| s.generation != generation)
        {
            return;
        }
        if *reported != waiting {
            *reported = waiting;
            self.send(needed, waiting);
        }
    }
    fn send(&self, needed: Option<(u64, u64)>, waiting: bool) {
        (self.report)(StreamStatus {
            file: self.file,
            token: self.token,
            needed: needed.map(|(first, _)| first),
            until: needed.map_or(0, |(_, until)| until),
            waiting,
        });
    }
    pub fn epoch(&self) -> Result<u64, ()> {
        self.state.lock().map(|s| s.generation).map_err(|_| ())
    }
    pub fn read_epoch(&self, output: &mut [u8], epoch: u64) -> Result<usize, ()> {
        self.read_inner(output, Some(epoch))
    }
    pub fn read(&self, output: &mut [u8]) -> Result<usize, ()> {
        self.read_inner(output, None)
    }
    fn read_inner(&self, output: &mut [u8], epoch: Option<u64>) -> Result<usize, ()> {
        if output.len() > MAX_READ {
            return Err(());
        }
        let mut s = self.state.lock().map_err(|_| ())?;
        if epoch.is_some_and(|epoch| epoch != s.generation) {
            return Err(());
        }
        let generation = s.generation;
        let offset = s.position;
        let mut waiting = false;
        loop {
            if s.closed || s.generation != generation {
                return Err(());
            }
            if offset == self.length || output.is_empty() {
                return Ok(0);
            }
            let end = s
                .ranges
                .iter()
                .find(|&&(a, b)| a <= offset && offset < b)
                .map(|r| r.1);
            let count = output.len().min((self.length - offset) as usize);
            // MF's MP4 source requires the requested count, except at the real EOF.
            // A short available prefix is not EOF: wait rather than silently skip samples.
            if let (Some(_), Some(path)) = (
                end.filter(|end| end - offset >= count as u64),
                s.file.path.clone(),
            ) {
                let revision = s.revision;
                drop(s);
                let result = (|| -> std::io::Result<()> {
                    let mut disk = self
                        .disk
                        .lock()
                        .map_err(|_| std::io::Error::other("Reader lock"))?;
                    if disk.as_ref().is_none_or(|(cached, _)| *cached != path) {
                        *disk = Some((path.clone(), File::open(&path)?));
                    }
                    let Some((_, file)) = disk.as_mut() else {
                        return Err(std::io::Error::other("Reader handle"));
                    };
                    file.seek(SeekFrom::Start(offset))?;
                    file.read_exact(&mut output[..count])
                })();
                s = self.state.lock().map_err(|_| ())?;
                if let Err(error) = result {
                    if error.kind() != std::io::ErrorKind::NotFound
                        || (s.file.completed && s.revision == revision)
                    {
                        return Err(());
                    }
                    // The completion rename can precede its updateFile. Wait for that update,
                    // seek or close instead of polling the old pathname or treating a hole as EOF.
                    if !waiting {
                        waiting = true;
                        drop(s);
                        self.report_wait(generation, Some((offset, offset + count as u64)), true);
                        s = self.state.lock().map_err(|_| ())?;
                    }
                    s = self
                        .wake
                        .wait_while(s, |s| {
                            !s.closed && s.generation == generation && s.revision == revision
                        })
                        .map_err(|_| ())?;
                    continue;
                }
                if s.closed || s.generation != generation {
                    return Err(());
                }
                s.position = offset + count as u64;
                drop(s);
                self.report_wait(generation, None, false);
                return Ok(count);
            }
            if !waiting {
                waiting = true;
                drop(s);
                self.report_wait(
                    generation,
                    Some((end.unwrap_or(offset), offset + count as u64)),
                    true,
                );
                s = self.state.lock().map_err(|_| ())?;
                continue;
            }
            s = self.wake.wait(s).map_err(|_| ())?;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tg_core::STREAM_PRELOAD;
    fn reader() -> Arc<Reader> {
        Reader::new(
            StreamFile {
                id: 1,
                size: 2 * STREAM_PRELOAD,
                path: None,
                offset: 0,
                prefix: 0,
                completed: false,
            },
            7,
            Arc::new(|_| {}),
        )
    }
    #[test]
    fn close_releases_a_blocked_read_and_validates_boundaries() {
        let r = reader();
        let worker = r.clone();
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || {
            let _ = tx.send(worker.read(&mut [0; 4]));
        });
        r.close();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().is_err());
        t.join().unwrap();
    }
    #[test]
    fn eos_offsets_and_read_bounds() {
        let r = reader();
        assert!(r.seek(-1, false).is_err());
        assert!(r.seek(i64::MAX, false).is_err());
        assert!(r.read(&mut vec![0; MAX_READ + 1]).is_err());
        r.seek(r.length as i64, false).unwrap();
        assert_eq!(r.read(&mut [0; 4]), Ok(0));
    }
}

#[cfg(test)]
mod partial_tests {
    use super::*;
    #[test]
    fn a_short_prefix_waits_for_the_requested_count_instead_of_signalling_eof() {
        let path =
            std::env::temp_dir().join(format!("ferrogram-full-read-{}.bin", std::process::id()));
        std::fs::write(&path, b"header-tail").unwrap();
        let mut file = StreamFile {
            id: 1,
            size: 11,
            path: Some(path.clone()),
            offset: 0,
            prefix: 6,
            completed: false,
        };
        let (wait, waited) = std::sync::mpsc::channel();
        let r = Reader::new(
            file.clone(),
            1,
            Arc::new(move |s| {
                let _ = wait.send(s);
            }),
        );
        let worker = r.clone();
        let (done, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut bytes = [0; 11];
            let status = worker.read(&mut bytes);
            let _ = done.send((status, bytes));
        });
        let status = waited
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(status.needed, Some(6));
        assert_eq!(status.until, 11, "the whole blocked read is reported");
        assert!(result.try_recv().is_err());
        file.prefix = 11;
        r.update(file);
        let (status, bytes) = result
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(status, Ok(11));
        assert_eq!(&bytes, b"header-tail");
        thread.join().unwrap();
        r.close();
        drop(r);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn available_ranges_read_real_offsets_and_preserve_the_header() {
        let path =
            std::env::temp_dir().join(format!("ferrogram-stream-test-{}.bin", std::process::id()));
        let mut bytes = vec![0u8; 256 * 1024];
        bytes[..4].copy_from_slice(b"head");
        bytes[128 * 1024..128 * 1024 + 4].copy_from_slice(b"tail");
        std::fs::write(&path, bytes).unwrap();
        let mut file = StreamFile {
            id: 1,
            size: 256 * 1024,
            path: Some(path.clone()),
            offset: 0,
            prefix: 4,
            completed: false,
        };
        let r = Reader::new(file.clone(), 1, Arc::new(|_| {}));
        let mut out = [0; 4];
        assert_eq!(r.read(&mut out), Ok(4));
        assert_eq!(&out, b"head");
        file.offset = 128 * 1024;
        file.prefix = 4;
        r.update(file);
        r.seek(128 * 1024, false).unwrap();
        assert_eq!(r.read(&mut out), Ok(4));
        assert_eq!(&out, b"tail");
        r.seek(0, false).unwrap();
        assert_eq!(r.read(&mut out), Ok(4));
        assert_eq!(&out, b"head");
        r.close();
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod range_limit_tests {
    use super::*;
    use tg_core::STREAM_PRELOAD;
    #[test]
    fn many_seeks_keep_header_and_the_newest_range() {
        let mut file = StreamFile {
            id: 1,
            size: 256 * STREAM_PRELOAD,
            path: None,
            offset: 0,
            prefix: 16,
            completed: false,
        };
        let r = Reader::new(file.clone(), 1, Arc::new(|_| {}));
        for n in 1..=70 {
            file.offset = n * 2 * STREAM_PRELOAD;
            r.update(file.clone());
        }
        file.offset = 128 * 1024;
        r.update(file);
        let s = r.state.lock().unwrap();
        assert!(s.ranges.len() <= 64);
        assert!(s.ranges.contains(&(0, 16)));
        assert!(s.ranges.contains(&(128 * 1024, 128 * 1024 + 16)));
    }
}

#[cfg(test)]
mod end_tests {
    use super::*;
    #[test]
    fn read_crossing_the_end_returns_only_the_remaining_bytes() {
        let path =
            std::env::temp_dir().join(format!("ferrogram-stream-eof-{}.bin", std::process::id()));
        std::fs::write(&path, b"head-extra").unwrap();
        let r = Reader::new(
            StreamFile {
                id: 1,
                size: 4,
                path: Some(path.clone()),
                offset: 0,
                prefix: 4,
                completed: true,
            },
            1,
            Arc::new(|_| {}),
        );
        let mut bytes = [0; 8];
        assert_eq!(r.read(&mut bytes), Ok(4));
        assert_eq!(&bytes[..4], b"head");
        assert_eq!(r.read(&mut bytes), Ok(0));
        assert!(r.seek(5, false).is_err());
        r.close();
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    #[test]
    fn open_partial_handle_survives_the_completion_rename() {
        let partial = std::env::temp_dir().join(format!(
            "ferrogram-stream-rename-{}.part",
            std::process::id()
        ));
        let complete = partial.with_extension("complete");
        std::fs::write(&partial, b"header-tail").unwrap();
        let mut f = StreamFile {
            id: 1,
            size: 11,
            path: Some(partial.clone()),
            offset: 0,
            prefix: 11,
            completed: false,
        };
        let r = Reader::new(f.clone(), 1, Arc::new(|_| {}));
        let mut bytes = [0; 6];
        assert_eq!(r.read(&mut bytes), Ok(6));
        std::fs::rename(&partial, &complete).unwrap();
        r.seek(0, false).unwrap();
        assert_eq!(r.read(&mut bytes), Ok(6));
        assert_eq!(&bytes, b"header");
        f.path = Some(complete.clone());
        f.completed = true;
        r.update(f);
        r.seek(6, false).unwrap();
        assert_eq!(r.read(&mut bytes), Ok(5));
        assert_eq!(&bytes[..5], b"-tail");
        r.close();
        drop(r);
        std::fs::remove_file(complete).unwrap();
    }
}

#[cfg(test)]
mod late_path_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    #[test]
    fn first_open_waits_for_a_renamed_path_update_instead_of_eof() {
        let old =
            std::env::temp_dir().join(format!("ferrogram-stream-late-{}.part", std::process::id()));
        let new = old.with_extension("complete");
        std::fs::write(&new, b"header").unwrap();
        let file = StreamFile {
            id: 1,
            size: 6,
            path: Some(old),
            offset: 0,
            prefix: 6,
            completed: false,
        };
        let (tx, rx) = mpsc::sync_channel(2);
        let r = Reader::new(
            file.clone(),
            1,
            Arc::new(move |s| {
                let _ = tx.try_send(s);
            }),
        );
        let worker = r.clone();
        let (done, result) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut bytes = [0; 6];
            let status = worker.read(&mut bytes);
            let _ = done.send((status, bytes));
        });
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap().waiting);
        let mut complete = file;
        complete.path = Some(new.clone());
        complete.completed = true;
        r.update(complete);
        let (status, bytes) = result.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(status, Ok(6));
        assert_eq!(&bytes, b"header");
        thread.join().unwrap();
        r.close();
        drop(r);
        std::fs::remove_file(new).unwrap();
    }
}

#[cfg(test)]
mod stale_wait_tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn a_read_cancelled_by_a_seek_cannot_suppress_the_next_reads_request() {
        let (tx, rx) = mpsc::channel();
        let r = Reader::new(
            StreamFile {
                id: 1,
                size: 1024,
                path: None,
                offset: 0,
                prefix: 0,
                completed: false,
            },
            1,
            Arc::new(move |s| {
                let _ = tx.send(s);
            }),
        );
        // A read of the old generation reaches its report only after a backward seek.
        let old = r.epoch().unwrap();
        r.seek(100, false).unwrap();
        r.report_wait(old, Some((900, 1000)), true);
        assert!(rx.try_recv().is_err(), "a cancelled read must not publish");
        // The read that replaced it must still request its own range.
        r.report_wait(old.wrapping_add(1), Some((100, 200)), true);
        let status = rx.try_recv().unwrap();
        assert!(status.waiting);
        assert_eq!(status.needed, Some(100));
        // A seek clears the published waiting state before the next generation can report.
        r.seek(0, false).unwrap();
        assert!(!rx.try_recv().unwrap().waiting);
        r.close();
    }
}

#[cfg(test)]
mod queued_seek_tests {
    use super::*;
    #[test]
    fn queued_read_is_cancelled_when_seek_precedes_worker_execution() {
        let path =
            std::env::temp_dir().join(format!("ferrogram-stream-epoch-{}.bin", std::process::id()));
        std::fs::write(&path, b"head").unwrap();
        let r = Reader::new(
            StreamFile {
                id: 1,
                size: 4,
                path: Some(path.clone()),
                offset: 0,
                prefix: 4,
                completed: true,
            },
            1,
            Arc::new(|_| {}),
        );
        let epoch = r.epoch().unwrap();
        r.seek(0, false).unwrap();
        let result = r.read_epoch(&mut [0; 4], epoch);
        r.close();
        drop(r);
        std::fs::remove_file(path).unwrap();
        assert!(
            result.is_err(),
            "Queued pre-seek read must not complete against the new cursor"
        );
    }
}
