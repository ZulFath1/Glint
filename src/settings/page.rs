//! What each Settings page holds, where everything goes, and how it looks.
//!
//! A page is a list of rows. Each row is one setting: a label on the left
//! and a control on the right. A control has one or more *parts* (the
//! clickable / focusable pieces: a segment, a swatch, a button...). This
//! file only does layout math and painting; `mod.rs` handles input.
//!
//! Everything is in DIPs, measured from the window's client top-left.

use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1Brush, ID2D1DeviceContext, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    IDWriteTextFormat,
};
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use crate::config::{
    self, Accent, BackgroundMode, Config, MAX_COLUMNS, MIN_COLUMNS, Preset, Rgb, ThemeId,
};
use crate::ui::background;
use crate::ui::gfx::{self, Gfx, text_primary, text_secondary};
use crate::ui::layout::Rect;

// ---------------------------------------------------------------------------
// Sizes
// ---------------------------------------------------------------------------

/// The window's client area when it opens (IMPROVE.md: about 720 × 560),
/// and the smallest it can be made (every page always fits).
pub const DEFAULT_SIZE: Vector2 = Vector2 { X: 720.0, Y: 580.0 };
pub const MIN_SIZE: Vector2 = Vector2 { X: 720.0, Y: 540.0 };

pub const SIDEBAR_W: f32 = 196.0;
const SIDE_PAD: f32 = 10.0;
/// The first section starts below the "Settings" heading.
const SIDE_TOP: f32 = 60.0;
const SIDE_ITEM_H: f32 = 38.0;
const SIDE_GAP: f32 = 2.0;

pub const CONTENT_X: f32 = SIDEBAR_W + 24.0;
const CONTENT_RIGHT: f32 = 24.0;
pub const CONTENT_TOP: f32 = 12.0;
pub const TITLE_H: f32 = 52.0;
const ROW_H: f32 = 52.0;
const LABEL_W: f32 = 150.0;
const CONTROL_H: f32 = 32.0;

pub const BUTTON_RADIUS: f32 = 8.0;
/// Space for the value at a slider's right end ("135°", "Medium").
const SLIDER_VALUE_W: f32 = 72.0;

/// Text colors that aren't the theme's: a soft red for warnings.
const WARNING: Rgb = Rgb::new(0xff, 0xb4, 0xa9);

// ---------------------------------------------------------------------------
// Sections and items
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Theme,
    Appearance,
    Layout,
    Animation,
    Music,
    General,
    About,
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Theme,
        Self::Appearance,
        Self::Layout,
        Self::Animation,
        Self::Music,
        Self::General,
        Self::About,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::Appearance => "Appearance",
            Self::Layout => "Layout",
            Self::Animation => "Animation",
            Self::Music => "Music",
            Self::General => "General",
            Self::About => "About",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&s| s == self).unwrap_or(0)
    }

    pub fn items(self) -> &'static [Item] {
        use Item::*;
        match self {
            Self::Theme => &[ThemeCard, IconTint],
            Self::Appearance => &[
                Mode,
                Presets,
                CustomColors,
                Angle,
                Darkness,
                GlassTint,
                AccentColor,
            ],
            Self::Layout => &[Columns, ShowNames],
            Self::Animation => &[OpenSpeed, Bounce, IconDelay, Replay],
            Self::Music => &[ShowMusic, AlbumTint],
            Self::General => &[Hotkey, Autostart, OpenConfig, Reset],
            Self::About => &[Version, Link, Ram],
        }
    }
}

/// The section names in the left list.
pub fn side_item(i: usize) -> Rect {
    Rect::new(
        SIDE_PAD,
        SIDE_TOP + i as f32 * (SIDE_ITEM_H + SIDE_GAP),
        SIDEBAR_W - 2.0 * SIDE_PAD,
        SIDE_ITEM_H,
    )
}

/// One setting (one row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    ThemeCard,
    IconTint,
    Mode,
    Presets,
    CustomColors,
    Angle,
    Darkness,
    GlassTint,
    AccentColor,
    Columns,
    ShowNames,
    OpenSpeed,
    Bounce,
    IconDelay,
    Replay,
    ShowMusic,
    AlbumTint,
    Hotkey,
    Autostart,
    OpenConfig,
    Reset,
    Version,
    Link,
    Ram,
}

/// How a row's control behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Pick one of several (segments, swatches): one Tab stop, Left/Right
    /// change the choice.
    Radio,
    Slider,
    Toggle,
    /// One or more buttons: a Tab stop each, Enter/Space presses.
    Buttons,
    /// Just text (nothing to focus).
    Text,
}

/// The Windows color picker is opened for one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorTarget {
    /// Custom gradient color `i` (0–2).
    Stop(usize),
    Accent,
}

/// The mode buttons, in the order IMPROVE.md lists them.
pub const MODES: [(BackgroundMode, &str); 4] = [
    (BackgroundMode::Solid, "Solid"),
    (BackgroundMode::Glass, "Glass"),
    (BackgroundMode::Gradient, "Gradient"),
    (BackgroundMode::GlassGradient, "Glass + Gradient"),
];

/// The preset swatches: the theme's own colors, the 5 built-in ones,
/// then "Custom".
pub fn presets() -> [Preset; 7] {
    let [a, b, c, d, e] = Preset::BUILT_IN;
    [Preset::Theme, a, b, c, d, e, Preset::Custom]
}

