//! Themes (IMPROVE.md milestone 9): a color palette, shape values (corner
//! radii, border widths), a font and a layout, bundled under one name.
//!
//! Themes are plain constants: switching costs no memory, and only the
//! active theme's colors are ever turned into brushes. The user's
//! Appearance settings still win: `preset = "theme"` / `accent = "theme"`
//! mean "use the theme's colors", anything else overrides them.
//!
//! Palettes come from each project's public, MIT-licensed definition:
//! Catppuccin (Mocha), Nord, Tokyo Night and Gruvbox (dark).

use serde::Deserialize;
use serde::de::{Deserializer, Error as _};

use crate::config::Rgb;

/// Which theme, as written in config.toml (`theme = "gruvbox"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeId {
    #[default]
    Glint,
    Catppuccin,
    Nord,
    TokyoNight,
    Gruvbox,
}

impl ThemeId {
    pub const ALL: [Self; 5] = [
        Self::Glint,
        Self::Catppuccin,
        Self::Nord,
        Self::TokyoNight,
        Self::Gruvbox,
    ];

    /// As written in config.toml.
    pub fn name(self) -> &'static str {
        match self {
            Self::Glint => "glint",
            Self::Catppuccin => "catppuccin",
            Self::Nord => "nord",
            Self::TokyoNight => "tokyo-night",
            Self::Gruvbox => "gruvbox",
        }
    }

    /// As shown in Settings.
    pub fn title(self) -> &'static str {
        match self {
            Self::Glint => "Glint",
            Self::Catppuccin => "Catppuccin",
            Self::Nord => "Nord",
            Self::TokyoNight => "Tokyo Night",
            Self::Gruvbox => "Gruvbox",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        // "tokyonight" and "tokyo_night" are common spellings too.
        let s = s.replace('_', "-");
        if s == "tokyonight" {
            return Some(Self::TokyoNight);
        }
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn theme(self) -> &'static Theme {
        match self {
            Self::Glint => &GLINT,
            Self::Catppuccin => &CATPPUCCIN,
            Self::Nord => &NORD,
            Self::TokyoNight => &TOKYO_NIGHT,
            Self::Gruvbox => &GRUVBOX,
        }
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown theme \"{s}\"; use \"glint\", \"catppuccin\", \"nord\", \"tokyo-night\" or \"gruvbox\""
            ))
        })
    }
}

/// How the menu is arranged (IMPROVE.md 9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// Search → pinned grid → music card → footer.
    Grid,
    /// Rofi style: a list of apps, an icon-only pinned strip, a bottom bar.
    List,
    /// A left rail, a greeting and a big clock.
    Sidebar,
}

/// What sits behind each app icon (IMPROVE.md 9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconTile {
    None,
    /// A soft, very round square.
    Squircle,
    Rounded,
    /// A tile with a thin border in one of the theme's accent colors.
    Bordered,
    Sharp,
}

/// The right side of the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterStyle {
    /// Gear + power button (opens the power strip).
    Power,
    /// Gear + Lock / Restart / Shut down as separate round-square buttons.
    Buttons,
    /// Gear + `[ lock ]` `[ restart ]` `[ off ]` as text.
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Segoe UI Variable.
    Segoe,
    /// Cascadia Mono (ships with Windows 11), else Consolas.
    Mono,
}

/// A color with an alpha (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub color: Rgb,
    pub alpha: f32,
}

const fn paint(hex: u32, alpha: f32) -> Paint {
    Paint {
        color: rgb(hex),
        alpha,
    }
}

