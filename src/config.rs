//! Settings. Milestone 2 only has built-in defaults (plus a few command-line
//! flags for testing); reading `config.toml` arrives in milestone 6.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundMode {
    Solid,
    Acrylic,
    Gradient,
    AcrylicGradient,
}

impl BackgroundMode {
    pub const ALL: [Self; 4] = [
        Self::Solid,
        Self::Acrylic,
        Self::Gradient,
        Self::AcrylicGradient,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Solid => "solid",
            Self::Acrylic => "acrylic",
            Self::Gradient => "gradient",
            Self::AcrylicGradient => "acrylic-gradient",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }

    /// True for the modes that blur what is behind the window.
    pub fn uses_backdrop(self) -> bool {
        matches!(self, Self::Acrylic | Self::AcrylicGradient)
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
}

impl Preset {
    /// The presets that can be picked from the tray menu.
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
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::BUILT_IN
            .into_iter()
            .chain([Self::Custom])
            .find(|p| p.name() == s)
    }

    /// The preset's colors (CLAUDE.md section 6). `None` for custom.
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
            Self::Custom => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    pub mode: BackgroundMode,
    pub preset: Preset,
    /// Used when `preset` is `Custom`; 1–3 colors.
    pub colors: Vec<Rgb>,
    /// Gradient direction in degrees (CSS convention: 0 = up, 90 = right).
    pub angle: f32,
    /// Black overlay alpha, 0.0–0.6.
    pub overlay: f32,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            mode: BackgroundMode::AcrylicGradient,
            preset: Preset::Aurora,
            colors: vec![
                Rgb::new(0x2b, 0x1a, 0x4a),
                Rgb::new(0x1a, 0x23, 0x47),
                Rgb::new(0x0f, 0x2a, 0x3a),
            ],
            angle: 135.0,
            overlay: 0.2,
        }
    }
}

impl Background {
    /// The 1–3 colors actually in use (preset or custom).
    pub fn resolved_colors(&self) -> Vec<Rgb> {
        let colors = match self.preset.colors() {
            Some(c) => c.to_vec(),
            None => self.colors.iter().copied().take(3).collect(),
        };
        if colors.is_empty() {
            Preset::Plain.colors().unwrap_or_default().to_vec()
        } else {
            colors
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub open_ms: u32,
    pub close_ms: u32,
    /// Used for the icon stagger in milestone 3.
    #[allow(dead_code)]
    pub stagger_ms: u32,
    /// 2nd control-point y of the open easing (1.0 = no bounce).
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub background: Background,
    pub animation: Animation,
}

impl Config {
    /// Defaults, plus test flags: `--bg <mode>`, `--preset <name>`,
    /// `--overlay <0..0.6>`, `--angle <deg>`, `--colors "#a,#b,#c"`.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        let mut config = Self::default();
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let Some(value) = args.next() else { break };
            let bg = &mut config.background;
            match flag.as_str() {
                "--bg" => bg.mode = BackgroundMode::parse(&value).unwrap_or(bg.mode),
                "--preset" => bg.preset = Preset::parse(&value).unwrap_or(bg.preset),
                "--overlay" => bg.overlay = value.parse().unwrap_or(bg.overlay),
                "--angle" => bg.angle = value.parse().unwrap_or(bg.angle),
                "--colors" => {
                    bg.colors = value.split(',').filter_map(Rgb::parse).take(3).collect();
                    bg.preset = Preset::Custom;
                }
                _ => log_warn!("unknown flag {flag}"),
            }
        }
        config.background.overlay = config.background.overlay.clamp(0.0, 0.6);
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors() {
        assert_eq!(Rgb::parse("#8fa8ff"), Some(Rgb::new(0x8f, 0xa8, 0xff)));
        assert_eq!(Rgb::parse("1e2026"), Some(Rgb::new(0x1e, 0x20, 0x26)));
        assert_eq!(Rgb::parse("#123"), None);
        assert_eq!(Rgb::parse("#zzzzzz"), None);
    }

    #[test]
    fn presets_and_custom_colors() {
        let mut bg = Background::default();
        assert_eq!(bg.resolved_colors().len(), 3); // aurora
        bg.preset = Preset::Custom;
        assert_eq!(bg.resolved_colors()[0], Rgb::new(0x2b, 0x1a, 0x4a));
        bg.colors.clear();
        assert_eq!(bg.resolved_colors(), [Rgb::new(0x1e, 0x20, 0x26)]);
    }

    #[test]
    fn command_line_flags() {
        let args = ["--bg", "solid", "--preset", "ocean", "--overlay", "0.9"];
        let c = Config::from_args(args.map(String::from));
        assert_eq!(c.background.mode, BackgroundMode::Solid);
        assert_eq!(c.background.preset, Preset::Ocean);
        assert_eq!(c.background.overlay, 0.6); // clamped
    }
}
