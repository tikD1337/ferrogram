//! Custom message text rendering: layout through parley, rasterization through swash.
//!
//! Tests what Slint's `Text` and `StyledText` elements cannot do: color emoji with
//! the software renderer, inline images, spoilers, text selection and accurate
//! link hit-testing. The result is a pixel buffer that Slint displays as an image.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use parley::{
    Affinity, Alignment, AlignmentOptions, Cursor, FontContext, FontFamily, FontStyle, FontWeight,
    GlyphRun, InlineBox, InlineBoxKind, Layout, LayoutContext, PositionedLayoutItem, Selection,
    StyleProperty,
};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Scaler, Source, StrikeWith};
use swash::zeno::{Format, Vector};

/// Non-premultiplied color: red, green, blue, opacity.
pub type Rgba = [u8; 4];

const TEXT_COLOR: Rgba = [255, 255, 255, 255];
const LINK_COLOR: Rgba = [106, 179, 243, 255];
const CODE_BACKGROUND: Rgba = [255, 255, 255, 28];
const SELECTION_COLOR: Rgba = [46, 112, 165, 255];
const SPOILER_COLOR: Rgba = [120, 134, 148, 255];

const FONT_SIZE: f32 = 14.0;
const EMOJI_BOX: f32 = 20.0;
/// Number of horizontal subpixel glyph positions; Slint itself uses the same value.
const SUBPIXEL_BINS: f32 = 4.0;
/// Glyph cache limit. Exceeding it clears the entire cache — sufficient for the prototype.
const GLYPH_CACHE_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    Bold,
    Italic,
    Code,
    Strike,
    /// Link index in [`RichText::links`].
    Link(usize),
    Spoiler,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: SpanKind,
}

/// Message text with markup — equivalent to a set of Telegram entities.
#[derive(Clone, Debug, Default)]
pub struct RichText {
    pub text: String,
    pub spans: Vec<Span>,
    /// Byte positions where emoji images are inserted.
    pub emoji_boxes: Vec<usize>,
    pub links: Vec<String>,
}

/// Rendered text image: RGBA pixels with premultiplied alpha.
pub struct Rendered {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Two points in text image coordinates: where the selection started and where it is now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionPoints {
    pub anchor: (f32, f32),
    pub focus: (f32, f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font_blob: u64,
    font_index: u32,
    size_bits: u32,
    coords_hash: u64,
    glyph: u16,
    bin: u8,
}

struct CachedGlyph {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    is_color: bool,
    data: Vec<u8>,
}

pub struct TextEngine {
    font_cx: FontContext,
    layout_cx: LayoutContext<Rgba>,
    scale_cx: ScaleContext,
    /// Font offset and key for swash: without them, its internal cache would miss on every call.
    fonts: HashMap<(u64, u32), Option<(u32, swash::CacheKey)>>,
    glyphs: HashMap<GlyphKey, Option<CachedGlyph>>,
    glyph_bytes: usize,
    pub layouts_built: u32,
    pub glyphs_rasterized: u32,
}

impl TextEngine {
    pub fn new() -> Self {
        Self {
            font_cx: FontContext::new(),
            layout_cx: LayoutContext::new(),
            scale_cx: ScaleContext::new(),
            fonts: HashMap::new(),
            glyphs: HashMap::new(),
            glyph_bytes: 0,
            layouts_built: 0,
            glyphs_rasterized: 0,
        }
    }

