//! Slint message feed prototype for measuring memory, CPU and scrolling smoothness.
//!
//! Messages are not stored: the model generates a row by index when requested.
//! This measures the cost of the framework itself rather than our data, and also
//! tests the principle that “history does not live in memory”.
//!
//! Options:
//!   --count N          number of messages (default 10000)
//!   --mode MODE        plain — Text element; styled — StyledText from Markdown;
//!                      custom — custom text rendering (see text.rs)
//!   --theme THEME      dark (default) or oled: true black background for OLED screens
//!   --start-bottom     open the feed anchored to the bottom, as in a chat
//!   --autoscroll SEC   scroll the feed back and forth for SEC seconds
//!   --speed PX         autoscroll speed, logical pixels per second (default 3000)
//!   --demo-select      in custom mode: select the last message text and test link hit detection
//!   --debug-row N      print the layout geometry of message N and exit
//!   --snapshot PATH    save a PNG snapshot of the window 2 seconds after startup
//!   --exit-after SEC   exit after SEC seconds
//!
//! The renderer is selected through the SLINT_BACKEND environment variable:
//! winit-software, winit-femtovg, winit-skia (the last requires a `--features skia` build).

mod text;

use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{
    ComponentHandle, Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel,
    SharedPixelBuffer, SharedString, StyledText, Timer, TimerMode,
};

use text::{RichText, SelectionPoints, Span, SpanKind, TextEngine};

slint::include_modules!();

const WORDS: &[&str] = &[
    "привет",
    "как",
    "дела",
    "сегодня",
    "завтра",
    "посмотри",
    "пожалуйста",
    "этот",
    "файл",
    "сборка",
    "прошла",
    "успешно",
    "но",
    "есть",
    "вопрос",
    "по",
    "архитектуре",
    "клиента",
    "release",
    "build",
    "ok",
    "meeting",
    "at",
    "noon",
    "שלום",
    "مرحبا",
    "你好",
    "🙂",
    "🔥",
    "👍",
    "https://example.org/some/long/path?with=query",
    "производительность",
    "память",
    "процессор",
    "и",
    "в",
    "на",
    "не",
    "да",
    "нет",
];

const SENDERS: &[&str] = &["Алиса", "Борис", "Вера", "Глеб", "Дарья", "Егор"];

/// Maximum text width in a bubble, in logical pixels: 520 minus padding of 8 on each side.
const TEXT_MAX_WIDTH: f32 = 504.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Plain,
    Styled,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    None,
    Bold,
    Italic,
    Code,
    Strike,
    Spoiler,
    CustomEmoji,
}

struct Generated {
    tokens: Vec<(&'static str, Style)>,
    outgoing: bool,
    sender: &'static str,
}

/// SplitMix64: a deterministic generator so that the same row always produces the same text.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn generate(row: usize) -> Generated {
    let mut state = (row as u64).wrapping_mul(0xA24B_AED4_963E_E407) ^ 0x5DEE_CE66D;

    // Half the messages are short, a third are medium, and the rest are long — as in a real chat.
    let words = match next(&mut state) % 100 {
        0..50 => 1 + next(&mut state) % 8,
        50..85 => 8 + next(&mut state) % 22,
        _ => 30 + next(&mut state) % 90,
    };

    let tokens = (0..words)
        .map(|_| {
            let word = WORDS[(next(&mut state) % WORDS.len() as u64) as usize];
            let style = match next(&mut state) % 40 {
                0..5 => Style::Bold,
                5..9 => Style::Italic,
                9..12 => Style::Code,
                12..14 => Style::Strike,
                14..16 => Style::Spoiler,
                16..18 => Style::CustomEmoji,
                _ => Style::None,
            };
            (word, style)
        })
        .collect();

    Generated {
        tokens,
        outgoing: next(&mut state) % 3 == 0,
        sender: SENDERS[(next(&mut state) % SENDERS.len() as u64) as usize],
    }
}

fn is_link(word: &str) -> bool {
    word.starts_with("https://")
}

