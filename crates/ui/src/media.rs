//! Viewport media demand, one byte-bounded cache, and two blocking decoder workers.
use crate::AppWindow;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    collections::VecDeque,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
use tg_core::{Command, MediaFile, Photo};

// Shared by avatars and message photos. 24 MiB is the initial decoded-media budget;
// text rasters have the existing separate budget. Entries own the only media cache.
const CACHE_BYTES: usize = 24 * 1024 * 1024;
// Feed cells (<=64) + chat-list avatars (<=40) + the header avatar, with room to spare.
const MAX_DEMAND: usize = 112;
const JOBS: usize = 16;
const MAX_ENCODED: u64 = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub id: i32,
    pub width: u32,
    pub height: u32,
}
impl Key {
    pub fn new(photo: &Photo, width: f32, height: f32, scale: f32) -> Self {
        // Media quality is capped at 2x DPI; output is never larger than 1008 x 640.
        let scale = scale.clamp(0.5, 2.0);
        Self {
            id: photo.file.id,
            width: (width * scale).round().clamp(1.0, 1008.0) as u32,
            height: (height * scale).round().clamp(1.0, 640.0) as u32,
        }
    }
}
struct Cache<K, T> {
    entries: VecDeque<(K, T, usize)>,
    bytes: usize,
    limit: usize,
}
impl<K: PartialEq, T: Clone> Cache<K, T> {
    fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }
    fn get(&mut self, key: &K) -> Option<T> {
        let i = self.entries.iter().position(|e| &e.0 == key)?;
        let entry = self.entries.remove(i)?;
        let value = entry.1.clone();
        self.entries.push_back(entry);
        Some(value)
    }
    fn insert(&mut self, key: K, value: T, bytes: usize) {
        if let Some(i) = self.entries.iter().position(|e| e.0 == key)
            && let Some(e) = self.entries.remove(i)
        {
            self.bytes -= e.2;
        }
        if bytes > self.limit {
            return;
        }
        // Count bound also covers tiny/missing thumbnails and metadata overhead.
        while self.bytes + bytes > self.limit || self.entries.len() >= 512 {
            if let Some(e) = self.entries.pop_front() {
                self.bytes -= e.2;
            } else {
                break;
            }
        }
        self.bytes += bytes;
        self.entries.push_back((key, value, bytes));
    }
}
struct Demand {
    key: Key,
    photo: Photo,
    requested: bool,
    preview_done: bool,
    complete: bool,
    failed: bool,
    job: Option<Arc<AtomicBool>>,
}
enum Source {
    File(std::path::PathBuf),
    Mini(Vec<u8>),
}
struct Job {
    key: Key,
    source: Source,
    cancelled: Arc<AtomicBool>,
    preview: bool,
}
struct Decoded {
    key: Key,
    pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    cancelled: Arc<AtomicBool>,
    preview: bool,
}
pub(crate) struct Media {
    // At most MAX_DEMAND metadata entries, each mini <=8 KiB and path <=4 KiB.
    demand: Vec<Demand>,
    cache: Cache<Key, (Image, bool)>,
    jobs: mpsc::SyncSender<Job>,
    results: mpsc::Receiver<Decoded>,
}
impl Media {
    pub fn new(window: &AppWindow) -> Result<Self, std::io::Error> {
        let (jobs, receiver) = mpsc::sync_channel::<Job>(JOBS);
        let receiver = Arc::new(Mutex::new(receiver));
        // 2 results max: workers block here until the UI drains. No queued pixel clones.
        let (results, ready) = mpsc::sync_channel(2);
        for i in 0..2 {
            let receiver = receiver.clone();
            let results = results.clone();
            let weak = window.as_weak();
            std::thread::Builder::new()
                .name(format!("tg-image-{i}"))
                .spawn(move || {
                    loop {
                        let job = match receiver.lock() {
                            Ok(rx) => rx.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else { return };
                        let pixels = if job.cancelled.load(Ordering::Acquire) {
                            None
                        } else {
                            decode(&job).ok()
                        };
                        if results
                            .send(Decoded {
                                key: job.key,
                                pixels,
                                cancelled: job.cancelled,
                                preview: job.preview,
                            })
                            .is_err()
                        {
                            return;
                        }
                        if weak
                            .upgrade_in_event_loop(|w| w.invoke_media_ready())
                            .is_err()
                        {
                            return;
                        }
                    }
                })?;
        }
        Ok(Self {
            demand: Vec::new(),
            cache: Cache::new(CACHE_BYTES),
            jobs,
            results: ready,
        })
    }
    pub fn image(&mut self, key: Key) -> Image {
        self.cache.get(&key).map_or_else(Image::default, |v| v.0)
    }
    pub fn sync(&mut self, wanted: Vec<(Key, Photo)>) -> Vec<Command> {
        // At most 2 * MAX_DEMAND start/cancel actions, immediately dispatched by State.
        let mut commands = Vec::new();
        self.demand.retain(|d| {
            let keep = wanted.iter().take(MAX_DEMAND).any(|v| v.0 == d.key);
            if !keep {
                if let Some(job) = &d.job {
                    job.store(true, Ordering::Release);
                }
                if d.requested
                    && !wanted.iter().any(|v| v.0.id == d.key.id)
                    && !commands
                        .iter()
                        .any(|c| matches!(c,Command::CancelDownload(id) if *id == d.key.id))
                {
                    commands.push(Command::CancelDownload(d.key.id));
                }
            }
            keep
        });
        for (key, photo) in wanted.into_iter().take(MAX_DEMAND) {
            if self.demand.iter().any(|d| d.key == key) {
                continue;
            }
            let cached = self.cache.get(&key);
            let complete = cached.as_ref().is_some_and(|v| v.1);
            let requested = photo.file.id > 0 && photo.file.path.is_none() && !complete;
            if requested
                && !self
                    .demand
                    .iter()
                    .any(|d| d.key.id == key.id && d.requested)
            {
                commands.push(Command::DownloadFile(key.id));
            }
            self.demand.push(Demand {
                key,
                photo,
                requested,
                preview_done: cached.is_some(),
                complete,
                failed: false,
                job: None,
            });
        }
        self.schedule();
        commands
    }
    pub fn file(&mut self, file: MediaFile) {
        for d in &mut self.demand {
            if d.key.id == file.id && file.path.is_some() {
                d.photo.file.path.clone_from(&file.path);
            }
        }
        self.schedule();
    }
    fn schedule(&mut self) {
        for d in &mut self.demand {
            if d.job.is_some() || d.complete || d.failed {
                continue;
            }
            let (source, preview) = if !d.preview_done && !d.photo.minithumbnail.is_empty() {
                (Source::Mini(d.photo.minithumbnail.clone()), true)
            } else if let Some(path) = &d.photo.file.path {
                (Source::File(path.clone()), false)
            } else {
                continue;
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let job = Job {
                key: d.key,
                source,
                cancelled: cancelled.clone(),
                preview,
            };
            if self.jobs.try_send(job).is_ok() {
                d.job = Some(cancelled);
            }
        }
    }
    pub fn drain(&mut self) {
        while let Ok(result) = self.results.try_recv() {
            if result.cancelled.load(Ordering::Acquire) {
                continue;
            }
            let Some(d) = self.demand.iter_mut().find(|d| {
                d.key == result.key
                    && d.job
                        .as_ref()
                        .is_some_and(|j| Arc::ptr_eq(j, &result.cancelled))
            }) else {
                continue;
            };
            d.job = None;
            if result.preview {
                d.preview_done = true;
            } else {
                d.complete = true;
            }
            if let Some(pixels) = result.pixels {
                let bytes = pixels.as_bytes().len() + 128;
                self.cache.insert(
                    result.key,
                    (Image::from_rgba8(pixels), !result.preview),
                    bytes,
                );
            } else if !result.preview {
                d.failed = true;
            }
        }
        self.schedule();
    }
}
impl Drop for Media {
    fn drop(&mut self) {
        for d in &self.demand {
            if let Some(job) = &d.job {
                job.store(true, Ordering::Release);
            }
        }
    }
}
fn decode(job: &Job) -> Result<SharedPixelBuffer<Rgba8Pixel>, ()> {
    // Worker-only file I/O, input bounded before allocation; no path loading via Slint.
    let bytes = match &job.source {
        Source::Mini(bytes) => bytes.clone(),
        Source::File(path) => {
            let file = std::fs::File::open(path).map_err(|_| ())?;
            if file.metadata().map_err(|_| ())?.len() > MAX_ENCODED {
                return Err(());
            }
            let mut bytes = Vec::new();
            file.take(MAX_ENCODED + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ())?;
            if bytes.len() as u64 > MAX_ENCODED {
                return Err(());
            }
            bytes
        }
    };
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        use image::ImageDecoder;
        let decoder =
            image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes)).map_err(|_| ())?;
        let (width, height) = decoder.dimensions();
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 4 * 1024 * 1024 {
            return Err(());
        }
        // At most 16 MiB source pixels per worker; no original pixels escape to the UI/cache.
        let decoded = image::DynamicImage::from_decoder(decoder).map_err(|_| ())?;
        if job.cancelled.load(Ordering::Acquire) {
            return Err(());
        }
        let scaled = decoded
            .resize_exact(
                job.key.width,
                job.key.height,
                image::imageops::FilterType::Triangle,
            )
            .into_rgba8();
        return Ok(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            scaled.as_raw(),
            job.key.width,
            job.key.height,
        ));
    }
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(16 * 1024 * 1024);
    decoder.read_info().map_err(|_| ())?;
    let info = decoder.info().ok_or(())?;
    // Reject huge/progressive coefficient allocations before decode (<=4M source pixels).
    if u64::from(info.width) * u64::from(info.height) > 4 * 1024 * 1024 {
        return Err(());
    }
    // Cover scaling: the smallest source scale that still fills the target in both axes.
    // Requesting that size lets the DCT decode a thumbnail without the full-size bitmap.
    let (source_width, source_height) = (u32::from(info.width), u32::from(info.height));
    let factor = (job.key.width as f32 / source_width.max(1) as f32)
        .max(job.key.height as f32 / source_height.max(1) as f32)
        .min(1.0);
    let requested_width =
        ((source_width as f32 * factor).ceil() as u32).clamp(1, source_width.max(1));
    let requested_height =
        ((source_height as f32 * factor).ceil() as u32).clamp(1, source_height.max(1));
    decoder
        .scale(requested_width as u16, requested_height as u16)
        .map_err(|_| ())?;
    let pixels = decoder.decode().map_err(|_| ())?;
    let info = decoder.info().ok_or(())?;
    if job.cancelled.load(Ordering::Acquire) {
        return Err(());
    }
    let channels = info.pixel_format.pixel_bytes();
    if !matches!(channels, 1 | 3) {
        return Err(());
    }
    // Scaled DCT above never allocates an original-resolution RGB bitmap for a thumbnail.
    // Resample directly into the exact display-size shared buffer; no second image cache.
    let mut output = SharedPixelBuffer::<Rgba8Pixel>::new(job.key.width, job.key.height);
    // Centre crop to the target aspect ratio, so album cells are cropped, never distorted.
    let (width, height) = (u32::from(info.width).max(1), u32::from(info.height).max(1));
    let (crop_width, crop_height) = if u64::from(width) * u64::from(job.key.height)
        > u64::from(height) * u64::from(job.key.width)
    {
        (
            (u64::from(height) * u64::from(job.key.width) / u64::from(job.key.height)).max(1)
                as u32,
            height,
        )
    } else {
        (
            width,
            (u64::from(width) * u64::from(job.key.height) / u64::from(job.key.width)).max(1) as u32,
        )
    };
    let (left, top) = ((width - crop_width) / 2, (height - crop_height) / 2);
    for y in 0..job.key.height {
        for x in 0..job.key.width {
            let sx = left + x * crop_width / job.key.width;
            let sy = top + y * crop_height / job.key.height;
            let i = (sy * u32::from(info.width) + sx) as usize * channels;
            let (r, g, b) = if channels == 3 {
                (pixels[i], pixels[i + 1], pixels[i + 2])
            } else {
                (pixels[i], pixels[i], pixels[i])
            };
            output.make_mut_slice()[(y * job.key.width + x) as usize] =
                Rgba8Pixel::new(r, g, b, 255);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn media() -> (Media, mpsc::Receiver<Job>) {
        let (jobs, rx) = mpsc::sync_channel(JOBS);
        let (_, results) = mpsc::sync_channel(2);
        (
            Media {
                demand: Vec::new(),
                cache: Cache::new(CACHE_BYTES),
                jobs,
                results,
            },
            rx,
        )
    }
    fn photo(id: i32) -> Photo {
        Photo {
            file: MediaFile {
                size: 0,
                downloaded: 0,
                downloading: false,
                id,
                path: None,
            },
            width: 1008,
            height: 640,
            minithumbnail: include_bytes!("../../app/assets/demo-mini.jpg").to_vec(),
        }
    }
    #[test]
    fn webp_sticker_preserves_alpha_and_decodes_only_to_display_buffer() {
        let job = Job {
            key: Key {
                id: 7,
                width: 90,
                height: 90,
            },
            source: Source::Mini(include_bytes!("../../app/assets/demo-sticker.webp").to_vec()),
            cancelled: Arc::new(AtomicBool::new(false)),
            preview: false,
        };
        let pixels = decode(&job).unwrap();
        assert_eq!(pixels.as_bytes().len(), 90 * 90 * 4);
        assert_eq!(pixels.as_slice()[0].a, 0);
        assert!(pixels.as_slice().iter().any(|p| p.a == 255));
    }
    #[test]
    fn document_minithumbnail_never_prefetches_the_document() {
        let (mut media, rx) = media();
        let p = photo(-42);
        let key = Key::new(&p, 48.0, 48.0, 1.0);
        assert!(media.sync(vec![(key, p)]).is_empty());
        assert!(rx.try_recv().unwrap().preview);
    }
    #[test]
    fn demand_deduplicates_downloads_and_cancels_only_after_last_consumer() {
        let (mut media, rx) = media();
        let p = photo(42);
        let key = Key::new(&p, 44.0, 44.0, 1.0);
        let other = Key::new(&p, 88.0, 88.0, 1.0);
        let commands = media.sync(vec![(key, p.clone()), (key, p.clone()), (other, p.clone())]);
        assert!(matches!(commands.as_slice(), [Command::DownloadFile(42)]));
        assert_eq!(media.demand.len(), 2);
        let first = rx.try_recv().unwrap();
        assert!(first.preview);
        assert!(media.sync(vec![(other, p)]).is_empty());
        assert!(first.cancelled.load(Ordering::Acquire));
        assert!(matches!(
            media.sync(vec![]).as_slice(),
            [Command::CancelDownload(42)]
        ));
    }
    #[test]
    fn cached_full_image_is_shared_without_a_new_download() {
        let (mut media, rx) = media();
        let p = photo(42);
        let key = Key::new(&p, 44.0, 44.0, 1.0);
        media
            .cache
            .insert(key, (Image::default(), true), 44 * 44 * 4);
        assert!(media.sync(vec![(key, p)]).is_empty());
        assert!(rx.try_recv().is_err());
        assert!(media.demand[0].complete);
    }
    #[test]
    fn offscreen_file_updates_do_not_enqueue_decodes() {
        let (mut media, rx) = media();
        media.file(MediaFile {
            size: 0,
            downloaded: 0,
            downloading: false,
            id: 99,
            path: Some("synthetic.jpg".into()),
        });
        assert!(rx.try_recv().is_err());
        assert!(media.demand.is_empty());
    }
    #[test]
    fn jpeg_decodes_to_display_dimensions_and_corrupt_input_is_rejected() {
        let key = Key {
            id: 1,
            width: 63,
            height: 40,
        };
        let mut job = Job {
            key,
            source: Source::Mini(include_bytes!("../../app/assets/demo-photo.jpg").to_vec()),
            cancelled: Arc::new(AtomicBool::new(false)),
            preview: false,
        };
        let image = decode(&job).unwrap();
        assert_eq!((image.width(), image.height()), (63, 40));
        assert_eq!(image.as_bytes().len(), 63 * 40 * 4);
        job.source = Source::Mini(vec![1, 2, 3]);
        assert!(decode(&job).is_err());
        job.cancelled.store(true, Ordering::Release);
        assert!(decode(&job).is_err());
    }
    #[test]
    fn queue_saturation_defers_work_with_bounded_demand() {
        let (mut media, rx) = media();
        let wanted = (1..=200)
            .map(|id| {
                let p = photo(id);
                (Key::new(&p, 44.0, 44.0, 1.0), p)
            })
            .collect();
        media.sync(wanted);
        assert_eq!(media.demand.len(), MAX_DEMAND);
        assert_eq!(rx.try_iter().count(), JOBS);
        assert_eq!(
            media.demand.iter().filter(|d| d.job.is_some()).count(),
            JOBS
        );
    }
    #[test]
    fn late_cancelled_pixels_do_not_enter_cache() {
        let (mut media, _) = media();
        let (sender, results) = mpsc::sync_channel(2);
        media.results = results;
        let cancelled = Arc::new(AtomicBool::new(true));
        let key = Key {
            id: 42,
            width: 1,
            height: 1,
        };
        sender
            .send(Decoded {
                key,
                pixels: Some(SharedPixelBuffer::new(1, 1)),
                cancelled,
                preview: false,
            })
            .unwrap();
        media.drain();
        assert!(media.cache.entries.is_empty());
    }
    #[test]
    fn lru_evicts_by_bytes_and_get_promotes() {
        let mut cache = Cache::new(12);
        cache.insert(1, 11, 4);
        cache.insert(2, 22, 4);
        cache.insert(3, 33, 4);
        assert_eq!(cache.get(&1), Some(11));
        cache.insert(4, 44, 5);
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), None);
        assert_eq!(cache.get(&1), Some(11));
        assert!(cache.bytes <= 12);
        cache.insert(5, 55, 13);
        assert_eq!(cache.get(&5), None);
    }
}