const fn rgb(hex: u32) -> Rgb {
    Rgb::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const WHITE: u32 = 0xffffff;
const BLACK: u32 = 0x000000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The background colors when `preset = "theme"` (1–3).
    pub background: &'static [Rgb],
    pub text: Rgb,
    pub subtext: Rgb,
    /// When `accent = "theme"`: caret, focus ring, focused search border.
    pub accent: Rgb,
    /// Text drawn on an accent or danger fill.
    pub on_accent: Rgb,
    /// The menu's border (non-glass modes).
    pub border: Paint,
    /// Search box fill and its border when not focused.
    pub field: Paint,
    pub field_border: Paint,
    /// Music card.
    pub card: Paint,
    pub card_border: Paint,
    /// Footer fill and the divider above it.
    pub footer: Paint,
    pub divider: Paint,
    /// Mouse hover, and the selected search result.
    pub hover: Paint,
    pub selected: Paint,
    /// Power strip and right-click menu.
    pub popup: Paint,
    pub popup_border: Paint,
    /// Small fills: the "Switch source" pill, Cancel, a missing cover.
    pub chip: Paint,
    /// Behind each app icon (see `IconTile`).
    pub tile: Paint,
    /// Bordered tiles cycle through these.
    pub tile_borders: &'static [Rgb],
    /// The seek bar's track, and its fill (`None` = the accent).
    pub track: Paint,
    pub progress: Option<Rgb>,
    /// A filled circle behind the play button (`None` = no circle).
    pub play: Option<Rgb>,
    /// Restart / Shut down confirm, and the "off" footer button.
    pub danger: Rgb,
    /// Icon colors of the footer's Lock / Restart buttons (Buttons style).
    pub lock: Rgb,
    pub restart: Rgb,
    /// The `❯` before the search text (terminal style).
    pub prompt: Rgb,
    /// A hard shadow under the menu, offset (8, 8), no blur.
    pub shadow: Option<Rgb>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub menu_radius: f32,
    /// The menu's border width (non-glass modes).
    pub menu_border: f32,
    /// A radius ≥ half the height makes a pill.
    pub search_radius: f32,
    pub search_border: f32,
    /// Hover and focus ring around grid cells.
    pub cell_radius: f32,
    pub tile_radius: f32,
    /// Footer buttons, music buttons, result rows.
    pub button_radius: f32,
    pub card_radius: f32,
    pub popup_radius: f32,
    pub cover_radius: f32,
    /// The footer's divider.
    pub divider: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub id: ThemeId,
    pub layout: LayoutKind,
    pub palette: Palette,
    pub shape: Shape,
    pub font: Family,
    pub tile: IconTile,
    pub footer: FooterStyle,
    /// Terminal look (gruvbox): a title strip with a clock, a `❯` prompt
    /// with a block caret, lowercase labels, a `── pinned ──` divider and
    /// a text-style music row.
    pub terminal: bool,
}

impl Theme {
    /// The seek bar's fill color.
    pub fn progress(&self, accent: Rgb) -> Rgb {
        self.palette.progress.unwrap_or(accent)
    }

    /// The pill radius for a box `h` tall.
    pub fn search_radius(&self, h: f32) -> f32 {
        self.shape.search_radius.min(h / 2.0)
    }
}

// ---------------------------------------------------------------------------
// The themes
// ---------------------------------------------------------------------------

/// Glint's own look (milestones 1–8): unchanged.
pub const GLINT: Theme = Theme {
    id: ThemeId::Glint,
    layout: LayoutKind::Grid,
    palette: Palette {
        // The "aurora" preset.
        background: &[rgb(0x0f3b3a), rgb(0x1c2350), rgb(0x3a1c4a)],
        text: rgb(0xf4f4f5),
        subtext: rgb(0xd4d4d8),
        accent: rgb(0x8fa8ff),
        on_accent: rgb(0x10101a),
        border: paint(WHITE, 0.10),
        field: paint(BLACK, 0.35),
        field_border: paint(WHITE, 0.14),
        card: paint(BLACK, 0.28),
        card_border: paint(WHITE, 0.08),
        footer: paint(BLACK, 0.22),
        divider: paint(WHITE, 0.08),
        hover: paint(WHITE, 0.08),
        selected: paint(WHITE, 0.13),
        popup: paint(0x1c1c22, 0.96),
        popup_border: paint(WHITE, 0.10),
        chip: paint(WHITE, 0.10),
        tile: paint(BLACK, 0.0),
        tile_borders: &[],
        track: paint(WHITE, 0.20),
        progress: None,
        play: None,
        danger: rgb(0xc42b1c),
        lock: rgb(0xf4f4f5),
        restart: rgb(0xf4f4f5),
        prompt: rgb(0x8fa8ff),
        shadow: None,
    },
    shape: Shape {
        menu_radius: 18.0,
        menu_border: 1.0,
        search_radius: 10.0,
        search_border: 1.0,
        cell_radius: 12.0,
        tile_radius: 0.0,
        button_radius: 10.0,
        card_radius: 14.0,
        popup_radius: 14.0,
        cover_radius: 8.0,
        divider: 1.0,
    },
    font: Family::Segoe,
    tile: IconTile::None,
    footer: FooterStyle::Power,
    terminal: false,
};