/// The theme cards: 3 per line, two lines.
const THEME_CARD_H: f32 = 116.0;
const THEME_CARD_GAP: f32 = 12.0;
const THEME_COLUMNS: usize = 3;

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            // The page title already says "Theme"; the cards use the row.
            Self::ThemeCard => "",
            Self::IconTint => "Icon tint",
            Self::Mode => "Background",
            Self::Presets => "Colors",
            Self::CustomColors => "Custom colors",
            Self::Angle => "Gradient angle",
            Self::Darkness => "Darkness",
            Self::GlassTint => "Glass tint",
            Self::AccentColor => "Accent color",
            Self::Columns => "Columns",
            Self::ShowNames => "App names",
            Self::OpenSpeed => "Open speed",
            Self::Bounce => "Bounce",
            Self::IconDelay => "Icon delay",
            Self::Replay => "Preview",
            Self::ShowMusic => "Music player",
            Self::AlbumTint => "Album tint",
            Self::Hotkey => "Hotkey",
            Self::Autostart => "Start with Windows",
            Self::OpenConfig => "Config file",
            Self::Reset => "Reset",
            Self::Version => "Version",
            Self::Link => "Source code",
            Self::Ram => "Memory",
        }
    }

    pub fn height(self) -> f32 {
        match self {
            Self::ThemeCard => 2.0 * THEME_CARD_H + THEME_CARD_GAP + 24.0,
            // Swatches with their names under them.
            Self::Presets => 68.0,
            // The shortcut field with a hint line under it.
            Self::Hotkey => 68.0,
            _ => ROW_H,
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::ThemeCard | Self::Mode | Self::Presets => Kind::Radio,
            Self::Angle
            | Self::Darkness
            | Self::GlassTint
            | Self::Columns
            | Self::OpenSpeed
            | Self::Bounce
            | Self::IconDelay => Kind::Slider,
            Self::ShowNames
            | Self::ShowMusic
            | Self::AlbumTint
            | Self::Autostart
            | Self::IconTint => Kind::Toggle,
            Self::Version | Self::Ram => Kind::Text,
            Self::CustomColors
            | Self::AccentColor
            | Self::Replay
            | Self::Hotkey
            | Self::OpenConfig
            | Self::Reset
            | Self::Link => Kind::Buttons,
        }
    }
}

// ---------------------------------------------------------------------------
// Sliders
// ---------------------------------------------------------------------------

/// A slider's range, in the units it shows (degrees, %, ms...).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

impl Range {
    /// Clamps to the range and rounds to a whole step, so the GUI can only
    /// ever produce values config.toml accepts.
    pub fn snap(self, v: f32) -> f32 {
        let steps = ((v.clamp(self.min, self.max) - self.min) / self.step).round();
        (self.min + steps * self.step).clamp(self.min, self.max)
    }

    /// Where `v` sits on the track, 0.0–1.0.
    pub fn fraction(self, v: f32) -> f32 {
        ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }
}

pub fn range(item: Item) -> Range {
    let r = |min, max, step| Range { min, max, step };
    match item {
        Item::Angle => r(0.0, 360.0, 1.0),
        Item::Darkness => r(0.0, 60.0, 1.0),
        Item::GlassTint => r(0.0, 100.0, 1.0),
        Item::Columns => r(MIN_COLUMNS as f32, MAX_COLUMNS as f32, 1.0),
        Item::OpenSpeed => r(120.0, 900.0, 10.0),
        Item::Bounce => r(1.0, 1.6, 0.01),
        Item::IconDelay => r(0.0, 60.0, 1.0),
        _ => r(0.0, 1.0, 1.0),
    }
}

/// The slider's value for this config, in its own units.
pub fn slider_value(item: Item, c: &Config) -> f32 {
    let v = match item {
        Item::Angle => c.background.angle.rem_euclid(360.0),
        Item::Darkness => c.background.overlay * 100.0,
        Item::GlassTint => c.background.tint * 100.0,
        Item::Columns => c.max_columns as f32,
        Item::OpenSpeed => c.animation.open_ms as f32,
        Item::Bounce => c.animation.overshoot,
        Item::IconDelay => c.animation.stagger_ms as f32,
        _ => 0.0,
    };
    let r = range(item);
    v.clamp(r.min, r.max)
}

/// Writes a slider value (snapped first) into the config.
pub fn set_slider(item: Item, c: &mut Config, v: f32) {
    let v = range(item).snap(v);
    // Percentages go back to 0–1 with exactly two decimals, so what is
    // saved reads back as the same number.
    let percent = |v: f32| (v.round() / 100.0 * 100.0).round() / 100.0;
    match item {
        Item::Angle => c.background.angle = v,
        Item::Darkness => c.background.overlay = percent(v),
        Item::GlassTint => c.background.tint = percent(v),
        Item::Columns => c.max_columns = v as usize,
        Item::OpenSpeed => c.animation.open_ms = v as u32,
        Item::Bounce => c.animation.overshoot = (v * 100.0).round() / 100.0,
        Item::IconDelay => c.animation.stagger_ms = v as u32,
        _ => {}
    }
}

