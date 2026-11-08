//! Settings: `%APPDATA%\Glint\config.toml` (CLAUDE.md section 10).
//!
//! - [`parse`] turns the file's text into a [`Config`]. Every field has a
//!   default, so a partial (or empty) file works. A wrong value is an error
//!   that names its line, and the caller keeps using the last good config.
//! - [`create_default`] writes the commented template on first run.
//! - [`edit`] changes the file the way Glint needs (pins, launch counts)
//!   while keeping the user's comments and formatting, and saves it
//!   atomically (temp file + rename), so the file is never half written.
//!
//! Nothing here stays in memory: the parsed TOML document is dropped right
//! after each load or save.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{Deserializer, Error as _};
use toml_edit::{Array, DocumentMut, Item, Table, Value};

pub use crate::theme::ThemeId;

/// A plain sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// "#rrggbb", as written in config.toml.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Parses "#rrggbb" (the leading # is optional).
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::new(byte(0)?, byte(2)?, byte(4)?))
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            D::Error::custom(format!(
                "\"{s}\" is not a color; use \"#rrggbb\", like \"#8fa8ff\""
            ))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundMode {
    Solid,
    Gradient,
    /// Frosted glass: blur of what's behind, tinted with the first color.
    Glass,
    /// Frosted glass tinted with the gradient.
    GlassGradient,
}

impl BackgroundMode {
    pub const ALL: [Self; 4] = [
        Self::Solid,
        Self::Gradient,
        Self::Glass,
        Self::GlassGradient,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Gradient => "gradient",
            Self::Glass => "glass",
            Self::GlassGradient => "glass-gradient",
        }
    }

    /// Also accepts the older names "acrylic" and "acrylic-gradient".
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "acrylic" => Some(Self::Glass),
            "acrylic-gradient" => Some(Self::GlassGradient),
            _ => Self::ALL.into_iter().find(|m| m.name() == s),
        }
    }

    /// True for the modes that blur what is behind the window.
    pub fn uses_backdrop(self) -> bool {
        matches!(self, Self::Glass | Self::GlassGradient)
    }

    pub fn uses_gradient(self) -> bool {
        matches!(self, Self::Gradient | Self::GlassGradient)
    }
}

impl<'de> Deserialize<'de> for BackgroundMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown mode \"{s}\"; use \"solid\", \"gradient\", \"glass\" or \"glass-gradient\""
            ))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Midnight,
    Aurora,
    Sunset,
    Ocean,
    Plain,
    Custom,
    /// The active theme's own colors (milestone 9).
    Theme,
}

impl Preset {
    pub const BUILT_IN: [Self; 5] = [
        Self::Midnight,
        Self::Aurora,
        Self::Sunset,
        Self::Ocean,
        Self::Plain,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Midnight => "midnight",
            Self::Aurora => "aurora",
            Self::Sunset => "sunset",
            Self::Ocean => "ocean",
            Self::Plain => "plain",
            Self::Custom => "custom",
            Self::Theme => "theme",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::BUILT_IN
            .into_iter()
            .chain([Self::Custom, Self::Theme])
            .find(|p| p.name() == s)
    }

    /// The preset's colors (CLAUDE.md section 6). `None` for custom and
    /// theme (those come from elsewhere).
    pub fn colors(self) -> Option<&'static [Rgb]> {
        const MIDNIGHT: [Rgb; 2] = [Rgb::new(0x1e, 0x1b, 0x3a), Rgb::new(0x14, 0x21, 0x3d)];
        const AURORA: [Rgb; 3] = [
            Rgb::new(0x0f, 0x3b, 0x3a),
            Rgb::new(0x1c, 0x23, 0x50),
            Rgb::new(0x3a, 0x1c, 0x4a),
        ];
        const SUNSET: [Rgb; 3] = [
            Rgb::new(0x4a, 0x1d, 0x3f),
            Rgb::new(0x3d, 0x21, 0x40),
            Rgb::new(0x1f, 0x1b, 0x3a),
        ];
        const OCEAN: [Rgb; 2] = [Rgb::new(0x0b, 0x2a, 0x3d), Rgb::new(0x0e, 0x3b, 0x4a)];
        const PLAIN: [Rgb; 1] = [Rgb::new(0x1e, 0x20, 0x26)];
        match self {
            Self::Midnight => Some(&MIDNIGHT),
            Self::Aurora => Some(&AURORA),
            Self::Sunset => Some(&SUNSET),
            Self::Ocean => Some(&OCEAN),
            Self::Plain => Some(&PLAIN),
            Self::Custom | Self::Theme => None,
        }
    }
}

impl<'de> Deserialize<'de> for Preset {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown preset \"{s}\"; use \"theme\", \"midnight\", \"aurora\", \"sunset\", \"ocean\", \"plain\" or \"custom\""
            ))
        })
    }
}

/// Caret, focus ring, focused search border and the seek bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    /// Follow the Windows accent color.
    System,
    /// The active theme's accent color (milestone 9).
    Theme,
    Color(Rgb),
}

impl<'de> Deserialize<'de> for Accent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let word = s.trim();
        if word.eq_ignore_ascii_case("system") {
            return Ok(Self::System);
        }
        if word.eq_ignore_ascii_case("theme") {
            return Ok(Self::Theme);
        }
        Rgb::parse(&s).map(Self::Color).ok_or_else(|| {
            D::Error::custom(format!(
                "\"{s}\" is not a color; use \"#rrggbb\", \"theme\" or \"system\""
            ))
        })
    }
}

/// Where the menu opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// What the theme wants (bottom, except nord: center).
    #[default]
    Theme,
    /// Above the taskbar, growing out of its edge.
    Bottom,
    /// In the middle of the monitor, growing out of its own center.
    Center,
}

impl Position {
    pub const ALL: [Self; 3] = [Self::Theme, Self::Bottom, Self::Center];

    pub fn name(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Bottom => "bottom",
            Self::Center => "center",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        let s = if s == "centre" {
            "center".to_string()
        } else {
            s
        };
        Self::ALL.into_iter().find(|p| p.name() == s)
    }

    /// Centered for this theme? The user's "bottom" / "center" always win.
    pub fn centered(self, theme: ThemeId) -> bool {
        match self {
            Self::Theme => theme.theme().centered,
            Self::Bottom => false,
            Self::Center => true,
        }
    }
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            D::Error::custom(format!(
                r#"unknown position "{s}"; use "theme", "bottom" or "center""#
            ))
        })
    }
}