/// Catppuccin Mocha: soft and cozy.
pub const CATPPUCCIN: Theme = {
    const BASE: u32 = 0x1e1e2e;
    const MANTLE: u32 = 0x181825;
    const CRUST: u32 = 0x11111b;
    const SURFACE0: u32 = 0x313244;
    const SURFACE1: u32 = 0x45475a;
    const MAUVE: u32 = 0xcba6f7;
    Theme {
        id: ThemeId::Catppuccin,
        layout: LayoutKind::Grid,
        palette: Palette {
            background: &[rgb(BASE), rgb(MANTLE), rgb(CRUST)],
            text: rgb(0xcdd6f4),
            subtext: rgb(0xa6adc8),
            accent: rgb(MAUVE),
            on_accent: rgb(CRUST),
            border: paint(SURFACE0, 1.0),
            field: paint(SURFACE0, 0.70),
            field_border: paint(MAUVE, 0.55),
            card: paint(SURFACE0, 0.60),
            card_border: paint(SURFACE1, 0.60),
            footer: paint(CRUST, 0.45),
            divider: paint(SURFACE0, 1.0),
            hover: paint(SURFACE1, 0.55),
            selected: paint(SURFACE1, 0.80),
            popup: paint(MANTLE, 0.97),
            popup_border: paint(SURFACE1, 1.0),
            chip: paint(SURFACE1, 0.60),
            tile: paint(SURFACE0, 0.70),
            tile_borders: &[],
            track: paint(SURFACE1, 1.0),
            progress: Some(rgb(0xf5c2e7)), // pink
            play: Some(rgb(MAUVE)),
            danger: rgb(0xf38ba8),  // red
            lock: rgb(0xf9e2af),    // yellow
            restart: rgb(0x89b4fa), // blue
            prompt: rgb(0xa6e3a1),  // green
            shadow: None,
        },
        shape: Shape {
            menu_radius: 24.0,
            menu_border: 1.0,
            search_radius: 999.0,
            search_border: 2.0,
            cell_radius: 16.0,
            tile_radius: 15.0,
            button_radius: 14.0,
            card_radius: 18.0,
            popup_radius: 18.0,
            cover_radius: 10.0,
            divider: 1.0,
        },
        font: Family::Segoe,
        tile: IconTile::Squircle,
        footer: FooterStyle::Buttons,
        terminal: false,
    }
};