/// The text at the slider's right end.
pub fn slider_text(item: Item, v: f32) -> String {
    match item {
        Item::Angle => format!("{v:.0}°"),
        Item::Darkness | Item::GlassTint => format!("{v:.0}%"),
        Item::Columns => format!("{v:.0}"),
        Item::OpenSpeed | Item::IconDelay => format!("{v:.0} ms"),
        Item::Bounce => match v {
            v if v < 1.005 => "None".into(),
            v if v < 1.2 => "Small".into(),
            v if v < 1.4 => "Medium".into(),
            _ => "Big".into(),
        },
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// What a row looks like right now
// ---------------------------------------------------------------------------

/// Everything painting and hit-testing need besides the config.
pub struct View<'a> {
    pub config: &'a Config,
    /// The accent in use (resolved, also for "Follow Windows").
    pub accent: Rgb,
    /// The hotkey field is waiting for keys.
    pub recording: bool,
    /// Shown under the hotkey field instead of the hint (a problem).
    pub hotkey_note: Option<&'a str>,
    /// Reset shows "Cancel" / "Reset" instead of its button.
    pub confirm_reset: bool,
    /// About: the memory line.
    pub ram: &'a str,
}

/// The control area of a row (right of the label).
fn control(row: Rect) -> Rect {
    Rect::new(
        row.x + LABEL_W,
        row.y + (ROW_H - CONTROL_H) / 2.0,
        (row.w - LABEL_W).max(0.0),
        CONTROL_H,
    )
}

/// The row rectangles of a page, top to bottom, for a window `width` wide.
pub fn rows(section: Section, width: f32) -> Vec<(Item, Rect)> {
    let w = (width - CONTENT_X - CONTENT_RIGHT).max(0.0);
    let mut y = CONTENT_TOP + TITLE_H;
    section
        .items()
        .iter()
        .map(|&item| {
            let r = Rect::new(CONTENT_X, y, w, item.height());
            y += item.height();
            (item, r)
        })
        .collect()
}

/// The page title's place.
pub fn title_rect(width: f32) -> Rect {
    Rect::new(
        CONTENT_X,
        CONTENT_TOP,
        (width - CONTENT_X - CONTENT_RIGHT).max(0.0),
        TITLE_H,
    )
}

/// A row of equal squares `size` wide with `gap` between them.
fn squares(c: Rect, n: usize, size: f32, gap: f32) -> Vec<Rect> {
    (0..n)
        .map(|i| Rect::new(c.x + i as f32 * (size + gap), c.y, size, c.h))
        .collect()
}

const SWATCH_W: f32 = 40.0;
const SWATCH_GAP: f32 = 6.0;
const STOP_W: f32 = 36.0;
const TOGGLE_W: f32 = 140.0;
const BUTTON_W: f32 = 180.0;

/// The clickable parts of a row, in window DIPs. Text rows have none.
pub fn parts(item: Item, row: Rect, v: &View) -> Vec<Rect> {
    let c = control(row);
    match item {
        Item::ThemeCard => {
            let w = (row.w - (THEME_COLUMNS - 1) as f32 * THEME_CARD_GAP) / THEME_COLUMNS as f32;
            (0..ThemeId::ALL.len())
                .map(|i| {
                    let (col, line) = (i % THEME_COLUMNS, i / THEME_COLUMNS);
                    Rect::new(
                        row.x + col as f32 * (w + THEME_CARD_GAP),
                        row.y + 4.0 + line as f32 * (THEME_CARD_H + THEME_CARD_GAP),
                        w.floor(),
                        THEME_CARD_H,
                    )
                })
                .collect()
        }
        Item::Mode => {
            // Each segment as wide as its name needs, sharing the row.
            let weight = |name: &str| name.chars().count() as f32 + 6.0;
            let total: f32 = MODES.iter().map(|(_, n)| weight(n)).sum();
            let mut x = c.x;
            MODES
                .iter()
                .map(|(_, name)| {
                    let w = c.w * weight(name) / total;
                    let r = Rect::new(x, c.y, w, c.h);
                    x += w;
                    r
                })
                .collect()
        }
        Item::Presets => {
            let top = Rect::new(c.x, row.y + 8.0, c.w, CONTROL_H);
            squares(top, presets().len(), SWATCH_W, SWATCH_GAP)
        }
        Item::CustomColors => {
            let n = v.config.background.colors.len().clamp(1, 3);
            let buttons = usize::from(n < 3) + usize::from(n > 1);
            squares(c, n + buttons, STOP_W, SWATCH_GAP)
        }
        Item::AccentColor => vec![
            Rect::new(c.x, c.y, SWATCH_W, c.h),
            Rect::new(c.x + SWATCH_W + 12.0, c.y, 190.0, c.h),
        ],
        Item::Hotkey => vec![Rect::new(c.x, row.y + 6.0, 200.0, CONTROL_H)],
        Item::Reset if v.confirm_reset => vec![
            Rect::new(c.x, c.y, 96.0, c.h),
            Rect::new(c.x + 104.0, c.y, 120.0, c.h),
        ],
        Item::Link => vec![Rect::new(c.x, c.y, c.w.min(260.0), c.h)],
        Item::Replay | Item::OpenConfig | Item::Reset => {
            vec![Rect::new(c.x, c.y, BUTTON_W, c.h)]
        }
        _ => match item.kind() {
            Kind::Slider => vec![c],
            Kind::Toggle => vec![Rect::new(c.x, c.y, TOGGLE_W, c.h)],
            _ => Vec::new(),
        },
    }
}

/// For the custom colors row: what part `i` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPart {
    Color(usize),
    Add,
    Remove,
}

pub fn stop_part(colors: usize, i: usize) -> Option<StopPart> {
    let n = colors.clamp(1, 3);
    if i < n {
        return Some(StopPart::Color(i));
    }
    let mut extra = Vec::new();
    if n < 3 {
        extra.push(StopPart::Add);
    }
    if n > 1 {
        extra.push(StopPart::Remove);
    }
    extra.get(i - n).copied()
}