/// An extra shortcut that toggles the menu, e.g. "Alt+Space".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// The Windows virtual-key code of the main key.
    pub vk: u16,
    /// As written in the file (for messages).
    pub text: String,
}

impl Hotkey {
    /// Parses "Ctrl+Alt+K", "Alt+Space", "Win+F1"... (case doesn't matter).
    /// Needs at least one of Ctrl / Alt / Win, or a function key, so a
    /// plain letter can't steal typing everywhere.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut key = Self {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk: 0,
            text: s.trim().to_string(),
        };
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let Some((last, mods)) = parts.split_last() else {
            return Err("empty hotkey".into());
        };
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => key.ctrl = true,
                "alt" => key.alt = true,
                "shift" => key.shift = true,
                "win" | "windows" => key.win = true,
                other => {
                    return Err(format!(
                        "unknown modifier \"{other}\" (use Ctrl, Alt, Shift or Win)"
                    ));
                }
            }
        }
        key.vk = vk_for(last).ok_or_else(|| format!("unknown key \"{last}\""))?;
        let is_f_key = (0x70..=0x87).contains(&key.vk);
        if !(key.ctrl || key.alt || key.win || is_f_key) {
            return Err("a hotkey needs Ctrl, Alt or Win (or an F key)".into());
        }
        Ok(key)
    }

    /// A hotkey from keys pressed in Settings' recorder, if it is one that
    /// `parse` accepts (so the file never gets a value it would reject).
    pub fn from_keys(ctrl: bool, alt: bool, shift: bool, win: bool, vk: u16) -> Option<Self> {
        let name = key_name(vk)?;
        let mut text = String::new();
        for (on, m) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (win, "Win")] {
            if on {
                text.push_str(m);
                text.push('+');
            }
        }
        text.push_str(&name);
        Self::parse(&text).ok()
    }
}

/// The name config.toml uses for a virtual-key code (the reverse of
/// `vk_for`). `None` for keys a hotkey can't use (modifiers alone...).
pub fn key_name(vk: u16) -> Option<String> {
    let c = u8::try_from(vk).ok()?;
    if c.is_ascii_uppercase() || c.is_ascii_digit() {
        return Some(char::from(c).to_string());
    }
    if (0x70..=0x87).contains(&vk) {
        return Some(format!("F{}", vk - 0x70 + 1));
    }
    let name = match vk {
        0x20 => "Space",
        0x0D => "Enter",
        0x09 => "Tab",
        0x08 => "Backspace",
        0x2D => "Insert",
        0x2E => "Delete",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0xC0 => "`",
        _ => return None,
    };
    Some(name.to_string())
}

/// The virtual-key code for a key name: A–Z, 0–9, F1–F24 and a few named
/// keys.
fn vk_for(name: &str) -> Option<u16> {
    let upper = name.to_ascii_uppercase();
    if let [c] = upper.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        // Letters and digits have the same code as their ASCII uppercase.
        return Some(u16::from(*c));
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    Some(match upper.as_str() {
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "`" | "BACKTICK" | "GRAVE" => 0xC0,
        _ => return None,
    })
}

fn hotkey<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Hotkey>, D::Error> {
    let s = String::deserialize(d)?;
    if s.trim().is_empty() {
        return Ok(None);
    }
    Hotkey::parse(&s)
        .map(Some)
        .map_err(|e| D::Error::custom(format!("hotkey \"{s}\": {e}")))
}

/// Reads a number and checks it is within `lo..=hi` (NaN fails too).
fn in_range<'de, D, T>(d: D, lo: T, hi: T) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + PartialOrd + fmt::Display + Copy,
{
    let v = T::deserialize(d)?;
    if (lo..=hi).contains(&v) {
        Ok(v)
    } else {
        Err(D::Error::custom(format!(
            "must be between {lo} and {hi}, not {v}"
        )))
    }
}

fn overlay<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    in_range(d, 0.0, 0.6)
}
fn unit<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    in_range(d, 0.0, 1.0)
}
fn angle<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    in_range(d, -360.0, 360.0)
}
/// The fewest and most grid columns.
pub const MIN_COLUMNS: usize = 3;
pub const MAX_COLUMNS: usize = 8;
/// The default number of columns (milestone 8: was 4).
pub const DEFAULT_COLUMNS: usize = 6;

/// 3 to 8 columns. Files from before milestone 8 could say 1 or 2: those
/// still load, raised to 3 (and logged), instead of being an error.
fn columns<'de, D: Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    let n = in_range(d, 1, MAX_COLUMNS)?;
    if n < MIN_COLUMNS {
        log_info!("config.toml: max_columns = {n} is below {MIN_COLUMNS}; using {MIN_COLUMNS}");
    }
    Ok(n.max(MIN_COLUMNS))
}
fn duration<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    in_range(d, 0, 3000)
}
fn stagger<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    in_range(d, 0, 500)
}
fn overshoot<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    in_range(d, 1.0, 2.0)
}

fn colors<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Rgb>, D::Error> {
    let colors = Vec::<Rgb>::deserialize(d)?;
    if (1..=3).contains(&colors.len()) {
        Ok(colors)
    } else {
        Err(D::Error::custom(format!(
            "needs 1 to 3 colors, not {}",
            colors.len()
        )))
    }
}

/// Pins, with repeats (in any letter case) removed.
fn pins<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let mut list = Vec::<String>::new();
    for pin in Vec::<String>::deserialize(d)? {
        let pin = pin.trim().to_string();
        if !pin.is_empty() && !list.iter().any(|p| p.eq_ignore_ascii_case(&pin)) {
            list.push(pin);
        }
    }
    Ok(list)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Background {
    pub mode: BackgroundMode,
    pub preset: Preset,
    /// Used when `preset` is `Custom`; 1–3 colors.
    #[serde(deserialize_with = "colors")]
    pub colors: Vec<Rgb>,
    /// Gradient direction in degrees (CSS convention: 0 = up, 90 = right).
    #[serde(deserialize_with = "angle")]
    pub angle: f32,
    /// Black overlay alpha, 0.0–0.6.
    #[serde(deserialize_with = "overlay")]
    pub overlay: f32,
    /// Glass modes: how strongly the color tints the glass.
    /// 0.0 = colorless glass, 1.0 = no see-through at all.
    #[serde(deserialize_with = "unit")]
    pub tint: f32,
    /// While music shows, replace the first color with the album cover's
    /// color (darkened for readability).
    pub tint_from_album: bool,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            mode: BackgroundMode::GlassGradient,
            tint: 0.35,
            preset: Preset::Theme,
            colors: vec![
                Rgb::new(0x2b, 0x1a, 0x4a),
                Rgb::new(0x1a, 0x23, 0x47),
                Rgb::new(0x0f, 0x2a, 0x3a),
            ],
            angle: 135.0,
            overlay: 0.2,
            tint_from_album: false,
        }
    }
}

