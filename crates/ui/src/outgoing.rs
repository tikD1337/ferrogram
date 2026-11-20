//! Outgoing attachments: one draft, bounded workers, local paths handed to TDLib.
use super::*;
use std::path::Path;
use std::sync::mpsc;

type ResultSlot = mpsc::Receiver<Result<Option<Prepared>, String>>;
/// Receives a borrowed clipboard DIB and returns whether the clipboard could be read.
pub type ClipboardReader = fn(&mut dyn FnMut(&[u8])) -> Result<(), &'static str>;

/// Native attachment sources, injected by tg-app. Errors are short user-facing reasons.
#[derive(Debug, Clone, Copy)]
pub struct AttachmentSources {
    /// A modal open-file dialog owned by the given Win32 window handle; `Ok(None)` on cancel.
    /// Runs on an attachment worker, never on the UI thread.
    pub pick: fn(isize) -> Result<Option<PathBuf>, &'static str>,
    /// A cheap clipboard format query, called on the UI thread for Ctrl+V. True only for a
    /// bitmap without text, so copied spreadsheet cells still paste as text.
    pub has_image: fn() -> bool,
    /// Lends the clipboard DIB (no file header) to the reader while it stays locked.
    /// Runs on an attachment worker.
    pub read_image: ClipboardReader,
}

impl Outgoing {
    pub(super) fn clear_uploads(&self) {
        self.uploads.borrow_mut().clear();
    }
}

#[derive(Default)]
pub(super) struct Outgoing {
    pub(super) sources: Cell<Option<AttachmentSources>>,
    pending: RefCell<Option<Prepared>>,
    uploads: RefCell<Vec<Prepared>>,
    busy: Cell<bool>,
    generation: Cell<u64>,
    result: RefCell<Option<(u64, tg_core::ChatId, ResultSlot)>>,
}

struct Prepared {
    path: PathBuf,
    temporary: bool,
    photo: bool,
    size: u64,
    pixels: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits
}

fn thumbnail(path: &Path) -> Result<slint::SharedPixelBuffer<slint::Rgba8Pixel>, String> {
    use image::ImageDecoder;
    let file = std::fs::File::open(path).map_err(|_| "Could not open the image.")?;
    // Limit encoded input as well as decoder allocations. A large photo remains sendable as a file.
    if file
        .metadata()
        .map_err(|_| "Could not inspect the image.")?
        .len()
        > 32 * 1024 * 1024
    {
        return Err("Image preview exceeds the size limit.".into());
    }
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|_| "Unsupported image.")?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("Send this image format as a file.".into());
    }
    reader.limits(image_limits());
    let decoder = reader.into_decoder().map_err(|_| "Unsupported image.")?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 8 * 1024 * 1024 {
        return Err("Image preview exceeds the 8 megapixel limit.".into());
    }
    let pixels = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| "Could not decode the image.")?
        .thumbnail(320, 200)
        .into_rgba8();
    Ok(slint::SharedPixelBuffer::clone_from_slice(
        pixels.as_raw(),
        pixels.width(),
        pixels.height(),
    ))
}

/// Telegram rejects photos above 10 MB; larger PNG/JPEG files default to "send as file".
const PHOTO_BYTES: u64 = 10 * 1024 * 1024;

/// `canonicalize` returns verbatim paths (`\\?\C:\...`). Drive paths short enough for classic
/// Win32 file APIs reach TDLib without that prefix; UNC and long paths keep it.
fn plain_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.len() < 260 && rest.as_bytes().get(1) == Some(&b':') => rest.into(),
        _ => path,
    }
}

fn prepare(path: PathBuf, temporary: bool) -> Result<Prepared, String> {
    let mut prepared = Prepared {
        path,
        temporary,
        photo: false,
        size: 0,
        pixels: None,
    };
    let metadata =
        std::fs::metadata(&prepared.path).map_err(|_| "Could not access the selected file.")?;
    if !metadata.is_file() {
        return Err("Select a regular file.".into());
    }
    prepared.path = plain_path(
        std::fs::canonicalize(&prepared.path)
            .map_err(|_| "Could not resolve the selected file.")?,
    );
    let path = prepared
        .path
        .to_str()
        .ok_or("The file path is not valid Unicode.")?;
    if path.len() > 4096 {
        return Err("The file path is too long.".into());
    }
    prepared.size = metadata.len();
    let ext = prepared
        .path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        use std::io::Read;
        let mut header = [0u8; 16];
        if let Ok(mut file) = std::fs::File::open(&prepared.path)
            && let Ok(n) = file.read(&mut header)
        {
            prepared.photo = matches!(
                image::guess_format(&header[..n]),
                Ok(image::ImageFormat::Png | image::ImageFormat::Jpeg)
            );
        }
        if prepared.photo {
            prepared.pixels = thumbnail(&prepared.path).ok();
        }
    }
    Ok(prepared)
}