fn plain_text(tokens: &[(&str, Style)]) -> String {
    tokens
        .iter()
        .map(|(word, _)| *word)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Markup for `StyledText`. Slint Markdown does not support spoilers or inline images.
fn markdown(tokens: &[(&str, Style)]) -> String {
    let parts: Vec<String> = tokens
        .iter()
        .map(|&(word, style)| match style {
            _ if is_link(word) => format!("[ссылка]({word})"),
            Style::Bold => format!("**{word}**"),
            Style::Italic => format!("*{word}*"),
            Style::Code => format!("`{word}`"),
            Style::Strike => format!("~~{word}~~"),
            Style::None | Style::Spoiler | Style::CustomEmoji => word.to_owned(),
        })
        .collect();
    parts.join(" ")
}

/// Markup for custom rendering: ranges, as with Telegram entities.
fn rich_text(tokens: &[(&str, Style)]) -> RichText {
    let mut rich = RichText::default();
    for (index, &(word, style)) in tokens.iter().enumerate() {
        if index > 0 {
            rich.text.push(' ');
        }
        let start = rich.text.len();

        if is_link(word) {
            rich.text.push_str("ссылка");
            rich.links.push(word.to_owned());
            rich.spans.push(Span {
                range: start..rich.text.len(),
                kind: SpanKind::Link(rich.links.len() - 1),
            });
            continue;
        }

        if style == Style::CustomEmoji {
            rich.emoji_boxes.push(start);
        }
        rich.text.push_str(word);

        let kind = match style {
            Style::Bold => Some(SpanKind::Bold),
            Style::Italic => Some(SpanKind::Italic),
            Style::Code => Some(SpanKind::Code),
            Style::Strike => Some(SpanKind::Strike),
            Style::Spoiler => Some(SpanKind::Spoiler),
            Style::None | Style::CustomEmoji => None,
        };
        if let Some(kind) = kind {
            rich.spans.push(Span {
                range: start..rich.text.len(),
                kind,
            });
        }
    }
    rich
}

/// A model without storage: a row is created by index when requested.
///
/// The model does not cache rendered text images: Slint requests a row once when
/// a message appears and keeps the image itself while it is visible. Measurement showed 0 cache hits
/// across 891 renders and 6 MB of extra memory — exactly the cache limit.
struct FeedModel {
    count: usize,
    mode: Mode,
    /// The number of physical pixels per logical pixel.
    scale: f32,
    engine: RefCell<TextEngine>,
    selection: Cell<Option<(usize, SelectionPoints)>>,
    notify: ModelNotify,
    renders: Cell<u32>,
}

impl FeedModel {
    fn new(count: usize, mode: Mode, scale: f32) -> Self {
        Self {
            count,
            mode,
            scale,
            engine: RefCell::new(TextEngine::new()),
            selection: Cell::new(None),
            notify: ModelNotify::default(),
            renders: Cell::new(0),
        }
    }

    fn max_width(&self) -> f32 {
        TEXT_MAX_WIDTH * self.scale
    }

    /// Text image and its dimensions in logical pixels.
    fn rendered(&self, row: usize, tokens: &[(&str, Style)]) -> (Image, f32, f32) {
        self.renders.set(self.renders.get() + 1);

        let selection = self
            .selection
            .get()
            .and_then(|(selected, points)| (selected == row).then_some(points));
        let rendered = self.engine.borrow_mut().render(
            &rich_text(tokens),
            self.scale,
            self.max_width(),
            selection,
        );

        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &rendered.pixels,
            rendered.width,
            rendered.height,
        );
        let image = Image::from_rgba8_premultiplied(buffer);
        let width = rendered.width as f32 / self.scale;
        let height = rendered.height as f32 / self.scale;
        (image, width, height)
    }

    /// Redraw one message. Borrows must have been released by this point:
    /// the notification immediately invokes `row_data`.
    fn invalidate(&self, row: usize) {
        self.notify.row_changed(row);
    }

    fn press(&self, row: usize, point: (f32, f32)) {
        let points = SelectionPoints {
            anchor: point,
            focus: point,
        };
        if let Some((previous, _)) = self.selection.replace(Some((row, points)))
            && previous != row
        {
            self.invalidate(previous);
        }
        self.invalidate(row);
    }

    fn drag(&self, row: usize, point: (f32, f32)) {
        if let Some((selected, mut points)) = self.selection.get()
            && selected == row
        {
            points.focus = point;
            self.selection.set(Some((row, points)));
            self.invalidate(row);
        }
    }

    fn release(&self, row: usize) {
        let Some((selected, points)) = self.selection.get() else {
            return;
        };
        if selected != row {
            return;
        }
        let rich = rich_text(&generate(row).tokens);
        let travelled =
            (points.focus.0 - points.anchor.0).abs() + (points.focus.1 - points.anchor.1).abs();

        if travelled < 3.0 * self.scale {
            // A press without dragging is a click rather than a selection.
            self.selection.set(None);
            self.invalidate(row);
            let link =
                self.engine
                    .borrow_mut()
                    .link_at(&rich, self.scale, self.max_width(), points.focus);
            if let Some(link) = link {
                eprintln!("link: {}", rich.links[link]);
            }
        } else {
            let selected_text =
                self.engine
                    .borrow_mut()
                    .selected_text(&rich, self.scale, self.max_width(), points);
            eprintln!("selected: {selected_text:?}");
        }
    }

    /// Self-check without a mouse: selects text in the last message and tests link hit detection.
    fn demo_select(&self) {
        let row = self.count.saturating_sub(1);
        if let Some(message) = self.row_data(row) {
            let (width, height) = (
                message.text_width * self.scale,
                message.text_height * self.scale,
            );
            self.press(row, (4.0, 6.0));
            self.drag(row, (width * 0.75, height - 6.0));
            self.release(row);
        }

        let with_link = (0..self.count.min(200))
            .map(|offset| self.count - 1 - offset)
            .map(|row| rich_text(&generate(row).tokens))
            .find(|rich| !rich.links.is_empty());
        if let Some(rich) = with_link {
            let mut engine = self.engine.borrow_mut();
            let (scale, max_width) = (self.scale, self.max_width());
            let inside = engine
                .link_center(&rich, scale, max_width)
                .and_then(|center| engine.link_at(&rich, scale, max_width, center));
            let outside = engine.link_at(&rich, scale, max_width, (-5.0, -5.0));
            eprintln!("link hit-test: inside={inside:?} outside={outside:?}");
        }
    }

    fn report(&self) {
        let engine = self.engine.borrow();
        eprintln!(
            "custom text: {} renders, {} layouts, {} glyphs rasterized",
            self.renders.get(),
            engine.layouts_built,
            engine.glyphs_rasterized
        );
    }
}