impl Background {
    /// The 1–3 colors actually in use (the theme's, a preset's or custom).
    pub fn resolved_colors(&self, theme: ThemeId) -> Vec<Rgb> {
        let colors = match (self.preset, self.preset.colors()) {
            (Preset::Theme, _) => theme.theme().palette.background.to_vec(),
            (_, Some(c)) => c.to_vec(),
            (_, None) => self.colors.iter().copied().take(3).collect(),
        };
        if colors.is_empty() {
            Preset::Plain.colors().unwrap_or_default().to_vec()
        } else {
            colors
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Animation {
    #[serde(deserialize_with = "duration")]
    pub open_ms: u32,
    #[serde(deserialize_with = "duration")]
    pub close_ms: u32,
    /// Delay between one pinned icon's pop-in and the next.
    #[serde(deserialize_with = "stagger")]
    pub stagger_ms: u32,
    /// 2nd control-point y of the open easing (1.0 = no bounce).
    #[serde(deserialize_with = "overshoot")]
    pub overshoot: f32,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            open_ms: 340,
            close_ms: 170,
            stagger_ms: 18,
            overshoot: 1.25,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Music {
    /// false = never show the music card.
    pub enabled: bool,
}

impl Default for Music {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Written by Glint, not by the user (so unknown keys are fine here).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// App ID → how often it was launched from the menu.
    pub launch_counts: HashMap<String, u32>,
}

/// The pins on a fresh install (CLAUDE.md section 10). Pins that aren't
/// installed on this PC are left out of a new file, and skipped when the
/// menu is built.
pub const DEFAULT_PINS: [&str; 6] = [
    "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
    "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
    "Microsoft.Windows.Explorer",
    "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
    "Microsoft.Paint_8wekyb3d8bbwe!App",
    "Microsoft.Windows.Photos_8wekyb3d8bbwe!App",
];

/// The default accent color (#8fa8ff).
pub const DEFAULT_ACCENT: Rgb = Rgb::new(0x8f, 0xa8, 0xff);

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Pinned apps, as AppUserModelIDs, in grid order.
    #[serde(deserialize_with = "pins")]
    pub pinned: Vec<String>,
    /// Colors, shapes, font and layout (milestone 9). New files say
    /// tokyo-night (`Config::default`); a file without this line is from
    /// before themes, so it keeps the look it always had: glint.
    #[serde(default)]
    pub theme: ThemeId,
    /// Draw app icons in one color (the theme's text color).
    pub icon_tint: bool,
    /// Where the menu opens: "theme", "bottom" or "center".
    pub position: Position,
    pub accent: Accent,
    /// Extra shortcut that toggles the menu (`None` = Win key only).
    #[serde(deserialize_with = "hotkey")]
    pub hotkey: Option<Hotkey>,
    /// The grid never gets wider than this many columns (3–8).
    #[serde(deserialize_with = "columns")]
    pub max_columns: usize,
    /// App names under the pinned icons.
    pub show_names: bool,
    /// Start Glint when you sign in to Windows (the HKCU `Run` key).
    pub autostart: bool,
    pub background: Background,
    pub music: Music,
    pub animation: Animation,
    pub stats: Stats,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pinned: DEFAULT_PINS.map(String::from).to_vec(),
            theme: ThemeId::TokyoNight,
            icon_tint: false,
            position: Position::Theme,
            accent: Accent::Theme,
            hotkey: None,
            max_columns: DEFAULT_COLUMNS,
            show_names: true,
            autostart: false,
            background: Background::default(),
            music: Music::default(),
            animation: Animation::default(),
            stats: Stats::default(),
        }
    }
}

impl Config {
    /// Settings' "Reset to defaults": every setting back to its default,
    /// except the pinned apps, the launch counts and "Start with Windows".
    pub fn reset_keeping(&self) -> Self {
        Self {
            pinned: self.pinned.clone(),
            autostart: self.autostart,
            stats: self.stats.clone(),
            ..Self::default()
        }
    }
}

impl Accent {
    /// As written in config.toml: "system", "theme" or "#rrggbb".
    pub fn text(self) -> String {
        match self {
            Self::System => "system".to_string(),
            Self::Theme => "theme".to_string(),
            Self::Color(c) => c.hex(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What is wrong with config.toml, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// 1-based line, when known.
    pub line: Option<usize>,
    /// That line's text (shortened), so the message shows *what* is wrong,
    /// e.g. `tint = 1.5`.
    pub snippet: Option<String>,
    pub message: String,
}

/// Longest snippet shown, in characters.
const SNIPPET_CHARS: usize = 60;

impl ConfigError {
    fn from_span(text: &str, span: Option<std::ops::Range<usize>>, message: &str) -> Self {
        // Counted in bytes, so a position inside a multi-byte character
        // can't make this panic.
        let line = span.map(|s| {
            let end = s.start.min(text.len());
            text.as_bytes()[..end]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1
        });
        let snippet = line
            .and_then(|n| text.lines().nth(n - 1))
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                if l.chars().count() > SNIPPET_CHARS {
                    let cut: String = l.chars().take(SNIPPET_CHARS - 1).collect();
                    cut + "…"
                } else {
                    l.to_string()
                }
            });
        Self {
            line,
            snippet,
            message: message.trim().to_string(),
        }
    }

    fn io(what: &str, e: &std::io::Error) -> Self {
        Self {
            line: None,
            snippet: None,
            message: format!("{what}: {e}"),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, &self.snippet) {
            (Some(line), Some(snippet)) => write!(f, "line {line} ({snippet}): {}", self.message),
            (Some(line), None) => write!(f, "line {line}: {}", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// `%APPDATA%\Glint\config.toml`.
pub fn path() -> Option<PathBuf> {
    let dir = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(dir).join("Glint").join("config.toml"))
}

/// Turns the file's text into settings.
pub fn parse(text: &str) -> Result<Config, ConfigError> {
    // A BOM (some editors add one) is not valid TOML; skip it.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    toml_edit::de::from_str(text).map_err(|e| ConfigError::from_span(text, e.span(), e.message()))
}

/// Reads and parses the file. `Ok(None)` = there is no file.
pub fn load(path: &Path) -> Result<Option<Config>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::io("can't read config.toml", &e)),
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// The file written on first run: every option with a comment. `{PINS}` is
/// replaced by the default pins that exist on this PC.
const TEMPLATE: &str = r##"# Glint settings.
#
# The easy way to change these: right-click the Glint tray icon, then
# "Settings…". Or edit this file: Glint watches it, so when you save, the
# menu changes right away.
# If something here is wrong, Glint keeps the last good settings and tells
# you the line number (a tray notification, and the log in
# %LOCALAPPDATA%\Glint\log.txt). Delete this file to get the defaults back.

# Pinned apps, in grid order, as app IDs (AppUserModelIDs).
# The easy way to change them: right-click an app in the menu, then
# "Pin to Glint" or "Unpin from Glint". Apps that aren't installed are skipped.
pinned = [{PINS}]

# The look: "glint", "catppuccin", "nord", "tokyo-night" or "gruvbox".
# A theme brings its own colors, shapes and font; `accent = "theme"` and
# `preset = "theme"` below use the theme's colors.
theme = "tokyo-night"

# Draw app icons in one color (the theme's text color).
icon_tint = false

# Where the menu opens: "bottom" (above the taskbar), "center" (the middle
# of the screen), or "theme" (bottom; the nord theme likes center).
position = "theme"

# Color of the text caret, keyboard focus ring, search border and seek bar:
# "theme", "#rrggbb", or "system" to follow your Windows accent color.
accent = "theme"

# An extra shortcut that opens the menu, like "Alt+Space" or "Ctrl+Shift+S"
# (modifiers: Ctrl, Alt, Shift, Win). Empty = the Win key only.
hotkey = ""

# The pinned grid is never wider than this many icons (3 to 8).
max_columns = 6

# Show app names under the pinned icons.
show_names = true

# Start Glint when you sign in to Windows. You can also switch this with
# "Start with Windows" in the tray icon's menu.
autostart = false

[background]
# "solid", "gradient", "glass" (frosted glass) or "glass-gradient".
mode = "glass-gradient"
# Colors: "theme" (the theme's own), "midnight", "aurora", "sunset",
# "ocean", "plain", or "custom" to use `colors` below.
preset = "theme"
# Used when preset = "custom": 1 to 3 colors.
colors = ["#2b1a4a", "#1a2347", "#0f2a3a"]
# Gradient direction in degrees (0 = up, 90 = right).
angle = 135
# How much to darken the background so text stays readable (0.0 to 0.6).
# Glint raises it by itself if the colors are too bright for white text.
overlay = 0.2
# Glass modes: how strongly the colors tint the glass
# (0.0 = clear glass, 1.0 = you can't see through it).
tint = 0.35
# While music plays, tint the background with the album cover's color.
tint_from_album = false

[music]
# false = never show the music card.
enabled = true

[animation]
# How long opening and closing take, in milliseconds (0 to 3000).
open_ms = 340
close_ms = 170
# Delay between one pinned icon popping in and the next (0 to 500 ms).
stagger_ms = 18
# Bounce at the end of the open animation (1.0 = none, up to 2.0).
overshoot = 1.25

# Written by Glint, not by you: how often you launched each app from the
# menu (search ranking uses it). Safe to delete.
[stats.launch_counts]
"##;

/// The first-run file, with these pins.
pub fn default_text(pins: &[String]) -> String {
    let list: String = pins
        .iter()
        .map(|p| format!("\n  {},", Value::from(p.as_str())))
        .collect();
    let list = if list.is_empty() { list } else { list + "\n" };
    TEMPLATE.replace("{PINS}", &list)
}

/// Writes the commented default file (first run, or after it was deleted).
pub fn create_default(path: &Path, pins: &[String]) -> Result<(), ConfigError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ConfigError::io("can't create the Glint folder", &e))?;
    }
    write_atomic(path, &default_text(pins))
}

/// Changes the file with `change`, keeping everything else (comments,
/// spacing, the user's newest edits) as it is. Fails without writing if
/// the file can't be read or isn't valid TOML right now.
pub fn edit(path: &Path, change: impl FnOnce(&mut DocumentMut)) -> Result<(), ConfigError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| ConfigError::io("can't read config.toml", &e))?;
    let body = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut doc: DocumentMut = body
        .parse()
        .map_err(|e: toml_edit::TomlError| ConfigError::from_span(body, e.span(), e.message()))?;
    change(&mut doc);
    let new = doc.to_string();
    if new == body {
        return Ok(());
    }
    write_atomic(path, &new)
}