/// Decodes a packed clipboard DIB. Dimensions are checked before the decoder allocates.
pub(super) fn decode_dib(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    use image::ImageDecoder;
    if bytes.len() < 40 {
        return Err("Invalid clipboard bitmap.".into());
    }
    let width = i32::from_le_bytes(bytes[4..8].try_into().map_err(|_| "Invalid bitmap.")?);
    let height = i32::from_le_bytes(bytes[8..12].try_into().map_err(|_| "Invalid bitmap.")?);
    if width <= 0
        || height == 0
        || u64::from(width.unsigned_abs()) * u64::from(height.unsigned_abs()) > 8 * 1024 * 1024
    {
        return Err("Clipboard image exceeds the 8 megapixel limit.".into());
    }
    let mut decoder =
        image::codecs::bmp::BmpDecoder::new_without_file_header(std::io::Cursor::new(bytes))
            .map_err(|_| "Unsupported clipboard bitmap.")?;
    decoder
        .set_limits(image_limits())
        .map_err(|_| "Clipboard image is too large.")?;
    image::DynamicImage::from_decoder(decoder)
        .map_err(|_| "Could not decode clipboard image.".into())
}

fn clipboard_file(sources: AttachmentSources) -> Result<Prepared, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    // Decoding happens while the clipboard memory is lent out, so no raw copy is made.
    let mut decoded = Err(String::from("No image in the clipboard."));
    (sources.read_image)(&mut |bytes| decoded = decode_dib(bytes))?;
    let image = decoded?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Invalid system clock.")?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "veygram-paste-{}-{now}-{}.png",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "Could not create a temporary image.")?;
    // The guard removes incomplete files on every error. No original clipboard pixels survive this worker.
    let mut guard = Prepared {
        path: path.clone(),
        temporary: true,
        photo: true,
        size: 0,
        pixels: None,
    };
    let mut writer = std::io::BufWriter::new(file);
    image
        .write_to(&mut writer, image::ImageFormat::Png)
        .map_err(|_| "Could not save the clipboard image.")?;
    std::io::Write::flush(&mut writer).map_err(|_| "Could not save the clipboard image.")?;
    drop(writer);
    drop(image);
    let mut prepared = prepare(path, false)?;
    // Transfer cleanup ownership only after preparation succeeds.
    prepared.temporary = true;
    guard.temporary = false;
    Ok(prepared)
}