impl Model for FeedModel {
    type Data = Message;

    fn row_count(&self) -> usize {
        self.count
    }

    fn row_data(&self, row: usize) -> Option<Message> {
        if row >= self.count {
            return None;
        }
        let generated = generate(row);
        let minutes = row % (24 * 60);
        let mut message = Message {
            sender: SharedString::from(generated.sender),
            time: SharedString::from(format!("{:02}:{:02}", minutes / 60, minutes % 60)),
            outgoing: generated.outgoing,
            ..Message::default()
        };

        match self.mode {
            Mode::Plain => message.text = SharedString::from(plain_text(&generated.tokens)),
            Mode::Styled => {
                message.styled = StyledText::from_markdown(&markdown(&generated.tokens))
                    .unwrap_or_else(|err| {
                        eprintln!("markdown: row {row}: {err}");
                        StyledText::from_plain_text(&plain_text(&generated.tokens))
                    });
            }
            Mode::Custom => {
                let (image, width, height) = self.rendered(row, &generated.tokens);
                message.rendered = image;
                message.text_width = width;
                message.text_height = height;
            }
        }
        Some(message)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[derive(Debug)]
struct Args {
    count: usize,
    mode: Mode,
    oled: bool,
    start_bottom: bool,
    autoscroll: Option<Duration>,
    speed: f32,
    demo_select: bool,
    debug_row: Option<usize>,
    snapshot: Option<String>,
    exit_after: Option<Duration>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        count: 10_000,
        mode: Mode::Plain,
        oled: false,
        start_bottom: false,
        autoscroll: None,
        speed: 3000.0,
        demo_select: false,
        debug_row: None,
        snapshot: None,
        exit_after: None,
    };

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{flag}: a value is required"))
        };
        match flag.as_str() {
            "--count" => args.count = value()?.parse()?,
            "--mode" => {
                args.mode = match value()?.as_str() {
                    "plain" => Mode::Plain,
                    "styled" => Mode::Styled,
                    "custom" => Mode::Custom,
                    other => return Err(format!("unknown mode: {other}").into()),
                }
            }
            "--theme" => {
                args.oled = match value()?.as_str() {
                    "dark" => false,
                    "oled" => true,
                    other => return Err(format!("unknown theme: {other}").into()),
                }
            }
            "--start-bottom" => args.start_bottom = true,
            "--autoscroll" => args.autoscroll = Some(Duration::from_secs_f32(value()?.parse()?)),
            "--speed" => args.speed = value()?.parse()?,
            "--demo-select" => args.demo_select = true,
            "--debug-row" => args.debug_row = Some(value()?.parse()?),
            "--snapshot" => args.snapshot = Some(value()?),
            "--exit-after" => args.exit_after = Some(Duration::from_secs_f32(value()?.parse()?)),
            other => return Err(format!("unknown option: {other}").into()),
        }
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    if let Some(row) = args.debug_row {
        let rich = rich_text(&generate(row).tokens);
        TextEngine::new().debug_layout(&rich, 1.0, TEXT_MAX_WIDTH);
        return Ok(());
    }
    let ui = FeedWindow::new()?;

