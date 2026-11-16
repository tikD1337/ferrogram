//! Visible-row text images: Parley shaping and Swash color/mask rasterization.
//! No rendered-image cache: the feed owns images only for its visible Flickable rows.
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, GlyphRun, Layout,
    LayoutContext, PositionedLayoutItem, StyleProperty,
};
use std::{borrow::Cow, collections::HashMap, ops::Range};
use swash::{
    scale::{Render, ScaleContext, Source, StrikeWith, image::Content},
    zeno::{Format, Vector},
};
use tg_core::{EntityKind, Message};
type Color = [u8; 4];
const WHITE: Color = [255, 255, 255, 255];
const LINK: Color = [106, 179, 243, 255];
const CACHE_BYTES: usize = 2 * 1024 * 1024;
// Includes negative entries and metadata, so missing glyphs cannot grow the cache forever.
const CACHE_ENTRIES: usize = 8192;
const ENTRY_COST: usize = 128;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn byte_range(text: &str, offset: u32, length: u32) -> Option<Range<usize>> {
    let end = offset.checked_add(length)?;
    let mut units = 0;
    let mut start = None;
    for (byte, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), '\0')))
    {
        if units == offset {
            start = Some(byte);
        }
        if units == end {
            return start.map(|start| start..byte);
        }
        units += ch.len_utf16() as u32;
    }
    None
}

#[derive(Hash, PartialEq, Eq)]
struct GlyphKey {
    font: u64,
    index: u32,
    size: u32,
    coords: u64,
    id: u16,
    bin: u8,
}
struct Glyph {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    color: bool,
    bytes: Vec<u8>,
}
pub(crate) struct Rendered {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub clipped: bool,
    /// Advance of the last line in pixels; the full width for RTL text.
    pub last_line: u32,
}
// Parley 0.12 adds a root-font strut and rounds normal line heights. Preserve the
// measured 0.11 message geometry: actual run metrics, fractional line accumulation,
// and pixel-aligned baselines. Shaping, wrapping and bidi remain entirely Parley's.
struct LinePlacement {
    shift: f32,
    top: f32,
    bottom: f32,
    content_top: f32,
}
fn message_lines(layout: &Layout<Color>, text: &str) -> (Vec<LinePlacement>, f32) {
    // At most one line per input byte plus an empty trailing line (engine limit: 64 KiB).
    let mut lines = Vec::new();
    let mut y = 0.0_f64;
    for line in layout.lines().take(65537) {
        let range = line.text_range();
        let non_space_end =
            range.start + text.get(range.clone()).unwrap_or_default().trim_end().len();
        let (mut ascent, mut descent, mut height) = (0.0_f32, 0.0_f32, 0.0_f32);
        for run in line.runs() {
            let metrics = run.font_metrics();
            height = height.max(metrics.ascent + metrics.descent + metrics.leading);
            if run.text_range().start < non_space_end || non_space_end == range.start {
                ascent = ascent.max(metrics.ascent);
                descent = descent.max(metrics.descent);
            }
        }
        let (a, d) = (ascent.round(), descent.round());
        let leading = height - a - d;
        let above = (leading * 0.5).floor();
        let baseline = y.round() as f32 + a + above;
        lines.push(LinePlacement {
            shift: baseline - line.metrics().baseline,
            top: baseline - a - above.max(0.0),
            bottom: baseline + d + (leading.round() - above).max(0.0),
            content_top: baseline - ascent,
        });
        y += f64::from(height);
    }
    (lines, y as f32)
}