impl State {
    pub(super) fn cancel_outgoing(&self, w: &AppWindow) {
        self.outgoing
            .generation
            .set(self.outgoing.generation.get().wrapping_add(1));
        self.outgoing.pending.borrow_mut().take();
        w.set_outgoing_open(false);
        w.set_outgoing_preview(Default::default());
        w.set_outgoing_caption("".into());
    }
    /// Returns whether a preparation worker was started. A declined paste falls through to
    /// the composer's normal text paste.
    fn start_outgoing(&self, w: &AppWindow, paste: bool) -> bool {
        if w.get_blocked()
            || !w.get_ready()
            || w.get_editing()
            || w.get_outgoing_open()
            || self.outgoing.busy.get()
        {
            return false;
        }
        let Some(chat) = self.feed.history.borrow().chat else {
            return false;
        };
        let Some(sources) = self.outgoing.sources.get() else {
            w.set_error("Attaching files is not available.".into());
            return false;
        };
        if self.outgoing.uploads.borrow().len() >= 8 {
            w.set_error("Wait for one of the eight pending file uploads to finish.".into());
            return false;
        }
        let generation = self.outgoing.generation.get();
        let (tx, rx) = mpsc::sync_channel(1);
        let owner = native_window(w.window()).unwrap_or_default();
        let weak = w.as_weak();
        self.outgoing.busy.set(true);
        w.set_attachment_busy(true);
        *self.outgoing.result.borrow_mut() = Some((generation, chat, rx));
        let spawned = std::thread::Builder::new()
            .name("tg-attachment".into())
            .spawn(move || {
                let result = if paste {
                    clipboard_file(sources).map(Some)
                } else {
                    (sources.pick)(owner)
                        .map_err(String::from)
                        .and_then(|path| path.map(|p| prepare(p, false)).transpose())
                };
                let _ = tx.send(result);
                let _ = weak.upgrade_in_event_loop(|w| w.invoke_outgoing_ready());
            });
        if spawned.is_err() {
            self.outgoing.busy.set(false);
            self.outgoing.result.borrow_mut().take();
            w.set_attachment_busy(false);
            w.set_error("Could not start file preparation.".into());
            return false;
        }
        true
    }
    fn outgoing_ready(&self, w: &AppWindow) {
        let Some((generation, chat, rx)) = self.outgoing.result.borrow_mut().take() else {
            return;
        };
        self.outgoing.busy.set(false);
        w.set_attachment_busy(false);
        let result = rx.try_recv();
        if generation != self.outgoing.generation.get()
            || self.feed.history.borrow().chat != Some(chat)
            || !w.get_ready()
        {
            return;
        }
        match result {
            Ok(Ok(Some(prepared))) => {
                self.cancel_action(w);
                w.set_outgoing_name(
                    prepared
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref()
                        .into(),
                );
                w.set_outgoing_size(format!("{} bytes", prepared.size).into());
                w.set_outgoing_photo(prepared.photo);
                w.set_outgoing_has_preview(prepared.pixels.is_some());
                w.set_outgoing_as_file(!prepared.photo || prepared.size > PHOTO_BYTES);
                w.set_outgoing_preview(
                    prepared
                        .pixels
                        .clone()
                        .map(slint::Image::from_rgba8)
                        .unwrap_or_default(),
                );
                *self.outgoing.pending.borrow_mut() = Some(prepared);
                w.set_outgoing_open(true);
                w.set_error("".into());
            }
            Ok(Err(error)) => w.set_error(error.into()),
            Err(_) => w.set_error("File preparation failed.".into()),
            Ok(Ok(None)) => {}
        }
    }
    pub(super) fn send_outgoing(&self, w: &AppWindow) {
        let Some(chat) = self.feed.history.borrow().chat else {
            return;
        };
        let caption = w.get_outgoing_caption().to_string();
        if caption.chars().count() > 1024 {
            w.set_error("Caption exceeds 1024 characters.".into());
            return;
        }
        let command = {
            let pending = self.outgoing.pending.borrow();
            let Some(pending) = pending.as_ref() else {
                return;
            };
            let caption = (!caption.is_empty()).then_some(caption);
            if pending.photo && !w.get_outgoing_as_file() {
                Command::SendPhoto {
                    chat,
                    path: pending.path.clone(),
                    caption,
                }
            } else {
                Command::SendDocument {
                    chat,
                    path: pending.path.clone(),
                    caption,
                }
            }
        };
        if self.send(w, command) {
            if let Some(mut pending) = self.outgoing.pending.borrow_mut().take() {
                pending.pixels = None;
                self.outgoing.uploads.borrow_mut().push(pending);
            }
            self.cancel_outgoing(w);
            w.invoke_focus_composer();
        }
    }
    pub(super) fn upload_finished(&self, path: &Path) {
        let mut uploads = self.outgoing.uploads.borrow_mut();
        // One event per finished upload: release one slot even if the same file is queued twice.
        if let Some(index) = uploads.iter().position(|p| p.path == path) {
            uploads.remove(index);
        }
    }
}