/// Writes to `config.toml.tmp`, flushes it to disk, then renames it over
/// `config.toml`. The rename replaces the file in one step, so a crash or
/// power cut leaves either the old file or the new one, never half of one.
fn write_atomic(path: &Path, text: &str) -> Result<(), ConfigError> {
    use std::io::Write;
    let tmp = path.with_extension("toml.tmp");
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // std's rename uses MoveFileExW(MOVEFILE_REPLACE_EXISTING).
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ConfigError::io("can't save config.toml", &e)
    })
}

/// Sets `pinned` to `pins`, changing the list in place: unpinned entries
/// are taken out, new ones are added at the end in the same style, and
/// everything else (comments, spacing) stays as the user wrote it.
pub fn set_pins(doc: &mut DocumentMut, pins: &[String]) {
    let wanted = |s: &str| pins.iter().any(|p| p.eq_ignore_ascii_case(s));
    let Some(array) = doc.get_mut("pinned").and_then(Item::as_array_mut) else {
        let mut array = Array::new();
        fill_pins(&mut array, pins);
        doc.insert("pinned", Item::Value(Value::Array(array)));
        return;
    };

    // Out with the ones no longer pinned (from the end, so indexes stay).
    for i in (0..array.len()).rev() {
        let keep = array.get(i).and_then(Value::as_str).is_some_and(wanted);
        if !keep {
            remove_keeping_comment(array, i);
        }
    }
    // In with the new ones, at the end.
    let indent = array
        .get(0)
        .and_then(|v| v.decor().prefix())
        .and_then(|p| p.as_str())
        .and_then(|p| p.rfind('\n').map(|n| p[n..].to_string()))
        .unwrap_or_else(|| "\n  ".to_string());
    let was_empty = array.is_empty();
    for pin in pins {
        let present = array
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(pin)));
        if !present {
            let mut v = Value::from(pin.as_str());
            v.decor_mut().set_prefix(indent.clone());
            array.push_formatted(v);
        }
    }
    if was_empty && !array.is_empty() {
        array.set_trailing_comma(true);
        array.set_trailing("\n");
    }
    if array.is_empty() {
        array.set_trailing_comma(false);
        array.set_trailing("");
    }

    // Our own changes only remove and append, so the order matches. If it
    // somehow doesn't (repeated entries in the file...), write the list
    // fresh in the right order.
    let same_order = array.len() == pins.len()
        && array
            .iter()
            .zip(pins)
            .all(|(v, p)| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(p)));
    if !same_order {
        array.clear();
        fill_pins(array, pins);
    }
}