/// For radio rows: the part that is chosen now.
pub fn chosen(item: Item, c: &Config) -> usize {
    match item {
        Item::Mode => MODES
            .iter()
            .position(|(m, _)| *m == c.background.mode)
            .unwrap_or(0),
        Item::Presets => presets()
            .iter()
            .position(|p| *p == c.background.preset)
            .unwrap_or(0),
        Item::ThemeCard => ThemeId::ALL.iter().position(|t| *t == c.theme).unwrap_or(0),
        _ => 0,
    }
}

/// The slider's track (the line the thumb runs along) inside its part.
pub fn track(part: Rect) -> Rect {
    Rect::new(
        part.x + 8.0,
        part.y,
        (part.w - SLIDER_VALUE_W - 16.0).max(1.0),
        part.h,
    )
}

/// One theme card: a small drawing of the theme (its colors, search bar,
/// icon tiles and layout) with its name under it.
fn paint_theme_card(
    pen: &Pen,
    fonts: &Fonts,
    r: Rect,
    id: ThemeId,
    selected: bool,
    accent: Rgb,
) -> Result<()> {
    use crate::theme::{IconTile, LayoutKind};
    let t = id.theme();
    let p = t.palette;
    let s = t.shape;
    // Corners scaled down with the drawing (a 24 px menu is ~8 px here).
    let small = |radius: f32| (radius / 3.0).clamp(1.0, 10.0);
    pen.gradient(r, small(s.menu_radius).max(4.0), p.background)?;
    let mini = Rect::new(r.x + 10.0, r.y + 10.0, r.w - 20.0, 72.0);
    let fill = |x: Rect, radius: f32, paint: crate::theme::Paint| {
        pen.fill(x, radius, paint.color, paint.alpha)
    };
    let tile_fill = |x: Rect, i: usize| -> Result<()> {
        let radius = match t.tile {
            IconTile::None | IconTile::Sharp => 1.0,
            IconTile::Squircle => 5.0,
            IconTile::Rounded | IconTile::Bordered => 3.0,
        };
        match t.tile {
            IconTile::None => pen.fill(x, 3.0, p.text, 0.55),
            _ => fill(x, radius, p.tile),
        }?;
        if let (IconTile::Bordered, false) = (t.tile, p.tile_borders.is_empty()) {
            pen.stroke(
                x,
                radius,
                p.tile_borders[i % p.tile_borders.len()],
                1.0,
                1.0,
            )?;
        }
        // The "icon" itself: a dot in an accent-ish color.
        let dot = Rect::new(x.x + x.w / 2.0 - 2.5, x.y + x.h / 2.0 - 2.5, 5.0, 5.0);
        pen.fill(dot, 2.5, p.text, 0.9)
    };
    match t.layout {
        LayoutKind::Grid | LayoutKind::List => {
            let mut y = mini.y;
            if t.terminal {
                fill(Rect::new(mini.x, y, mini.w, 6.0), 0.0, p.footer)?;
                y += 9.0;
            }
            let search = Rect::new(mini.x, y, mini.w, 12.0);
            fill(search, small(s.search_radius).min(6.0), p.field)?;
            pen.stroke(search, small(s.search_radius).min(6.0), p.accent, 0.8, 1.0)?;
            y += 17.0;
            if t.layout == LayoutKind::List {
                // Rows, the first one selected (accent), then a pinned strip.
                for row in 0..3 {
                    let rr = Rect::new(mini.x, y + row as f32 * 10.0, mini.w, 8.0);
                    if row == 0 {
                        pen.fill(rr, 2.0, p.accent, 1.0)?;
                    } else {
                        pen.fill(
                            Rect::new(rr.x + 4.0, rr.y + 2.0, rr.w * 0.6, 4.0),
                            2.0,
                            p.text,
                            0.5,
                        )?;
                    }
                }
                for i in 0..5 {
                    tile_fill(Rect::new(mini.x + i as f32 * 14.0, y + 32.0, 10.0, 10.0), i)?;
                }
            } else {
                let size = 14.0;
                let gap = ((mini.w - 5.0 * size) / 4.0).max(2.0);
                for line in 0..2 {
                    for col in 0..5 {
                        let x = mini.x + col as f32 * (size + gap);
                        let tile = Rect::new(x, y + line as f32 * (size + 5.0), size, size);
                        tile_fill(tile, line * 5 + col)?;
                    }
                }
            }
            fill(
                Rect::new(mini.x - 10.0, mini.bottom() - 6.0, r.w, 10.0),
                0.0,
                p.footer,
            )?;
        }
        LayoutKind::Sidebar => {
            let rail = Rect::new(r.x, r.y, 20.0, mini.bottom() + 4.0 - r.y);
            pen.fill(
                rail,
                small(s.menu_radius),
                p.background[p.background.len() - 1],
                1.0,
            )?;
            for i in 0..3 {
                pen.fill(
                    Rect::new(r.x + 6.0, mini.y + i as f32 * 12.0, 8.0, 8.0),
                    4.0,
                    p.accent,
                    0.8,
                )?;
            }
            let x = r.x + 28.0;
            let w = r.right() - 10.0 - x;
            pen.fill(Rect::new(x, mini.y, w * 0.5, 4.0), 2.0, p.subtext, 0.7)?;
            pen.fill(
                Rect::new(x, mini.y + 8.0, w * 0.35, 10.0),
                2.0,
                p.text,
                0.95,
            )?;
            let search = Rect::new(x, mini.y + 24.0, w, 10.0);
            fill(search, small(s.search_radius), p.field)?;
            for i in 0..4 {
                let tile = Rect::new(x + i as f32 * 17.0, mini.y + 40.0, 13.0, 13.0);
                tile_fill(tile, i)?;
            }
        }
    }
    // Name band.
    let band = Rect::new(r.x, r.bottom() - 30.0, r.w, 30.0);
    pen.fill(band, 0.0, Rgb::new(0, 0, 0), 0.30)?;
    pen.text(
        id.title(),
        &fonts.strong,
        Rect::new(r.x + 10.0, band.y, r.w - 20.0, band.h),
        DWRITE_TEXT_ALIGNMENT_LEADING,
        p.text,
    )?;
    if selected {
        let ring = Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
        pen.stroke(ring, 10.0, accent, 1.0, 2.0)
    } else {
        pen.stroke(r, 8.0, WHITE, 0.14, 1.0)
    }
}