pub(super) fn bind(w: &AppWindow, state: &Rc<State>) {
    let weak = w.as_weak();
    let s = state.clone();
    w.on_attach_file(move || {
        if let Some(w) = weak.upgrade() {
            s.start_outgoing(&w, false);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_paste_image(move || {
        if !s
            .outgoing
            .sources
            .get()
            .is_some_and(|sources| (sources.has_image)())
        {
            return false;
        }
        weak.upgrade().is_some_and(|w| s.start_outgoing(&w, true))
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_outgoing_ready(move || {
        if let Some(w) = weak.upgrade() {
            s.outgoing_ready(&w);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_cancel_outgoing(move || {
        if let Some(w) = weak.upgrade() {
            s.cancel_outgoing(&w);
        }
    });
    let weak = w.as_weak();
    let s = state.clone();
    w.on_send_outgoing(move || {
        if let Some(w) = weak.upgrade() {
            s.send_outgoing(&w);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn one_pixel_dib() -> Vec<u8> {
        let mut dib = vec![0; 44];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib[40..43].copy_from_slice(&[30, 20, 10]);
        dib
    }
    #[test]
    fn clipboard_header_limits_are_checked_before_decode() {
        assert!(decode_dib(&[]).is_err());
        let mut dib = vec![0; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&100_000i32.to_le_bytes());
        dib[8..12].copy_from_slice(&100_000i32.to_le_bytes());
        assert!(decode_dib(&dib).unwrap_err().contains("megapixel"));
    }
    #[test]
    fn clipboard_dib_decodes_real_bgr_pixels() {
        let image = decode_dib(&one_pixel_dib()).unwrap();
        assert_eq!(image.to_rgb8().as_raw(), &[10, 20, 30]);
    }
    #[test]
    fn pasted_images_become_temporary_png_files_removed_on_drop() {
        fn read(reader: &mut dyn FnMut(&[u8])) -> Result<(), &'static str> {
            reader(&one_pixel_dib());
            Ok(())
        }
        let sources = AttachmentSources {
            pick: |_| Ok(None),
            has_image: || true,
            read_image: read,
        };
        let prepared = clipboard_file(sources).unwrap();
        assert!(prepared.photo && prepared.temporary && prepared.pixels.is_some());
        let path = prepared.path.clone();
        assert!(path.exists());
        drop(prepared);
        assert!(!path.exists());
    }
    #[test]
    fn clipboard_errors_are_reported_without_files() {
        let sources = AttachmentSources {
            pick: |_| Ok(None),
            has_image: || false,
            read_image: |_| Err("The clipboard is busy. Try pasting again."),
        };
        assert!(clipboard_file(sources).err().unwrap().contains("busy"));
    }
    #[test]
    fn documents_are_prepared_without_reading_file_contents() {
        // A sparse 2 GiB document must not be loaded into memory for its preview.
        let path =
            std::env::temp_dir().join(format!("veygram-document-{}.bin", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(2 * 1024 * 1024 * 1024).unwrap();
        drop(file);
        let prepared = prepare(path.clone(), true).unwrap();
        assert!(!prepared.photo);
        assert!(prepared.pixels.is_none());
        assert_eq!(prepared.size, 2 * 1024 * 1024 * 1024);
        drop(prepared);
        assert!(!path.exists());
    }
    #[test]
    fn photo_preview_is_small_and_original_is_preserved_on_cancel() {
        let path = std::env::temp_dir().join(format!("veygram-photo-{}.png", std::process::id()));
        image::RgbaImage::new(800, 600).save(&path).unwrap();
        let prepared = prepare(path.clone(), false).unwrap();
        assert!(prepared.photo);
        let pixels = prepared.pixels.as_ref().unwrap();
        assert!(pixels.width() <= 320 && pixels.height() <= 200);
        drop(prepared);
        assert!(path.exists());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn verbatim_drive_paths_are_simplified_for_tdlib() {
        assert_eq!(
            plain_path(r"\\?\C:\Photos\a.png".into()),
            PathBuf::from(r"C:\Photos\a.png")
        );
        for kept in [
            r"\\?\UNC\server\share\a.png".to_string(),
            format!(r"\\?\C:\{}", "a".repeat(300)),
        ] {
            assert_eq!(plain_path(kept.clone().into()), PathBuf::from(kept));
        }
        let path = std::env::temp_dir().join(format!("veygram-plain-{}.txt", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let prepared = prepare(path, true).unwrap();
        assert!(!prepared.path.to_string_lossy().starts_with(r"\\?\"));
    }
    #[test]
    fn missing_files_and_directories_are_rejected() {
        assert!(prepare(std::env::temp_dir(), false).is_err());
        assert!(prepare(PathBuf::from("does-not-exist.zip"), false).is_err());
    }
    #[test]
    fn unsupported_or_oversized_images_remain_sendable_as_documents() {
        let path = std::env::temp_dir().join(format!("veygram-large-{}.png", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(33 * 1024 * 1024).unwrap();
        drop(file);
        let prepared = prepare(path.clone(), true).unwrap();
        assert!(!prepared.photo);
        assert!(prepared.pixels.is_none());
        drop(prepared);
        assert!(!path.exists());
    }
    #[test]
    fn original_photo_can_be_sent_when_the_preview_exceeds_its_budget() {
        let path =
            std::env::temp_dir().join(format!("veygram-original-{}.png", std::process::id()));
        image::RgbaImage::new(1, 1).save(&path).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(33 * 1024 * 1024)
            .unwrap();
        let prepared = prepare(path.clone(), true).unwrap();
        assert!(prepared.photo);
        assert!(prepared.pixels.is_none());
        drop(prepared);
        assert!(!path.exists());
    }
}