/// Fills an empty array with `pins`, one per line.
fn fill_pins(array: &mut Array, pins: &[String]) {
    for pin in pins {
        let mut v = Value::from(pin.as_str());
        v.decor_mut().set_prefix("\n  ");
        array.push_formatted(v);
    }
    if !array.is_empty() {
        array.set_trailing_comma(true);
        array.set_trailing("\n");
    }
}

/// Removes entry `i`. In TOML, a comment at the end of a line inside a
/// list is stored with the *next* entry (`"a", # mine` keeps `# mine` in
/// the space before `"b"`), so that part of the removed entry's prefix is
/// handed on to whatever follows, instead of disappearing.
fn remove_keeping_comment(array: &mut Array, i: usize) {
    let removed = array.remove(i);
    let prefix = removed
        .decor()
        .prefix()
        .and_then(|p| p.as_str())
        .unwrap_or("");
    // The end of the line before the removed entry ("" if it was inline).
    let Some(n) = prefix.find('\n') else {
        return;
    };
    let line_end = &prefix[..=n];
    // The next entry's own end-of-line comment belonged to the removed
    // entry's line: replace it with ours.
    let rest_after_line = |s: &str| {
        s.find('\n')
            .map_or(String::new(), |n| s[n + 1..].to_string())
    };
    match array.get_mut(i) {
        Some(next) => {
            let own = next
                .decor()
                .prefix()
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            next.decor_mut()
                .set_prefix(format!("{line_end}{}", rest_after_line(&own)));
        }
        None => {
            let own = array.trailing().as_str().unwrap_or("").to_string();
            array.set_trailing(format!("{line_end}{}", rest_after_line(&own)));
        }
    }
}

/// Sets `autostart = on`, keeping the line's comments. Added (after the
/// other top-level settings) if the file doesn't have it yet.
pub fn set_autostart(doc: &mut DocumentMut, on: bool) {
    set_key(doc, None, "autostart", Value::from(on));
}

/// Sets `key` (in `[table]`, or at the top for `None`) to `value`. An
/// existing line keeps its spacing and comment; a missing one is added
/// (and a missing table too).
fn set_key(doc: &mut DocumentMut, table: Option<&str>, key: &str, value: Value) {
    let target: &mut dyn toml_edit::TableLike = match table {
        None => doc.as_table_mut(),
        Some(name) => {
            let item = doc.entry(name).or_insert_with(|| Item::Table(Table::new()));
            match item.as_table_like_mut() {
                Some(t) => t,
                None => return,
            }
        }
    };
    if let Some(old) = target.get_mut(key).and_then(Item::as_value_mut) {
        let decor = old.decor().clone();
        *old = value;
        *old.decor_mut() = decor;
    } else {
        target.insert(key, Item::Value(value));
    }
}

/// A number with at most `places` decimals, so the file says `0.25` and
/// not `0.25000000372529`. Whole numbers are written without a dot.
fn number(x: f32, places: i32) -> Value {
    let f = 10f64.powi(places);
    let r = (f64::from(x) * f).round() / f;
    if r.fract() == 0.0 {
        Value::from(r as i64)
    } else {
        Value::from(r)
    }
}

/// Writes every setting that differs between `old` (what the file holds)
/// and `new` (what Settings shows). Unchanged lines stay exactly as they
/// are. Pins and launch counts have their own functions above.
pub fn save_changes(doc: &mut DocumentMut, old: &Config, new: &Config) {
    let mut put = |changed: bool, table: Option<&str>, key: &str, value: Value| {
        if changed {
            set_key(doc, table, key, value);
        }
    };
    put(
        old.theme != new.theme,
        None,
        "theme",
        Value::from(new.theme.name()),
    );
    put(
        old.icon_tint != new.icon_tint,
        None,
        "icon_tint",
        Value::from(new.icon_tint),
    );
    put(
        old.position != new.position,
        None,
        "position",
        Value::from(new.position.name()),
    );
    put(
        old.accent != new.accent,
        None,
        "accent",
        Value::from(new.accent.text()),
    );
    let hotkey = new
        .hotkey
        .as_ref()
        .map_or(String::new(), |k| k.text.clone());
    put(
        old.hotkey != new.hotkey,
        None,
        "hotkey",
        Value::from(hotkey),
    );
    put(
        old.max_columns != new.max_columns,
        None,
        "max_columns",
        Value::from(new.max_columns as i64),
    );
    put(
        old.show_names != new.show_names,
        None,
        "show_names",
        Value::from(new.show_names),
    );
    put(
        old.autostart != new.autostart,
        None,
        "autostart",
        Value::from(new.autostart),
    );

    let (ob, nb) = (&old.background, &new.background);
    let bg = Some("background");
    put(ob.mode != nb.mode, bg, "mode", Value::from(nb.mode.name()));
    put(
        ob.preset != nb.preset,
        bg,
        "preset",
        Value::from(nb.preset.name()),
    );
    let colors: Array = nb.colors.iter().map(|c| c.hex()).collect();
    put(ob.colors != nb.colors, bg, "colors", Value::Array(colors));
    put(ob.angle != nb.angle, bg, "angle", number(nb.angle, 0));
    put(
        ob.overlay != nb.overlay,
        bg,
        "overlay",
        number(nb.overlay, 2),
    );
    put(ob.tint != nb.tint, bg, "tint", number(nb.tint, 2));
    put(
        ob.tint_from_album != nb.tint_from_album,
        bg,
        "tint_from_album",
        Value::from(nb.tint_from_album),
    );

    put(
        old.music.enabled != new.music.enabled,
        Some("music"),
        "enabled",
        Value::from(new.music.enabled),
    );

    let (oa, na) = (&old.animation, &new.animation);
    let anim = Some("animation");
    put(
        oa.open_ms != na.open_ms,
        anim,
        "open_ms",
        Value::from(i64::from(na.open_ms)),
    );
    put(
        oa.close_ms != na.close_ms,
        anim,
        "close_ms",
        Value::from(i64::from(na.close_ms)),
    );
    put(
        oa.stagger_ms != na.stagger_ms,
        anim,
        "stagger_ms",
        Value::from(i64::from(na.stagger_ms)),
    );
    put(
        oa.overshoot != na.overshoot,
        anim,
        "overshoot",
        number(na.overshoot, 2),
    );
}