/// Nord: calm and minimal (list layout from milestone 9b).
pub const NORD: Theme = {
    const NIGHT0: u32 = 0x2e3440;
    const NIGHT1: u32 = 0x3b4252;
    const NIGHT2: u32 = 0x434c5e;
    const NIGHT3: u32 = 0x4c566a;
    const FROST: u32 = 0x88c0d0;
    Theme {
        id: ThemeId::Nord,
        layout: LayoutKind::List,
        palette: Palette {
            background: &[rgb(NIGHT0), rgb(NIGHT1)],
            text: rgb(0xeceff4),
            subtext: rgb(0xd8dee9),
            accent: rgb(FROST),
            on_accent: rgb(NIGHT0),
            border: paint(NIGHT1, 1.0),
            field: paint(NIGHT1, 1.0),
            field_border: paint(NIGHT2, 1.0),
            card: paint(NIGHT1, 1.0),
            card_border: paint(NIGHT2, 1.0),
            footer: paint(NIGHT1, 0.55),
            divider: paint(NIGHT1, 1.0),
            hover: paint(NIGHT2, 0.85),
            selected: paint(NIGHT2, 1.0),
            popup: paint(NIGHT0, 0.98),
            popup_border: paint(NIGHT3, 1.0),
            chip: paint(NIGHT2, 1.0),
            tile: paint(NIGHT1, 1.0),
            tile_borders: &[],
            track: paint(NIGHT3, 1.0),
            progress: None,
            play: None,
            danger: rgb(0xbf616a),
            lock: rgb(0xebcb8b),
            restart: rgb(0x81a1c1),
            prompt: rgb(0xa3be8c),
            shadow: None,
        },
        shape: Shape {
            menu_radius: 12.0,
            menu_border: 2.0,
            search_radius: 8.0,
            search_border: 2.0,
            cell_radius: 10.0,
            tile_radius: 10.0,
            button_radius: 8.0,
            card_radius: 10.0,
            popup_radius: 10.0,
            cover_radius: 6.0,
            divider: 2.0,
        },
        font: Family::Segoe,
        tile: IconTile::Rounded,
        footer: FooterStyle::Power,
        terminal: false,
    }
};

/// Tokyo Night: neon, dashboard feel (sidebar layout from milestone 9b).
pub const TOKYO_NIGHT: Theme = {
    const BG: u32 = 0x1a1b26;
    const RAIL: u32 = 0x16161e;
    const SURFACE: u32 = 0x24283b;
    const BORDER: u32 = 0x292e42;
    const INPUT: u32 = 0x3b4261;
    Theme {
        id: ThemeId::TokyoNight,
        layout: LayoutKind::Sidebar,
        palette: Palette {
            background: &[rgb(BG), rgb(RAIL)],
            text: rgb(0xc0caf5),
            subtext: rgb(0xa9b1d6),
            accent: rgb(0x7aa2f7),
            on_accent: rgb(BG),
            border: paint(BORDER, 1.0),
            field: paint(RAIL, 0.9),
            field_border: paint(INPUT, 1.0),
            card: paint(SURFACE, 1.0),
            card_border: paint(BORDER, 1.0),
            footer: paint(RAIL, 0.7),
            divider: paint(BORDER, 1.0),
            hover: paint(BORDER, 1.0),
            selected: paint(INPUT, 0.8),
            popup: paint(BG, 0.98),
            popup_border: paint(INPUT, 1.0),
            chip: paint(BORDER, 1.0),
            tile: paint(SURFACE, 1.0),
            // blue, purple, cyan, green, orange, yellow, red, teal
            tile_borders: &[
                rgb(0x7aa2f7),
                rgb(0xbb9af7),
                rgb(0x7dcfff),
                rgb(0x9ece6a),
                rgb(0xff9e64),
                rgb(0xe0af68),
                rgb(0xf7768e),
                rgb(0x73daca),
            ],
            track: paint(INPUT, 1.0),
            progress: Some(rgb(0xbb9af7)),
            play: None,
            danger: rgb(0xf7768e),
            lock: rgb(0xe0af68),
            restart: rgb(0x7dcfff),
            prompt: rgb(0x9ece6a),
            shadow: None,
        },
        shape: Shape {
            menu_radius: 18.0,
            menu_border: 1.0,
            search_radius: 10.0,
            search_border: 1.0,
            cell_radius: 12.0,
            tile_radius: 11.0,
            button_radius: 10.0,
            card_radius: 14.0,
            popup_radius: 14.0,
            cover_radius: 8.0,
            divider: 1.0,
        },
        font: Family::Segoe,
        tile: IconTile::Bordered,
        footer: FooterStyle::Power,
        terminal: false,
    }
};