/// Light text on dark or colored fills (Glint's #f4f4f5).
const LIGHT_TEXT: Rgb = Rgb::new(0xf4, 0xf4, 0xf5);

/// Text on a filled accent shape: dark or light, whichever reads better.
pub fn on_accent(accent: Rgb) -> Rgb {
    let dark = Rgb::new(0x1b, 0x1b, 0x1f);
    let light = LIGHT_TEXT;
    if background::contrast(dark, accent) >= background::contrast(light, accent) {
        dark
    } else {
        light
    }
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// The text formats Settings paints with (made once per window).
pub struct Fonts {
    pub label: IDWriteTextFormat,
    pub value: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub strong: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    pub heading: IDWriteTextFormat,
}

impl Fonts {
    pub fn new(gfx: &Gfx) -> Result<Self> {
        Ok(Self {
            label: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            value: gfx.text_format(12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            small: gfx.text_format(11.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            strong: gfx.text_format(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            title: gfx.text_format(20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            heading: gfx.text_format(15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        })
    }
}

/// Small drawing helpers. Every rect is relative to the canvas.
struct Pen<'a> {
    ctx: &'a ID2D1DeviceContext,
    gfx: &'a Gfx,
}

impl Pen<'_> {
    fn round(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
        D2D1_ROUNDED_RECT {
            rect: gfx::d2d_rect(r),
            radiusX: radius,
            radiusY: radius,
        }
    }

    fn fill(&self, r: Rect, radius: f32, c: Rgb, alpha: f32) -> Result<()> {
        let brush = gfx::brush(self.ctx, c, alpha)?;
        // SAFETY: plain drawing call with a live brush and a local struct.
        unsafe {
            self.ctx
                .FillRoundedRectangle(&Self::round(r, radius), &brush)
        };
        Ok(())
    }

    /// An outline `width` thick, fully inside `r`.
    fn stroke(&self, r: Rect, radius: f32, c: Rgb, alpha: f32, width: f32) -> Result<()> {
        let brush = gfx::brush(self.ctx, c, alpha)?;
        let h = width / 2.0;
        let inner = Rect::new(r.x + h, r.y + h, r.w - width, r.h - width);
        // SAFETY: plain drawing call with a live brush and a local struct.
        unsafe {
            self.ctx.DrawRoundedRectangle(
                &Self::round(inner, (radius - h).max(0.0)),
                &brush,
                width,
                None,
            )
        };
        Ok(())
    }

    fn circle(&self, center: Vector2, radius: f32, c: Rgb, alpha: f32) -> Result<()> {
        let brush = gfx::brush(self.ctx, c, alpha)?;
        // SAFETY: plain drawing call with a live brush and a local struct.
        unsafe {
            self.ctx.FillEllipse(
                &D2D1_ELLIPSE {
                    point: center,
                    radiusX: radius,
                    radiusY: radius,
                },
                &brush,
            )
        };
        Ok(())
    }

    fn text(
        &self,
        s: &str,
        format: &IDWriteTextFormat,
        r: Rect,
        align: DWRITE_TEXT_ALIGNMENT,
        c: Rgb,
    ) -> Result<()> {
        let layout = self.gfx.text_layout(s, format, r.size(), align, true)?;
        let brush = gfx::brush(self.ctx, c, 1.0)?;
        // SAFETY: draws a live text layout with a live brush.
        unsafe {
            self.ctx.DrawTextLayout(
                Vector2 { X: r.x, Y: r.y },
                &layout,
                &brush,
                gfx::TEXT_OPTIONS,
            )
        };
        Ok(())
    }

    /// A rounded rect filled with `colors` blending left to right (one
    /// color = flat).
    fn gradient(&self, r: Rect, radius: f32, colors: &[Rgb]) -> Result<()> {
        if colors.len() < 2 {
            let c = colors.first().copied().unwrap_or(Rgb::new(0, 0, 0));
            return self.fill(r, radius, c, 1.0);
        }
        let last = (colors.len() - 1) as f32;
        let stops: Vec<D2D1_GRADIENT_STOP> = colors
            .iter()
            .enumerate()
            .map(|(i, &c)| D2D1_GRADIENT_STOP {
                position: i as f32 / last,
                color: gfx::d2d_color(c, 1.0),
            })
            .collect();
        // SAFETY: plain COM calls; the stops are copied during the call.
        unsafe {
            // The plain render-target version (the device context has a
            // longer one with the same name).
            let target: &ID2D1RenderTarget = self.ctx;
            let collection = target.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            )?;
            let brush = self.ctx.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 { X: r.x, Y: r.y },
                    endPoint: Vector2 {
                        X: r.right(),
                        Y: r.bottom(),
                    },
                },
                None,
                &collection,
            )?;
            let brush: ID2D1Brush = brush.cast()?;
            self.ctx
                .FillRoundedRectangle(&Self::round(r, radius), &brush);
        }
        Ok(())
    }
}

const WHITE: Rgb = Rgb::new(255, 255, 255);
const BLACK: Rgb = Rgb::new(0, 0, 0);

/// Moves `r` (window DIPs) into a row canvas whose top-left is `origin`.
fn local(r: Rect, origin: Rect) -> Rect {
    Rect::new(r.x - origin.x, r.y - origin.y, r.w, r.h)
}

/// Paints the "Settings" heading and the section names (the left list).
pub fn paint_sidebar(
    ctx: &ID2D1DeviceContext,
    gfx: &Gfx,
    fonts: &Fonts,
    selected: Section,
    accent: Rgb,
) -> Result<()> {
    let pen = Pen { ctx, gfx };
    pen.text(
        "Settings",
        &fonts.heading,
        Rect::new(20.0, 14.0, SIDEBAR_W - 30.0, 32.0),
        DWRITE_TEXT_ALIGNMENT_LEADING,
        text_primary(),
    )?;
    for (i, section) in Section::ALL.iter().enumerate() {
        let r = side_item(i);
        let on = *section == selected;
        if on {
            // A short accent bar marks the open section.
            pen.fill(
                Rect::new(r.x + 2.0, r.y + 11.0, 3.0, r.h - 22.0),
                1.5,
                accent,
                1.0,
            )?;
        }
        let format = if on { &fonts.strong } else { &fonts.label };
        let color = if on { text_primary() } else { text_secondary() };
        pen.text(
            section.name(),
            format,
            Rect::new(r.x + 14.0, r.y, r.w - 18.0, r.h),
            DWRITE_TEXT_ALIGNMENT_LEADING,
            color,
        )?;
    }
    Ok(())
}

pub fn paint_title(
    ctx: &ID2D1DeviceContext,
    gfx: &Gfx,
    fonts: &Fonts,
    section: Section,
    size: Vector2,
) -> Result<()> {
    Pen { ctx, gfx }.text(
        section.name(),
        &fonts.title,
        Rect::new(0.0, 0.0, size.X, size.Y),
        DWRITE_TEXT_ALIGNMENT_LEADING,
        text_primary(),
    )
}

/// Paints one row (label + control) into its canvas. `row` is where the
/// canvas sits, in window DIPs.
pub fn paint_row(
    ctx: &ID2D1DeviceContext,
    gfx: &Gfx,
    fonts: &Fonts,
    item: Item,
    row: Rect,
    v: &View,
) -> Result<()> {
    let pen = Pen { ctx, gfx };
    let c = v.config;
    let primary = text_primary();
    let secondary = text_secondary();
    let left = DWRITE_TEXT_ALIGNMENT_LEADING;
    let center = DWRITE_TEXT_ALIGNMENT_CENTER;

    let label = if item == Item::Reset && v.confirm_reset {
        "Are you sure?"
    } else {
        item.label()
    };
    pen.text(
        label,
        &fonts.label,
        Rect::new(0.0, 0.0, LABEL_W - 8.0, ROW_H),
        left,
        primary,
    )?;

    let parts: Vec<Rect> = parts(item, row, v)
        .into_iter()
        .map(|r| local(r, row))
        .collect();
    let ctrl = local(control(row), row);

    let button = |r: Rect, text: &str| -> Result<()> {
        pen.fill(r, BUTTON_RADIUS, WHITE, 0.10)?;
        pen.stroke(r, BUTTON_RADIUS, WHITE, 0.12, 1.0)?;
        pen.text(text, &fonts.label, r, center, primary)
    };

    match item {
        Item::ThemeCard => {
            let on = chosen(item, c);
            for (i, (r, id)) in parts.iter().zip(ThemeId::ALL).enumerate() {
                paint_theme_card(&pen, fonts, *r, id, i == on, v.accent)?;
            }
        }
        Item::Mode => {
            let whole = Rect::new(ctrl.x, ctrl.y, parts.iter().map(|r| r.w).sum(), ctrl.h);
            pen.fill(whole, BUTTON_RADIUS, WHITE, 0.08)?;
            let on = chosen(item, c);
            for (i, (r, (_, name))) in parts.iter().zip(MODES).enumerate() {
                let color = if i == on {
                    pen.fill(*r, BUTTON_RADIUS, v.accent, 1.0)?;
                    on_accent(v.accent)
                } else {
                    primary
                };
                pen.text(name, &fonts.value, *r, center, color)?;
            }
        }
        Item::Presets => {
            let on = chosen(item, c);
            for (i, (r, preset)) in parts.iter().zip(presets()).enumerate() {
                let colors = match (preset, preset.colors()) {
                    (Preset::Theme, _) => c.theme.theme().palette.background.to_vec(),
                    (_, Some(colors)) => colors.to_vec(),
                    (_, None) => c.background.colors.clone(),
                };
                pen.gradient(*r, BUTTON_RADIUS, &colors)?;
                pen.stroke(*r, BUTTON_RADIUS, WHITE, 0.18, 1.0)?;
                if i == on {
                    let ring = Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
                    pen.stroke(ring, BUTTON_RADIUS + 3.0, WHITE, 0.95, 2.0)?;
                }
                let name = Rect::new(r.x - 4.0, r.bottom() + 4.0, r.w + 8.0, 16.0);
                let color = if i == on { primary } else { secondary };
                pen.text(
                    &title_case(preset.name()),
                    &fonts.small,
                    name,
                    center,
                    color,
                )?;
            }
        }
        Item::CustomColors => {
            let colors = &c.background.colors;
            for (i, r) in parts.iter().enumerate() {
                match stop_part(colors.len(), i) {
                    Some(StopPart::Color(n)) => {
                        let color = colors.get(n).copied().unwrap_or(BLACK);
                        pen.fill(*r, BUTTON_RADIUS, color, 1.0)?;
                        pen.stroke(*r, BUTTON_RADIUS, WHITE, 0.35, 1.0)?;
                    }
                    Some(StopPart::Add) => button(*r, "+")?,
                    Some(StopPart::Remove) => button(*r, "\u{2212}")?,
                    None => {}
                }
            }
            if c.background.preset != Preset::Custom
                && let Some(last) = parts.last()
            {
                let hint = Rect::new(last.right() + 10.0, ctrl.y, 140.0, ctrl.h);
                pen.text("Click to use these", &fonts.small, hint, left, secondary)?;
            }
        }
        Item::AccentColor => {
            let follow = c.accent == Accent::System;
            pen.fill(parts[0], BUTTON_RADIUS, v.accent, 1.0)?;
            pen.stroke(parts[0], BUTTON_RADIUS, WHITE, 0.35, 1.0)?;
            paint_toggle(&pen, fonts, parts[1], follow, "Follow Windows", v.accent)?;
        }
        Item::Hotkey => {
            let field = parts[0];
            pen.fill(field, BUTTON_RADIUS, BLACK, 0.30)?;
            let (border, alpha, width) = if v.recording {
                (v.accent, 1.0, 2.0)
            } else {
                (WHITE, 0.16, 1.0)
            };
            pen.stroke(field, BUTTON_RADIUS, border, alpha, width)?;
            let (text, color) = match (&c.hotkey, v.recording) {
                (_, true) => ("Press the keys…".to_string(), secondary),
                (Some(k), false) => (k.text.clone(), primary),
                (None, false) => ("None (Win key only)".to_string(), secondary),
            };
            let inner = Rect::new(field.x + 12.0, field.y, field.w - 24.0, field.h);
            pen.text(&text, &fonts.label, inner, left, color)?;
            let (hint, color) = match v.hotkey_note {
                Some(note) => (note, WARNING),
                None if v.recording => ("Esc cancels, Backspace removes the hotkey.", secondary),
                None => ("Click, then press Ctrl or Alt with a key.", secondary),
            };
            let line = Rect::new(field.x, field.bottom() + 6.0, row.w - field.x, 16.0);
            pen.text(hint, &fonts.small, line, left, color)?;
        }
        Item::Reset if v.confirm_reset => {
            button(parts[0], "Cancel")?;
            pen.fill(parts[1], BUTTON_RADIUS, Rgb::new(0xc4, 0x2b, 0x1c), 1.0)?;
            pen.text("Reset", &fonts.label, parts[1], center, LIGHT_TEXT)?;
            let note = Rect::new(parts[1].right() + 12.0, ctrl.y, 160.0, ctrl.h);
            pen.text("Pins stay.", &fonts.small, note, left, secondary)?;
        }
        Item::Replay => button(parts[0], "Replay animation")?,
        Item::OpenConfig => button(parts[0], "Open config file")?,
        Item::Reset => button(parts[0], "Reset to defaults…")?,
        Item::Link => button(parts[0], "github.com/ZulFath1/Glint")?,
        Item::Version => pen.text(
            &format!("Glint {}", env!("CARGO_PKG_VERSION")),
            &fonts.label,
            ctrl,
            left,
            primary,
        )?,
        Item::Ram => pen.text(v.ram, &fonts.label, ctrl, left, primary)?,
        _ => match item.kind() {
            Kind::Slider => paint_slider(&pen, fonts, item, parts[0], c, v.accent)?,
            Kind::Toggle => {
                let on = match item {
                    Item::ShowNames => c.show_names,
                    Item::ShowMusic => c.music.enabled,
                    Item::AlbumTint => c.background.tint_from_album,
                    Item::IconTint => c.icon_tint,
                    Item::Autostart => c.autostart,
                    _ => false,
                };
                paint_toggle(
                    &pen,
                    fonts,
                    parts[0],
                    on,
                    if on { "On" } else { "Off" },
                    v.accent,
                )?;
            }
            _ => {}
        },
    }
    Ok(())
}

fn paint_toggle(
    pen: &Pen,
    fonts: &Fonts,
    r: Rect,
    on: bool,
    text: &str,
    accent: Rgb,
) -> Result<()> {
    let pill = Rect::new(r.x, r.y + (r.h - 20.0) / 2.0, 40.0, 20.0);
    let knob_x = if on {
        pill.right() - 10.0
    } else {
        pill.x + 10.0
    };
    let knob = Vector2 {
        X: knob_x,
        Y: pill.y + 10.0,
    };
    if on {
        pen.fill(pill, 10.0, accent, 1.0)?;
        pen.circle(knob, 6.0, on_accent(accent), 1.0)?;
    } else {
        pen.stroke(pill, 10.0, WHITE, 0.65, 1.0)?;
        pen.circle(knob, 5.0, WHITE, 0.85)?;
    }
    let label = Rect::new(pill.right() + 10.0, r.y, r.w - 50.0, r.h);
    pen.text(
        text,
        &fonts.label,
        label,
        DWRITE_TEXT_ALIGNMENT_LEADING,
        text_primary(),
    )
}

fn paint_slider(
    pen: &Pen,
    fonts: &Fonts,
    item: Item,
    part: Rect,
    c: &Config,
    accent: Rgb,
) -> Result<()> {
    let value = slider_value(item, c);
    let t = track(part);
    let mid = t.y + t.h / 2.0;
    let x = t.x + range(item).fraction(value) * t.w;
    pen.fill(Rect::new(t.x, mid - 2.0, t.w, 4.0), 2.0, WHITE, 0.22)?;
    pen.fill(Rect::new(t.x, mid - 2.0, x - t.x, 4.0), 2.0, accent, 1.0)?;
    pen.circle(Vector2 { X: x, Y: mid }, 8.0, WHITE, 1.0)?;
    pen.circle(Vector2 { X: x, Y: mid }, 4.0, accent, 1.0)?;
    let text = Rect::new(
        part.right() - SLIDER_VALUE_W,
        part.y,
        SLIDER_VALUE_W,
        part.h,
    );
    pen.text(
        &slider_text(item, value),
        &fonts.value,
        text,
        DWRITE_TEXT_ALIGNMENT_TRAILING,
        text_secondary(),
    )
}

/// For the About page: "12.3 MB".
pub fn ram_text(bytes: usize) -> String {
    format!(
        "{:.1} MB (Task Manager's \"Memory\", read when this page opened)",
        bytes as f64 / (1024.0 * 1024.0)
    )
}

/// Does `config` change anything the Settings window shows?
pub fn same_for_settings(a: &Config, b: &Config) -> bool {
    let strip = |c: &Config| Config {
        pinned: Vec::new(),
        stats: config::Stats::default(),
        ..c.clone()
    };
    strip(a) == strip(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_only_make_values_the_config_accepts() {
        let mut c = Config::default();
        for item in [
            Item::Angle,
            Item::Darkness,
            Item::GlassTint,
            Item::Columns,
            Item::OpenSpeed,
            Item::Bounce,
            Item::IconDelay,
        ] {
            let r = range(item);
            for v in [-1000.0, r.min, (r.min + r.max) / 2.3, r.max, 1e6, f32::MAX] {
                set_slider(item, &mut c, v);
                let back = slider_value(item, &c);
                assert!((r.min..=r.max).contains(&back), "{item:?} {v}");
            }
        }
        // Whatever the sliders did, the config saves and loads unchanged.
        let text = config::default_text(&[]);
        let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
        config::save_changes(&mut doc, &config::parse(&text).unwrap(), &c);
        let mut back = config::parse(&doc.to_string()).unwrap();
        back.pinned = c.pinned.clone();
        assert_eq!(back, c);
    }

    #[test]
    fn slider_steps_and_text() {
        let r = range(Item::OpenSpeed);
        assert_eq!(r.snap(347.0), 350.0);
        assert_eq!(r.snap(10.0), 120.0);
        assert_eq!(range(Item::Bounce).snap(1.234), 1.23);
        assert_eq!(slider_text(Item::Bounce, 1.0), "None");
        assert_eq!(slider_text(Item::Bounce, 1.25), "Medium");
        assert_eq!(slider_text(Item::Bounce, 1.6), "Big");
        assert_eq!(slider_text(Item::Angle, 135.0), "135°");
        let mut c = Config::default();
        set_slider(Item::Darkness, &mut c, 25.4);
        assert_eq!(c.background.overlay, 0.25);
        // An angle written as -45 in the file shows as 315.
        c.background.angle = -45.0;
        assert_eq!(slider_value(Item::Angle, &c), 315.0);
    }

    #[test]
    fn every_page_fits_the_smallest_window() {
        let v = View {
            config: &Config::default(),
            accent: Rgb::new(0x8f, 0xa8, 0xff),
            recording: false,
            hotkey_note: None,
            confirm_reset: true,
            ram: "",
        };
        for section in Section::ALL {
            let rows = rows(section, MIN_SIZE.X);
            let bottom = rows.last().map_or(0.0, |(_, r)| r.bottom());
            assert!(bottom <= MIN_SIZE.Y - 8.0, "{section:?}: {bottom}");
            for (item, row) in rows {
                for part in parts(item, row, &v) {
                    assert!(part.right() <= MIN_SIZE.X - 16.0, "{item:?}");
                    assert!(part.bottom() <= row.bottom(), "{item:?}");
                    // Hit targets of at least 32 DIPs (IMPROVE.md 8.2).
                    assert!(part.h >= 32.0 && part.w >= 32.0, "{item:?}");
                }
            }
        }
        assert!(side_item(Section::ALL.len() - 1).bottom() <= MIN_SIZE.Y);
    }

    #[test]
    fn custom_color_parts() {
        assert_eq!(stop_part(1, 0), Some(StopPart::Color(0)));
        assert_eq!(stop_part(1, 1), Some(StopPart::Add));
        assert_eq!(stop_part(1, 2), None);
        assert_eq!(stop_part(2, 2), Some(StopPart::Add));
        assert_eq!(stop_part(2, 3), Some(StopPart::Remove));
        assert_eq!(stop_part(3, 3), Some(StopPart::Remove));
        assert_eq!(stop_part(3, 4), None);
    }

    #[test]
    fn text_on_the_accent_stays_readable() {
        for accent in [
            Rgb::new(0x8f, 0xa8, 0xff),
            Rgb::new(0x00, 0x5a, 0x9e),
            Rgb::new(0xff, 0xff, 0x00),
            Rgb::new(0x20, 0x20, 0x20),
        ] {
            assert!(
                background::contrast(on_accent(accent), accent) >= 4.5,
                "{accent:?}"
            );
        }
    }
}