    fn layout(&mut self, rich: &RichText, scale: f32, max_width: f32) -> Layout<Rgba> {
        self.layouts_built += 1;
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, &rich.text, scale, true);
        builder.push_default(StyleProperty::FontSize(FONT_SIZE));
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
            Cow::Borrowed("Segoe UI, sans-serif"),
        )));
        builder.push_default(StyleProperty::Brush(TEXT_COLOR));

        for span in &rich.spans {
            let range = span.range.clone();
            match span.kind {
                SpanKind::Bold => builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range),
                SpanKind::Italic => {
                    builder.push(StyleProperty::FontStyle(FontStyle::Italic), range)
                }
                SpanKind::Code => builder.push(
                    StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
                        "Consolas, monospace",
                    ))),
                    range,
                ),
                SpanKind::Strike => builder.push(StyleProperty::Strikethrough(true), range),
                SpanKind::Link(_) => {
                    builder.push(StyleProperty::Brush(LINK_COLOR), range.clone());
                    builder.push(StyleProperty::Underline(true), range);
                }
                SpanKind::Spoiler => {}
            }
        }

        for (id, &index) in rich.emoji_boxes.iter().enumerate() {
            builder.push_inline_box(InlineBox {
                id: id as u64,
                kind: InlineBoxKind::InFlow,
                index,
                width: EMOJI_BOX * scale,
                height: EMOJI_BOX * scale,
            });
        }

        let mut layout: Layout<Rgba> = builder.build(&rich.text);
        layout.break_all_lines(Some(max_width));
        if layout.is_rtl() {
            // parley aligns lines relative to the width supplied for line breaking. Without this step,
            // a right-to-left paragraph is pushed to the right edge of the entire available area
            // and extends beyond the image sized to fit the text. Breaking lines again at
            // the actual width preserves the same lines but aligns them within it.
            // Use the full width, including trailing spaces: otherwise the longest line will not fit.
            layout.break_all_lines(Some(content_width(&layout)));
        }
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    /// Renders text. `max_width` is in physical pixels.
    pub fn render(
        &mut self,
        rich: &RichText,
        scale: f32,
        max_width: f32,
        selection: Option<SelectionPoints>,
    ) -> Rendered {
        let layout = self.layout(rich, scale, max_width);
        let width = (content_width(&layout) as u32).max(1);
        let height = (layout.height().ceil() as u32).max(1);
        let mut canvas = Canvas::new(width, height);

        // Backgrounds: code background and selection — beneath the text.
        for span in rich.spans.iter().filter(|span| span.kind == SpanKind::Code) {
            for rect in range_rects(&layout, &span.range) {
                canvas.fill_rect(rect, CODE_BACKGROUND);
            }
        }
        if let Some(points) = selection {
            for (rect, _line) in selection_at(&layout, points).geometry(&layout) {
                canvas.fill_rect(rect.into(), SELECTION_COLOR);
            }
        }

        for line in layout.lines() {
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(run) => self.draw_run(&mut canvas, &run),
                    PositionedLayoutItem::InlineBox(inline_box) => canvas.draw_custom_emoji(
                        inline_box.x,
                        inline_box.y,
                        inline_box.width.min(inline_box.height),
                        inline_box.id,
                    ),
                }
            }
        }

        // Spoilers cover the text from above.
        for span in rich
            .spans
            .iter()
            .filter(|span| span.kind == SpanKind::Spoiler)
        {
            for rect in range_rects(&layout, &span.range) {
                canvas.fill_rect(rect, SPOILER_COLOR);
            }
        }

        Rendered {
            pixels: canvas.pixels,
            width,
            height,
        }
    }

    /// Text included in the selection.
    pub fn selected_text(
        &mut self,
        rich: &RichText,
        scale: f32,
        max_width: f32,
        points: SelectionPoints,
    ) -> String {
        let layout = self.layout(rich, scale, max_width);
        let range = selection_at(&layout, points).text_range();
        rich.text.get(range).unwrap_or_default().to_owned()
    }

    /// Link at a point, if the point actually hits its text.
    pub fn link_at(
        &mut self,
        rich: &RichText,
        scale: f32,
        max_width: f32,
        point: (f32, f32),
    ) -> Option<usize> {
        let layout = self.layout(rich, scale, max_width);
        rich.spans.iter().find_map(|span| match span.kind {
            SpanKind::Link(link) => range_rects(&layout, &span.range)
                .iter()
                .any(|rect| rect.contains(point))
                .then_some(link),
            _ => None,
        })
    }

    /// Prints layout geometry — for investigating defects.
    pub fn debug_layout(&mut self, rich: &RichText, scale: f32, max_width: f32) {
        let layout = self.layout(rich, scale, max_width);
        eprintln!(
            "text={:?} rtl={} width={} full_width={} height={}",
            rich.text,
            layout.is_rtl(),
            layout.width(),
            layout.full_width(),
            layout.height()
        );
        for (index, line) in layout.lines().enumerate() {
            let metrics = line.metrics();
            eprintln!(
                "  line {index}: offset={} advance={} trailing_ws={} inline_min={} inline_max={}",
                metrics.offset,
                metrics.advance,
                metrics.trailing_whitespace,
                metrics.inline_min_coord,
                metrics.inline_max_coord
            );
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(run) => eprintln!(
                        "    run: offset={} advance={} rtl={} range={:?}",
                        run.offset(),
                        run.advance(),
                        run.run().is_rtl(),
                        run.run().text_range()
                    ),
                    PositionedLayoutItem::InlineBox(inline_box) => {
                        eprintln!("    box: x={} width={}", inline_box.x, inline_box.width);
                    }
                }
            }
        }
    }

    /// Center of the first link in the text — for the hit-test self-check.
    pub fn link_center(
        &mut self,
        rich: &RichText,
        scale: f32,
        max_width: f32,
    ) -> Option<(f32, f32)> {
        let layout = self.layout(rich, scale, max_width);
        let span = rich
            .spans
            .iter()
            .find(|span| matches!(span.kind, SpanKind::Link(_)))?;
        let rect = *range_rects(&layout, &span.range).first()?;
        Some(((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0))
    }

    fn draw_run(&mut self, canvas: &mut Canvas, glyph_run: &GlyphRun<'_, Rgba>) {
        let style = glyph_run.style();
        let run = glyph_run.run();
        let font = run.font();
        let font_size = run.font_size();
        let coords = run.normalized_coords();
        let blob_id = font.data.id();

        let font_ids = *self.fonts.entry((blob_id, font.index)).or_insert_with(|| {
            swash::FontRef::from_index(font.data.data(), font.index as usize)
                .map(|font_ref| (font_ref.offset, font_ref.key))
        });
        let Some((offset, key)) = font_ids else {
            return;
        };
        let font_ref = swash::FontRef {
            data: font.data.data(),
            offset,
            key,
        };

        let coords_hash = coords.iter().fold(0_u64, |hash, &coord| {
            hash.wrapping_mul(0x0100_0000_01B3) ^ u64::from(coord as u16)
        });

        if self.glyph_bytes > GLYPH_CACHE_LIMIT {
            self.glyphs.clear();
            self.glyph_bytes = 0;
        }

        let placed: Vec<(GlyphKey, i32, i32)> = glyph_run
            .positioned_glyphs()
            .map(|glyph| {
                let x_floor = glyph.x.floor();
                let key = GlyphKey {
                    font_blob: blob_id,
                    font_index: font.index,
                    size_bits: font_size.to_bits(),
                    coords_hash,
                    glyph: glyph.id as u16,
                    bin: ((glyph.x - x_floor) * SUBPIXEL_BINS) as u8,
                };
                (key, x_floor as i32, glyph.y.round() as i32)
            })
            .collect();

        // The scaler is needed only on a cache miss: it is not created in steady state.
        let any_missing = placed
            .iter()
            .any(|(key, _, _)| !self.glyphs.contains_key(key));
        let mut scaler: Option<Scaler<'_>> = any_missing.then(|| {
            self.scale_cx
                .builder(font_ref)
                .size(font_size)
                .normalized_coords(coords)
                .build()
        });

        for (key, x, y) in placed {
            if !self.glyphs.contains_key(&key)
                && let Some(scaler) = scaler.as_mut()
            {
                let rasterized = rasterize(scaler, key.glyph, f32::from(key.bin) / SUBPIXEL_BINS);
                self.glyphs_rasterized += 1;
                self.glyph_bytes += rasterized.as_ref().map_or(0, |glyph| glyph.data.len());
                self.glyphs.insert(key, rasterized);
            }

            if let Some(Some(cached)) = self.glyphs.get(&key) {
                let left = x + cached.left;
                let top = y - cached.top;
                if cached.is_color {
                    canvas.blit_color(left, top, cached.width, cached.height, &cached.data);
                } else {
                    canvas.blit_mask(
                        left,
                        top,
                        cached.width,
                        cached.height,
                        &cached.data,
                        style.brush,
                    );
                }
            }
        }

        let metrics = run.metrics();
        let x0 = glyph_run.offset();
        let x1 = x0 + glyph_run.advance();
        if let Some(underline) = &style.underline {
            let top = glyph_run.baseline() - underline.offset.unwrap_or(metrics.underline_offset);
            let size = underline.size.unwrap_or(metrics.underline_size).max(1.0);
            canvas.fill_rect(
                Rect {
                    x0,
                    y0: top,
                    x1,
                    y1: top + size,
                },
                underline.brush,
            );
        }
        if let Some(strike) = &style.strikethrough {
            let top = glyph_run.baseline() - strike.offset.unwrap_or(metrics.strikethrough_offset);
            let size = strike.size.unwrap_or(metrics.strikethrough_size).max(1.0);
            canvas.fill_rect(
                Rect {
                    x0,
                    y0: top,
                    x1,
                    y1: top + size,
                },
                strike.brush,
            );
        }
    }
}