    // The display scale is known only after the window is created, and is needed for sharp text rendering.
    ui.show()?;
    let scale = ui.window().scale_factor();
    eprintln!("mode: {:?}, scale factor: {scale}", args.mode);

    let model = Rc::new(FeedModel::new(args.count, args.mode, scale));
    ui.set_mode(match args.mode {
        Mode::Plain => 0,
        Mode::Styled => 1,
        Mode::Custom => 2,
    });
    ui.set_oled(args.oled);
    ui.set_messages(ModelRc::from(Rc::clone(&model)));
    ui.on_link_clicked(|url| eprintln!("link: {url}"));

    ui.on_text_pressed({
        let model = Rc::clone(&model);
        move |row, x, y| model.press(row as usize, (x * model.scale, y * model.scale))
    });
    ui.on_text_moved({
        let model = Rc::clone(&model);
        move |row, x, y| model.drag(row as usize, (x * model.scale, y * model.scale))
    });
    ui.on_text_released({
        let model = Rc::clone(&model);
        move |row| model.release(row as usize)
    });

    // Timers must live until the event loop ends.
    let mut timers = Vec::new();

    if args.start_bottom {
        let weak = ui.as_weak();
        let timer = Timer::default();
        // The content height is known only after the first layout pass.
        timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(300),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_scroll_y(-(ui.get_content_height() - ui.get_visible_height()).max(0.0));
                }
            },
        );
        timers.push(timer);
    }

    if args.demo_select {
        let model = Rc::clone(&model);
        let timer = Timer::default();
        timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(1200),
            move || model.demo_select(),
        );
        timers.push(timer);
    }

    if let Some(duration) = args.autoscroll {
        let weak = ui.as_weak();
        let started = Instant::now();
        let last_tick = Cell::new(Instant::now());
        let direction = Cell::new(-1.0_f32);
        let ticks = Rc::new(Cell::new(0_u32));
        let ticks_for_report = Rc::clone(&ticks);
        let speed = args.speed;

        let timer = Timer::default();
        timer.start(TimerMode::Repeated, Duration::from_millis(8), move || {
            let Some(ui) = weak.upgrade() else { return };
            if started.elapsed() >= duration {
                return;
            }
            let now = Instant::now();
            let dt = now.duration_since(last_tick.replace(now)).as_secs_f32();
            let lowest = -(ui.get_content_height() - ui.get_visible_height()).max(0.0);
            let mut y = ui.get_scroll_y() + direction.get() * speed * dt;
            if y <= lowest {
                y = lowest;
                direction.set(1.0);
            } else if y >= 0.0 {
                y = 0.0;
                direction.set(-1.0);
            }
            ui.set_scroll_y(y);
            ticks.set(ticks.get() + 1);
        });
        timers.push(timer);

        let report = Timer::default();
        report.start(TimerMode::SingleShot, duration, move || {
            eprintln!(
                "autoscroll: {} steps in {:.1} s",
                ticks_for_report.get(),
                duration.as_secs_f32()
            );
        });
        timers.push(report);
    }

    if let Some(path) = args.snapshot {
        let weak = ui.as_weak();
        let timer = Timer::default();
        timer.start(TimerMode::SingleShot, Duration::from_secs(2), move || {
            let Some(ui) = weak.upgrade() else { return };
            match ui.window().take_snapshot() {
                Ok(buffer) => {
                    let saved = image::save_buffer(
                        &path,
                        buffer.as_bytes(),
                        buffer.width(),
                        buffer.height(),
                        image::ColorType::Rgba8,
                    );
                    match saved {
                        Ok(()) => eprintln!("snapshot: {path}"),
                        Err(err) => eprintln!("snapshot: failed to save: {err}"),
                    }
                }
                Err(err) => eprintln!("snapshot: failed to capture: {err}"),
            }
        });
        timers.push(timer);
    }

    if let Some(after) = args.exit_after {
        let timer = Timer::default();
        timer.start(TimerMode::SingleShot, after, || {
            if let Err(err) = slint::quit_event_loop() {
                eprintln!("exit-after: {err}");
            }
        });
        timers.push(timer);
    }

    slint::run_event_loop()?;
    drop(timers);
    if args.mode == Mode::Custom {
        model.report();
    }
    Ok(())
}