pub(crate) struct TextEngine {
    fonts: FontContext,
    layouts: LayoutContext<Color>,
    scaler: ScaleContext,
    // Bounded by CACHE_BYTES including a conservative per-entry bookkeeping charge.
    glyphs: HashMap<GlyphKey, Option<Glyph>>,
    bytes: usize,
    // At most 128 font identities (~8 KiB); stable keys let Swash reuse its 8 font entries.
    font_keys: HashMap<(u64, u32), (u32, swash::CacheKey)>,
    emoji_family: String,
}
impl TextEngine {
    pub fn new(font: Option<Vec<u8>>) -> Self {
        let mut fonts = FontContext::new();
        let mut emoji_family = "Segoe UI Emoji".to_owned();
        if let Some(data) = font {
            let families = fonts
                .collection
                .register_fonts(parley::fontique::Blob::new(std::sync::Arc::new(data)), None);
            if let Some((id, _)) = families.first()
                && let Some(name) = fonts.collection.family_name(*id)
            {
                emoji_family = name.to_owned();
            }
        }
        Self {
            fonts,
            layouts: LayoutContext::new(),
            scaler: ScaleContext::new(),
            glyphs: HashMap::new(),
            bytes: 0,
            font_keys: HashMap::new(),
            emoji_family,
        }
    }
    pub fn render(&mut self, message: &Message, scale: f32, width: f32) -> Rendered {
        let family = format!("Segoe UI, {}, sans-serif", self.emoji_family);
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, &message.text, scale, true);
        builder.push_default(StyleProperty::FontSize(14.0));
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed(&family),
        )));
        builder.push_default(StyleProperty::Brush(WHITE));
        for entity in &message.entities {
            let Some(range) = byte_range(&message.text, entity.offset, entity.length) else {
                continue;
            };
            match entity.kind {
                EntityKind::Bold => {
                    builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range)
                }
                EntityKind::Italic => {
                    builder.push(StyleProperty::FontStyle(FontStyle::Italic), range)
                }
                EntityKind::Underline => builder.push(StyleProperty::Underline(true), range),
                EntityKind::Strikethrough => {
                    builder.push(StyleProperty::Strikethrough(true), range)
                }
                EntityKind::Code | EntityKind::Pre => builder.push(
                    StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
                        "Consolas, monospace",
                    ))),
                    range,
                ),
                EntityKind::Url | EntityKind::TextUrl { .. } | EntityKind::Mention => {
                    builder.push(StyleProperty::Brush(LINK), range.clone());
                    builder.push(StyleProperty::Underline(true), range);
                }
                EntityKind::Spoiler => {}
            }
        }
        let mut layout: Layout<Color> = builder.build(&message.text);
        layout.break_all_lines(Some(width));
        // As in the measured spike, RTL must align within the image's actual full width.
        if layout.is_rtl() {
            layout.break_all_lines(Some(layout.full_width().ceil()));
        }
        layout.align(Alignment::Start, AlignmentOptions::default());
        let image_width = if layout.is_rtl() {
            layout.full_width()
        } else {
            layout.width()
        }
        .ceil()
        .clamp(1.0, width) as u32;
        let (placements, legacy_height) = message_lines(&layout, &message.text);
        let full_height = legacy_height.ceil().max(1.0) as u32;
        let height = full_height.min((MAX_IMAGE_BYTES / (image_width as usize * 4)) as u32);
        let mut canvas = Canvas {
            width: image_width,
            height,
            pixels: vec![0; image_width as usize * height as usize * 4],
        };
        // Code backgrounds are composited beneath glyphs, unlike opaque spoiler covers.
        for entity in message
            .entities
            .iter()
            .filter(|e| matches!(e.kind, EntityKind::Code | EntityKind::Pre))
        {
            let Some(range) = byte_range(&message.text, entity.offset, entity.length) else {
                continue;
            };
            use parley::{Affinity, Cursor, Selection};
            let a = Cursor::from_byte_index(&layout, range.start, Affinity::Downstream);
            let b = Cursor::from_byte_index(&layout, range.end, Affinity::Upstream);
            for (rect, line) in Selection::new(a, b).geometry(&layout) {
                let Some(placement) = placements.get(line) else {
                    continue;
                };
                canvas.rect(
                    rect.x0 as f32,
                    placement.top,
                    rect.x1 as f32,
                    placement.bottom,
                    [255, 255, 255, 28],
                );
            }
        }
        for (line, placement) in layout.lines().zip(&placements) {
            if placement.content_top > height as f32 {
                break;
            }
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    self.draw(&mut canvas, &run, placement.shift);
                }
            }
        }
        for entity in message
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Spoiler)
        {
            let Some(range) = byte_range(&message.text, entity.offset, entity.length) else {
                continue;
            };
            use parley::{Affinity, Cursor, Selection};
            let a = Cursor::from_byte_index(&layout, range.start, Affinity::Downstream);
            let b = Cursor::from_byte_index(&layout, range.end, Affinity::Upstream);
            for (rect, line) in Selection::new(a, b).geometry(&layout) {
                let Some(placement) = placements.get(line) else {
                    continue;
                };
                canvas.rect(
                    rect.x0 as f32,
                    placement.top,
                    rect.x1 as f32,
                    placement.bottom,
                    [120, 134, 148, 255],
                );
            }
        }
        let last_line = if layout.is_rtl() {
            image_width
        } else {
            layout
                .lines()
                .last()
                .map_or(0.0, |line| line.metrics().advance)
                .ceil() as u32
        };
        Rendered {
            pixels: canvas.pixels,
            width: image_width,
            height,
            clipped: height < full_height,
            last_line,
        }
    }
    fn draw(&mut self, canvas: &mut Canvas, run: &GlyphRun<'_, Color>, shift: f32) {
        let font = run.run().font();
        if self.font_keys.len() >= 128 {
            self.font_keys.clear();
        }
        let identity = (font.data.id(), font.index);
        let (offset, key) = if let Some(cached) = self.font_keys.get(&identity) {
            *cached
        } else {
            let Some(reference) = swash::FontRef::from_index(font.data.data(), font.index as usize)
            else {
                return;
            };
            let value = (reference.offset, reference.key);
            self.font_keys.insert(identity, value);
            value
        };
        let font_ref = swash::FontRef {
            data: font.data.data(),
            offset,
            key,
        };
        let coords = run.run().normalized_coords();
        let coords_hash = coords.iter().fold(0u64, |hash, x| {
            hash.wrapping_mul(16777619) ^ (x.to_bits() as u16 as u64)
        });
        for placed in run.positioned_glyphs() {
            let floor = placed.x.floor();
            let bin = ((placed.x - floor) * 4.0) as u8;
            let key = GlyphKey {
                font: font.data.id(),
                index: font.index,
                size: run.run().font_size().to_bits(),
                coords: coords_hash,
                id: placed.id as u16,
                bin,
            };
            if !self.glyphs.contains_key(&key) {
                let mut scaler = self
                    .scaler
                    .builder(font_ref)
                    .size(run.run().font_size())
                    .normalized_coords(coords.iter().map(|coord| coord.to_bits()))
                    .build();
                let image = Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                ])
                .format(Format::Alpha)
                .offset(Vector::new(f32::from(bin) / 4.0, 0.0))
                .render(&mut scaler, key.id);
                let glyph = image.map(|image| Glyph {
                    left: image.placement.left,
                    top: image.placement.top,
                    width: image.placement.width,
                    height: image.placement.height,
                    color: image.content == Content::Color,
                    bytes: image.data,
                });
                let cost = ENTRY_COST + glyph.as_ref().map_or(0, |g| g.bytes.len());
                if self.bytes + cost > CACHE_BYTES || self.glyphs.len() == CACHE_ENTRIES {
                    self.glyphs.clear();
                    self.bytes = 0;
                }
                if cost <= CACHE_BYTES {
                    self.bytes += cost;
                    self.glyphs.insert(key, glyph);
                }
            }
            if let Some(Some(glyph)) = self.glyphs.get(&GlyphKey {
                font: font.data.id(),
                index: font.index,
                size: run.run().font_size().to_bits(),
                coords: coords_hash,
                id: placed.id as u16,
                bin,
            }) {
                for y in 0..glyph.height {
                    for x in 0..glyph.width {
                        let i = (y * glyph.width + x) as usize;
                        let (color, coverage) = if glyph.color {
                            (
                                glyph.bytes[i * 4..i * 4 + 4].try_into().unwrap_or(WHITE),
                                255,
                            )
                        } else {
                            (run.style().brush, u32::from(glyph.bytes[i]))
                        };
                        canvas.blend(
                            floor as i32 + glyph.left + x as i32,
                            (placed.y + shift).round() as i32 - glyph.top + y as i32,
                            color,
                            coverage,
                        );
                    }
                }
            }
        }
        let metrics = run.run().font_metrics();
        if let Some(line) = &run.style().underline {
            let y = run.baseline() + shift - line.offset.unwrap_or(metrics.underline_offset);
            canvas.rect(
                run.offset(),
                y,
                run.offset() + run.advance(),
                y + line.size.unwrap_or(metrics.underline_size).max(1.0),
                line.brush,
            );
        }
        if let Some(line) = &run.style().strikethrough {
            let y = run.baseline() + shift - line.offset.unwrap_or(metrics.strikethrough_offset);
            canvas.rect(
                run.offset(),
                y,
                run.offset() + run.advance(),
                y + line.size.unwrap_or(metrics.strikethrough_size).max(1.0),
                line.brush,
            );
        }
    }
}
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}
impl Canvas {
    fn blend(&mut self, x: i32, y: i32, color: Color, coverage: u32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let alpha = u32::from(color[3]) * coverage / 255;
        let i = (y as usize * self.width as usize + x as usize) * 4;
        for (channel, value) in color.iter().take(3).enumerate() {
            self.pixels[i + channel] = (u32::from(*value) * alpha / 255
                + u32::from(self.pixels[i + channel]) * (255 - alpha) / 255)
                .min(255) as u8;
        }
        self.pixels[i + 3] =
            (alpha + u32::from(self.pixels[i + 3]) * (255 - alpha) / 255).min(255) as u8;
    }
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) {
        for y in (y0.round() as i32).max(0)..(y1.round() as i32).min(self.height as i32) {
            for x in (x0.round() as i32).max(0)..(x1.round() as i32).min(self.width as i32) {
                self.blend(x, y, color, 255);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multilingual_images_are_bounded_and_use_cached_glyphs() {
        let mut engine = TextEngine::new(None);
        let mut message = Message {
            reply: None,
            forwarded: None,
            edited: false,
            is_text: true,
            permissions: None,
            video: None,
            photo: None,
            id: tg_core::MessageId(1),
            chat: tg_core::ChatId(1),
            date: 0,
            outgoing: false,
            text: "Hello \u{041f}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442} \u{1f642}".into(),
            entities: vec![],
            send_state: tg_core::SendState::Sent,
            album: 0,
            sender_id: 0,
            sender: None,
        };
        let rendered = engine.render(&message, 1.0, 180.0);
        assert!(rendered.width <= 180 && rendered.height > 0);
        assert!(
            rendered
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
        let cached = engine.glyphs.len();
        let bytes = engine.bytes;
        engine.render(&message, 1.0, 180.0);
        assert_eq!(engine.glyphs.len(), cached);
        assert_eq!(engine.bytes, bytes);
        message.text =
            "\u{05e9}\u{05dc}\u{05d5}\u{05dd} \u{0645}\u{0631}\u{062d}\u{0628}\u{0627}".into();
        let rtl = engine.render(&message, 1.0, 180.0);
        assert!(
            rtl.width <= 180
                && rtl
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0)
        );
        assert!(engine.bytes <= CACHE_BYTES && engine.glyphs.len() <= CACHE_ENTRIES);
        assert!(engine.font_keys.len() <= 128);
    }
    #[test]
    fn utf16_ranges_reject_split_surrogates_and_overflow() {
        let text = "A\u{1f642}\u{05e9}\u{05dc}\u{05d5}\u{05dd}";
        assert_eq!(byte_range(text, 1, 2), Some(1..5));
        assert_eq!(byte_range(text, 3, 4), Some(5..13));
        assert_eq!(byte_range(text, 2, 1), None);
        assert_eq!(byte_range(text, 1, 1), None);
        assert_eq!(byte_range(text, u32::MAX, 2), None);
    }
}