fn rasterize(scaler: &mut Scaler<'_>, glyph: u16, subpixel_x: f32) -> Option<CachedGlyph> {
    // Source order: color outlines and bitmaps (emoji) first, then the regular outline.
    let image = Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Outline,
    ])
    .format(Format::Alpha)
    .offset(Vector::new(subpixel_x, 0.0))
    .render(scaler, glyph)?;

    Some(CachedGlyph {
        left: image.placement.left,
        top: image.placement.top,
        width: image.placement.width,
        height: image.placement.height,
        is_color: image.content == Content::Color,
        data: image.data,
    })
}

/// Width of the image for the text, in whole pixels.
fn content_width(layout: &Layout<Rgba>) -> f32 {
    if layout.is_rtl() {
        layout.full_width().ceil()
    } else {
        layout.width().ceil()
    }
}

fn selection_at(layout: &Layout<Rgba>, points: SelectionPoints) -> Selection {
    Selection::from_point(layout, points.anchor.0, points.anchor.1).extend_to_point(
        layout,
        points.focus.0,
        points.focus.1,
    )
}

/// Rectangles covering a text range: one per line.
fn range_rects(layout: &Layout<Rgba>, range: &Range<usize>) -> Vec<Rect> {
    let start = Cursor::from_byte_index(layout, range.start, Affinity::Downstream);
    let end = Cursor::from_byte_index(layout, range.end, Affinity::Upstream);
    Selection::new(start, end)
        .geometry(layout)
        .into_iter()
        .map(|(rect, _line)| rect.into())
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn contains(&self, (x, y): (f32, f32)) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

impl From<parley::BoundingBox> for Rect {
    fn from(rect: parley::BoundingBox) -> Self {
        Self {
            x0: rect.x0 as f32,
            y0: rect.y0 as f32,
            x1: rect.x1 as f32,
            y1: rect.y1 as f32,
        }
    }
}

/// RGBA canvas with premultiplied alpha — the buffer format accepted by Slint.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Composites a non-premultiplied color with additional `coverage` (0–255).
    fn blend(&mut self, x: i32, y: i32, color: Rgba, coverage: u32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let alpha = u32::from(color[3]) * coverage / 255;
        if alpha == 0 {
            return;
        }
        let index = (y as usize * self.width as usize + x as usize) * 4;
        let inverse = 255 - alpha;
        for channel in 0..3 {
            let source = u32::from(color[channel]) * alpha / 255;
            let below = u32::from(self.pixels[index + channel]) * inverse / 255;
            self.pixels[index + channel] = (source + below).min(255) as u8;
        }
        let below = u32::from(self.pixels[index + 3]) * inverse / 255;
        self.pixels[index + 3] = (alpha + below).min(255) as u8;
    }

    fn fill_rect(&mut self, rect: Rect, color: Rgba) {
        let (x0, x1) = (rect.x0.round() as i32, rect.x1.round() as i32);
        let (y0, y1) = (rect.y0.round() as i32, rect.y1.round() as i32);
        for y in y0..y1.max(y0 + 1) {
            for x in x0..x1 {
                self.blend(x, y, color, 255);
            }
        }
    }

    fn blit_mask(
        &mut self,
        left: i32,
        top: i32,
        width: u32,
        height: u32,
        mask: &[u8],
        color: Rgba,
    ) {
        for row in 0..height {
            for column in 0..width {
                let coverage = mask[(row * width + column) as usize];
                if coverage != 0 {
                    self.blend(
                        left + column as i32,
                        top + row as i32,
                        color,
                        u32::from(coverage),
                    );
                }
            }
        }
    }

    fn blit_color(&mut self, left: i32, top: i32, width: u32, height: u32, rgba: &[u8]) {
        for row in 0..height {
            for column in 0..width {
                let index = ((row * width + column) * 4) as usize;
                let color = [
                    rgba[index],
                    rgba[index + 1],
                    rgba[index + 2],
                    rgba[index + 3],
                ];
                self.blend(left + column as i32, top + row as i32, color, 255);
            }
        }
    }

    /// Custom emoji placeholder: an image absent from all fonts.
    /// In the client, this will be a sticker frame.
    fn draw_custom_emoji(&mut self, x: f32, y: f32, size: f32, id: u64) {
        const PALETTE: [(Rgba, Rgba); 3] = [
            ([255, 214, 64, 255], [255, 140, 0, 255]),
            ([120, 220, 255, 255], [40, 110, 230, 255]),
            ([190, 255, 150, 255], [30, 160, 80, 255]),
        ];
        let (inner, outer) = PALETTE[(id % PALETTE.len() as u64) as usize];
        let radius = size / 2.0 - 1.0;
        let (center_x, center_y) = (x + size / 2.0, y + size / 2.0);
        let eye = radius * 0.16;

        for py in y.floor() as i32..(y + size).ceil() as i32 {
            for px in x.floor() as i32..(x + size).ceil() as i32 {
                let (dx, dy) = (px as f32 + 0.5 - center_x, py as f32 + 0.5 - center_y);
                let distance = (dx * dx + dy * dy).sqrt();
                let coverage = ((radius - distance + 0.5).clamp(0.0, 1.0) * 255.0) as u32;
                if coverage == 0 {
                    continue;
                }
                let mix = (distance / radius).clamp(0.0, 1.0);
                let mut color = [0, 0, 0, 255];
                for channel in 0..3 {
                    color[channel] = (f32::from(inner[channel]) * (1.0 - mix)
                        + f32::from(outer[channel]) * mix)
                        as u8;
                }
                let in_eye = |offset: f32| {
                    let (ex, ey) = (dx - offset * radius, dy + radius * 0.2);
                    (ex * ex + ey * ey).sqrt() < eye
                };
                let in_smile = dy > radius * 0.15
                    && (distance - radius * 0.55).abs() < radius * 0.09
                    && dx.abs() < radius * 0.5;
                if in_eye(-0.35) || in_eye(0.35) || in_smile {
                    color = [60, 40, 20, 255];
                }
                self.blend(px, py, color, coverage);
            }
        }
    }
}