/// Gruvbox (dark): terminal style.
pub const GRUVBOX: Theme = {
    const BG: u32 = 0x282828;
    const BG0_HARD: u32 = 0x1d2021;
    const BG1: u32 = 0x3c3836;
    const BG2: u32 = 0x504945;
    const BG3: u32 = 0x665c54;
    const YELLOW: u32 = 0xfabd2f;
    Theme {
        id: ThemeId::Gruvbox,
        layout: LayoutKind::Grid,
        palette: Palette {
            background: &[rgb(BG), rgb(BG0_HARD)],
            text: rgb(0xebdbb2),
            subtext: rgb(0xd5c4a1),
            accent: rgb(YELLOW),
            on_accent: rgb(BG),
            border: paint(BG2, 1.0),
            field: paint(BG0_HARD, 0.9),
            field_border: paint(BG3, 1.0),
            card: paint(BG1, 0.9),
            card_border: paint(BG2, 1.0),
            footer: paint(BG1, 0.6),
            divider: paint(BG2, 1.0),
            hover: paint(BG2, 0.75),
            selected: paint(BG2, 1.0),
            popup: paint(BG, 0.99),
            popup_border: paint(BG3, 1.0),
            chip: paint(BG2, 1.0),
            tile: paint(BG1, 1.0),
            tile_borders: &[],
            track: paint(BG2, 1.0),
            progress: Some(rgb(YELLOW)),
            play: None,
            danger: rgb(0xfb4934),
            lock: rgb(0xebdbb2),
            restart: rgb(0xebdbb2),
            prompt: rgb(0xb8bb26), // green
            shadow: Some(rgb(0x0f1112)),
        },
        shape: Shape {
            menu_radius: 4.0,
            menu_border: 2.0,
            search_radius: 2.0,
            search_border: 2.0,
            cell_radius: 3.0,
            tile_radius: 2.0,
            button_radius: 2.0,
            card_radius: 3.0,
            popup_radius: 3.0,
            cover_radius: 2.0,
            divider: 2.0,
        },
        font: Family::Mono,
        tile: IconTile::Sharp,
        footer: FooterStyle::Text,
        terminal: true,
    }
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::background::{contrast, darken, guard_overlay};

    #[test]
    fn names_round_trip() {
        for id in ThemeId::ALL {
            assert_eq!(ThemeId::parse(id.name()), Some(id));
            assert_eq!(id.theme().id, id);
        }
        assert_eq!(ThemeId::parse("Tokyo_Night"), Some(ThemeId::TokyoNight));
        assert_eq!(ThemeId::parse("tokyonight"), Some(ThemeId::TokyoNight));
        assert_eq!(ThemeId::parse("dracula"), None);
    }

    #[test]
    fn glint_is_unchanged() {
        let t = &GLINT;
        assert_eq!(t.palette.text, Rgb::new(0xf4, 0xf4, 0xf5));
        assert_eq!(t.palette.accent, crate::config::DEFAULT_ACCENT);
        assert_eq!(
            Some(t.palette.background),
            crate::config::Preset::Aurora.colors()
        );
        assert_eq!(t.shape.menu_radius, 18.0);
        assert_eq!(t.layout, LayoutKind::Grid);
    }

    /// IMPROVE.md 9.4: every theme passes the contrast guard on its own
    /// colors, with no extra overlay needed.
    #[test]
    fn every_theme_is_readable_on_its_own_background() {
        for id in ThemeId::ALL {
            let p = &id.theme().palette;
            for &bg in p.background {
                assert!(contrast(p.text, bg) >= 4.5, "{id:?}: text on {}", bg.hex());
            }
            assert!(
                (guard_overlay(p.background, 0.0, p.text) - 0.0).abs() < 0.01,
                "{id:?} needs an overlay"
            );
            // Text on the accent / danger fills is readable too.
            assert!(contrast(p.on_accent, p.accent) >= 4.5, "{id:?} on accent");
        }
    }

    #[test]
    fn subtext_is_readable_on_the_darkest_overlay_too() {
        for id in ThemeId::ALL {
            let p = &id.theme().palette;
            for &bg in p.background {
                assert!(contrast(p.subtext, darken(bg, 0.0)) >= 4.5, "{id:?}");
            }
        }
    }
}