/// Writes the launch counts into `[stats.launch_counts]`, one line per app.
pub fn set_launch_counts(doc: &mut DocumentMut, counts: &HashMap<String, u32>) {
    let stats = doc.entry("stats").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    let Some(stats) = stats.as_table_like_mut() else {
        return;
    };
    // An inline `launch_counts = {}` becomes a normal table: one app per line.
    let needs_table = !stats.get("launch_counts").is_some_and(Item::is_table);
    if needs_table {
        stats.insert("launch_counts", Item::Table(Table::new()));
    }
    let Some(table) = stats.get_mut("launch_counts").and_then(Item::as_table_mut) else {
        return;
    };
    table.retain(|k, _| counts.contains_key(k));
    let mut ids: Vec<&String> = counts.keys().collect();
    ids.sort_by_key(|id| id.to_lowercase());
    for id in ids {
        let n = i64::from(counts[id]);
        if table.get(id).and_then(Item::as_integer) != Some(n) {
            table.insert(id, toml_edit::value(n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pins(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgb::parse("#8fa8ff"), Some(Rgb::new(0x8f, 0xa8, 0xff)));
        assert_eq!(Rgb::parse("1e2026"), Some(Rgb::new(0x1e, 0x20, 0x26)));
        assert_eq!(Rgb::parse("#123"), None);
        assert_eq!(Rgb::parse("#zzzzzz"), None);
    }

    #[test]
    fn presets_and_custom_colors() {
        let glint = ThemeId::Glint;
        let mut bg = Background::default();
        // "theme" with glint = aurora, the look before themes existed.
        assert_eq!(bg.resolved_colors(glint), Preset::Aurora.colors().unwrap());
        assert_eq!(
            bg.resolved_colors(ThemeId::Gruvbox),
            crate::theme::GRUVBOX.palette.background
        );
        bg.preset = Preset::Sunset;
        assert_eq!(
            bg.resolved_colors(ThemeId::Nord),
            Preset::Sunset.colors().unwrap()
        );
        bg.preset = Preset::Custom;
        assert_eq!(bg.resolved_colors(glint)[0], Rgb::new(0x2b, 0x1a, 0x4a));
        bg.colors.clear();
        assert_eq!(bg.resolved_colors(glint), [Rgb::new(0x1e, 0x20, 0x26)]);
    }

    #[test]
    fn positions() {
        let c = parse(r#"position = "center""#).unwrap();
        assert_eq!(c.position, Position::Center);
        assert_eq!(parse("").unwrap().position, Position::Theme);
        let centre = parse(r#"position = "Centre""#).unwrap();
        assert_eq!(centre.position, Position::Center);
        assert!(parse(r#"position = "left""#).is_err());
        // "theme": nord is centered, the others sit at the bottom; the
        // user's choice always wins.
        assert!(Position::Theme.centered(ThemeId::Nord));
        assert!(!Position::Theme.centered(ThemeId::TokyoNight));
        assert!(!Position::Bottom.centered(ThemeId::Nord));
        assert!(Position::Center.centered(ThemeId::Glint));
        let text = default_text(&pins(&["a"]));
        let old = parse(&text).unwrap();
        let mut new = old.clone();
        new.position = Position::Bottom;
        let mut doc: DocumentMut = text.parse().unwrap();
        save_changes(&mut doc, &old, &new);
        assert!(doc.to_string().contains("position = \"bottom\"\n"), "{doc}");
        assert_eq!(parse(&doc.to_string()).unwrap(), new);
    }

    #[test]
    fn themes_and_theme_colors() {
        let c = parse("theme = \"gruvbox\"\nicon_tint = true\naccent = \"theme\"\n[background]\npreset = \"theme\"").unwrap();
        assert_eq!(c.theme, ThemeId::Gruvbox);
        assert!(c.icon_tint);
        assert_eq!(c.accent, Accent::Theme);
        assert_eq!(c.background.preset, Preset::Theme);
        // Old files (no theme line) are glint with their own colors.
        let old = parse("accent = \"#8fa8ff\"\n[background]\npreset = \"aurora\"").unwrap();
        assert_eq!(old.theme, ThemeId::Glint);
        assert!(!old.icon_tint);
        assert_eq!(old.accent, Accent::Color(DEFAULT_ACCENT));
        let e = parse("theme = \"dracula\"").unwrap_err();
        assert_eq!(e.line, Some(1));
        assert!(e.message.contains("tokyo-night"), "{e}");
        // Saving a theme change writes only those lines.
        let text = default_text(&pins(&["a"]));
        let old = parse(&text).unwrap();
        let mut new = old.clone();
        new.theme = ThemeId::TokyoNight;
        new.icon_tint = true;
        let mut doc: DocumentMut = text.parse().unwrap();
        save_changes(&mut doc, &old, &new);
        let out = doc.to_string();
        assert!(out.contains("theme = \"tokyo-night\"\n"), "{out}");
        assert!(out.contains("icon_tint = true\n"), "{out}");
        assert_eq!(parse(&out).unwrap(), new);
    }

    #[test]
    fn the_template_is_the_defaults() {
        let text = default_text(&pins(&DEFAULT_PINS));
        assert_eq!(parse(&text).unwrap(), Config::default());
        let none = parse(&default_text(&[])).unwrap();
        assert!(none.pinned.is_empty());
    }

    #[test]
    fn missing_fields_use_defaults() {
        // (An empty file is an old one: glint. New files say tokyo-night.)
        let empty = parse("").unwrap();
        assert_eq!(empty.theme, ThemeId::Glint);
        assert_eq!(
            empty,
            Config {
                theme: ThemeId::Glint,
                ..Config::default()
            }
        );
        let c = parse("[background]\nmode = \"solid\"\n").unwrap();
        assert_eq!(c.background.mode, BackgroundMode::Solid);
        assert_eq!(c.background.overlay, 0.2);
        assert_eq!(c.animation, Animation::default());
        // Integers are fine where a decimal is expected.
        let c = parse("[background]\noverlay = 0\nangle = 90\n").unwrap();
        assert_eq!(c.background.overlay, 0.0);
        assert_eq!(c.background.angle, 90.0);
    }

    #[test]
    fn glass_modes_and_old_names() {
        let mode = |m: &str| parse(&format!("[background]\nmode = \"{m}\"")).unwrap();
        assert_eq!(mode("glass").background.mode, BackgroundMode::Glass);
        assert_eq!(mode("acrylic").background.mode, BackgroundMode::Glass);
        assert_eq!(
            mode("acrylic-gradient").background.mode,
            BackgroundMode::GlassGradient
        );
    }

    #[test]
    fn bad_values_name_their_line() {
        let err = |text: &str| parse(text).unwrap_err();
        let e = err("accent = \"#8fa8ff\"\n\n[background]\nmode = \"glasss\"\n");
        assert_eq!(e.line, Some(4), "{e}");
        assert!(e.message.contains("glasss"), "{e}");

        let e = err("[background]\n  overlay = 0.9  \n");
        assert_eq!(e.line, Some(2));
        assert_eq!(e.snippet.as_deref(), Some("overlay = 0.9"));
        assert!(e.to_string().starts_with("line 2 (overlay = 0.9): "), "{e}");
        assert_eq!(err("[background]\ntint = -1").line, Some(2));
        assert_eq!(err("[background]\ncolors = []").line, Some(2));
        assert_eq!(err("[background]\ncolors = [\"#12\"]").line, Some(2));
        assert_eq!(err("max_columns = 0").line, Some(1));
        assert_eq!(err("\n\n[animation]\nopen_ms = -5").line, Some(4));
        assert_eq!(err("[animation]\novershoot = 3.0").line, Some(2));
        assert_eq!(err("accent = \"blue\"").line, Some(1));
        assert_eq!(err("hotkey = \"Alt+Spcae\"").line, Some(1));
        assert_eq!(err("[music]\nenabled = \"yes\"").line, Some(2));
        // Syntax errors too.
        assert!(err("pinned = [\n\"a\",\nmode = \n").line.is_some());
        assert_eq!(err("[background\nmode = \"solid\"").line, Some(1));
    }

    #[test]
    fn unknown_keys_are_errors() {
        let e = parse("[background]\nmode = \"solid\"\noverlai = 0.3\n").unwrap_err();
        assert_eq!(e.line, Some(3), "{e}");
        assert!(e.message.contains("overlai"), "{e}");
        // ...except in [stats], which Glint writes.
        assert!(parse("[stats]\nsomething_new = 1\n").is_ok());
    }

    #[test]
    fn accent_and_hotkey() {
        let c = parse("accent = \"system\"\nhotkey = \"Alt+Space\"").unwrap();
        assert_eq!(c.accent, Accent::System);
        let k = c.hotkey.unwrap();
        assert!(k.alt && !k.ctrl && !k.shift && !k.win);
        assert_eq!(k.vk, 0x20);
        assert_eq!(parse("hotkey = \"  \"").unwrap().hotkey, None);

        let k = Hotkey::parse("ctrl+shift+s").unwrap();
        assert!(k.ctrl && k.shift && k.vk == u16::from(b'S'));
        assert_eq!(Hotkey::parse("Win+F1").unwrap().vk, 0x70);
        assert_eq!(Hotkey::parse("F24").unwrap().vk, 0x87);
        assert!(Hotkey::parse("S").is_err()); // would steal typing
        assert!(Hotkey::parse("Shift+S").is_err());
        assert!(Hotkey::parse("Hyper+S").is_err());
        assert!(Hotkey::parse("Alt+").is_err());
        assert!(Hotkey::parse("Alt+F25").is_err());
    }

    #[test]
    fn columns_default_to_6_and_old_small_values_become_3() {
        assert_eq!(Config::default().max_columns, 6);
        assert_eq!(parse("max_columns = 1").unwrap().max_columns, 3);
        assert_eq!(parse("max_columns = 2").unwrap().max_columns, 3);
        assert_eq!(parse("max_columns = 4").unwrap().max_columns, 4);
        assert_eq!(parse("max_columns = 8").unwrap().max_columns, 8);
        assert_eq!(parse("max_columns = 9").unwrap_err().line, Some(1));
        assert!(Config::default().show_names);
        assert!(!parse("show_names = false").unwrap().show_names);
    }

    #[test]
    fn hotkeys_from_recorded_keys() {
        let k = Hotkey::from_keys(false, true, false, false, 0x20).unwrap();
        assert_eq!(k.text, "Alt+Space");
        assert_eq!(Hotkey::parse(&k.text).unwrap(), k);
        let k = Hotkey::from_keys(true, false, true, false, u16::from(b'K')).unwrap();
        assert_eq!(k.text, "Ctrl+Shift+K");
        assert_eq!(
            Hotkey::from_keys(false, false, false, false, 0x70)
                .unwrap()
                .text,
            "F1"
        );
        // Not allowed: a plain letter, Shift+letter, a modifier key alone.
        assert!(Hotkey::from_keys(false, false, false, false, u16::from(b'K')).is_none());
        assert!(Hotkey::from_keys(false, false, true, false, u16::from(b'K')).is_none());
        assert!(Hotkey::from_keys(true, false, false, false, 0x11).is_none());
        // Every key name reads back as the same key.
        for vk in 0u16..=0xFF {
            if let Some(name) = key_name(vk) {
                assert_eq!(vk_for(&name), Some(vk), "{name}");
            }
        }
    }

    #[test]
    fn reset_keeps_pins_counts_and_autostart() {
        let mut c = parse(
            "pinned = [\"a\"]\nautostart = true\nmax_columns = 3\n[stats.launch_counts]\na = 2\n",
        )
        .unwrap();
        c.background.overlay = 0.5;
        let r = c.reset_keeping();
        assert_eq!(r.pinned, ["a"]);
        assert!(r.autostart);
        assert_eq!(r.stats.launch_counts["a"], 2);
        assert_eq!(r.max_columns, 6);
        assert_eq!(r.background, Background::default());
    }

    #[test]
    fn saving_changes_touches_only_changed_lines() {
        let text = default_text(&pins(&["a"]));
        let old = parse(&text).unwrap();
        let mut new = old.clone();
        new.max_columns = 8;
        new.show_names = false;
        new.accent = Accent::System;
        new.hotkey = Hotkey::parse("Alt+Space").ok();
        new.background.mode = BackgroundMode::Solid;
        new.background.preset = Preset::Custom;
        new.background.colors = vec![Rgb::new(1, 2, 3), Rgb::new(0xff, 0, 0x10)];
        new.background.angle = 90.0;
        new.background.overlay = 0.25;
        new.background.tint = 0.4;
        new.music.enabled = false;
        new.animation.open_ms = 500;
        new.animation.overshoot = 1.4;
        let mut doc: DocumentMut = text.parse().unwrap();
        save_changes(&mut doc, &old, &new);
        let out = doc.to_string();
        assert_eq!(parse(&out).unwrap(), new, "{out}");
        assert!(out.contains("overlay = 0.25\n"), "{out}");
        assert!(out.contains("overshoot = 1.4\n"), "{out}");
        assert!(out.contains("angle = 90\n"), "{out}");
        assert!(out.contains("colors = [\"#010203\", \"#ff0010\"]"), "{out}");
        // Comments stay; untouched lines are byte for byte the same.
        assert!(out.contains("# How much to darken the background"), "{out}");
        assert!(out.contains("close_ms = 170\n"), "{out}");
        // Nothing changed: nothing written.
        let mut doc: DocumentMut = text.parse().unwrap();
        save_changes(&mut doc, &old, &old);
        assert_eq!(doc.to_string(), text);
        // A file missing the lines and tables still gets them.
        // (`old` is what that file holds, as in the app.)
        let mut doc: DocumentMut = "pinned = []\n".parse().unwrap();
        save_changes(&mut doc, &parse("pinned = []").unwrap(), &new);
        let mut want = new.clone();
        want.pinned.clear();
        assert_eq!(parse(&doc.to_string()).unwrap(), want, "{doc}");
    }

    #[test]
    fn repeated_pins_are_dropped() {
        let c = parse("pinned = [\"A\", \"b\", \"a\", \" \"]").unwrap();
        assert_eq!(c.pinned, ["A", "b"]);
    }

    #[test]
    fn launch_counts_load() {
        let c = parse("[stats.launch_counts]\n\"Microsoft.Windows.Explorer\" = 3\n").unwrap();
        assert_eq!(c.stats.launch_counts["Microsoft.Windows.Explorer"], 3);
        let c = parse("[stats]\nlaunch_counts = { \"x!App\" = 2 }\n").unwrap();
        assert_eq!(c.stats.launch_counts["x!App"], 2);
    }

    #[test]
    fn editing_pins_keeps_comments() {
        let text = "# my comment\npinned = [\n  \"a\", # first\n  \"b\",\n] # after\n\n[background]\n# keep me\nmode = \"solid\"\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        set_pins(&mut doc, &pins(&["a", "c"]));
        let out = doc.to_string();
        assert!(out.contains("# my comment"), "{out}");
        assert!(out.contains("# first"), "{out}");
        assert!(out.contains("# after"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
        assert!(!out.contains("\"b\""), "{out}");
        assert_eq!(parse(&out).unwrap().pinned, ["a", "c"]);

        // Unpinning everything, then pinning into an empty list.
        set_pins(&mut doc, &[]);
        assert!(parse(&doc.to_string()).unwrap().pinned.is_empty());
        set_pins(&mut doc, &pins(&["z"]));
        assert_eq!(parse(&doc.to_string()).unwrap().pinned, ["z"]);

        // In the real template: same style as the lines already there.
        let mut doc: DocumentMut = default_text(&pins(&["a", "b"])).parse().unwrap();
        set_pins(&mut doc, &pins(&["a", "b", "c"]));
        assert!(
            doc.to_string()
                .contains("pinned = [\n  \"a\",\n  \"b\",\n  \"c\",\n]\n"),
            "{doc}"
        );
        // Removing the last entry keeps the comment on the line before.
        let mut doc: DocumentMut = "pinned = [\n  \"a\", # mine\n  \"b\", # bee\n]\n"
            .parse()
            .unwrap();
        set_pins(&mut doc, &pins(&["a"]));
        assert_eq!(doc.to_string(), "pinned = [\n  \"a\", # mine\n]\n");

        // No `pinned` line at all yet.
        let mut doc: DocumentMut = "accent = \"system\"\n".parse().unwrap();
        set_pins(&mut doc, &pins(&["q"]));
        assert_eq!(parse(&doc.to_string()).unwrap().pinned, ["q"]);
    }

    #[test]
    fn editing_launch_counts() {
        let text = default_text(&pins(&["a"]));
        let mut doc: DocumentMut = text.parse().unwrap();
        let mut counts = HashMap::new();
        counts.insert(
            "{6D809377-6AF0-444B-8957-A3773F02200E}\\App\\x.exe".to_string(),
            4,
        );
        counts.insert("Microsoft.Windows.Explorer".to_string(), 1);
        set_launch_counts(&mut doc, &counts);
        let out = doc.to_string();
        assert!(out.contains("Safe to delete."), "{out}");
        assert_eq!(parse(&out).unwrap().stats.launch_counts, counts);
        // Again with the same counts: nothing changes.
        set_launch_counts(&mut doc, &counts);
        assert_eq!(doc.to_string(), out);

        // From an inline table, and from no [stats] at all.
        for start in ["[stats]\nlaunch_counts = {}\n", "pinned = []\n"] {
            let mut doc: DocumentMut = start.parse().unwrap();
            set_launch_counts(&mut doc, &counts);
            assert_eq!(parse(&doc.to_string()).unwrap().stats.launch_counts, counts);
        }
    }

    #[test]
    fn editing_autostart() {
        // The template has the line: only the value changes, comments stay.
        let text = default_text(&pins(&["a"]));
        let mut doc: DocumentMut = text.parse().unwrap();
        set_autostart(&mut doc, true);
        let out = doc.to_string();
        assert!(out.contains("autostart = true\n"), "{out}");
        assert!(out.contains("# Start Glint when you sign in"), "{out}");
        assert!(parse(&out).unwrap().autostart);
        assert_eq!(out.replace("autostart = true", "autostart = false"), text);

        // An older file without the line: it is added and still parses.
        let mut doc: DocumentMut = "accent = \"system\"\n\n[music]\nenabled = true\n"
            .parse()
            .unwrap();
        set_autostart(&mut doc, true);
        let c = parse(&doc.to_string()).unwrap();
        assert!(c.autostart && c.music.enabled);
        assert!(!parse("").unwrap().autostart, "off by default");
    }
}
