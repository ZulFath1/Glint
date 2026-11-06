//! Menu background: solid / acrylic / gradient / acrylic-gradient, with the
//! black overlay, the readability (contrast) guard and the fallbacks.
//!
//! Layers inside the menu's rounded clip, bottom to top:
//! backdrop (blur of what is behind the window) → tint or gradient → overlay.
//!
//! All brushes are created once and only re-colored afterwards, so switching
//! modes costs nothing and nothing is redrawn per frame.

use std::cell::{Cell, RefCell};

use windows::UI::Color;
use windows::UI::Composition::{
    CompositionBackdropBrush, CompositionColorBrush, CompositionLinearGradientBrush,
    CompositionMappingMode, Compositor, ContainerVisual, SpriteVisual,
};
use windows::core::Result;
use windows_numerics::Vector2;

use crate::config::{self, BackgroundMode, Rgb};

/// Primary text color (#f4f4f5) that must stay readable on the background.
pub const TEXT: Rgb = Rgb::new(0xf4, 0xf4, 0xf5);
const MIN_CONTRAST: f32 = 4.5;
/// Alpha of the tint/gradient over the blurred backdrop.
const GLASS_TINT_ALPHA: f32 = 0.55;
const GUARD_STEP: f32 = 0.05;
const GUARD_MAX: f32 = 0.9;

/// The Windows settings that can force acrylic off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemEffects {
    /// Settings > Personalization > Colors > Transparency effects.
    pub transparency: bool,
    pub battery_saver: bool,
}

/// What `apply` ended up doing (for logging and testing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    pub glass: bool,
    pub overlay: f32,
}

pub struct Background {
    compositor: Compositor,
    size: Vector2,
    backdrop: SpriteVisual,
    tint: SpriteVisual,
    tint_color: CompositionColorBrush,
    gradient: CompositionLinearGradientBrush,
    overlay_color: CompositionColorBrush,
    /// Created on first use; reused after that.
    host_backdrop: RefCell<Option<CompositionBackdropBrush>>,
    host_backdrop_failed: Cell<bool>,
}

impl Background {
    /// Creates the three layers and adds them to `parent` (bottom to top).
    pub fn new(compositor: &Compositor, parent: &ContainerVisual, size: Vector2) -> Result<Self> {
        let layer = || -> Result<SpriteVisual> {
            let v = compositor.CreateSpriteVisual()?;
            v.SetSize(size)?;
            parent.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        let backdrop = layer()?;
        let tint = layer()?;
        let overlay = layer()?;

        let tint_color = compositor.CreateColorBrush()?;
        let gradient = compositor.CreateLinearGradientBrush()?;
        gradient.SetMappingMode(CompositionMappingMode::Absolute)?;
        let overlay_color = compositor.CreateColorBrush()?;
        overlay.SetBrush(&overlay_color)?;

        Ok(Self {
            compositor: compositor.clone(),
            size,
            backdrop,
            tint,
            tint_color,
            gradient,
            overlay_color,
            host_backdrop: RefCell::new(None),
            host_backdrop_failed: Cell::new(false),
        })
    }

    /// Applies the background settings. Safe to call again at any time
    /// (settings changed, transparency toggled, battery saver...).
    pub fn apply(&self, cfg: &config::Background, effects: SystemEffects) -> Result<Applied> {
        let colors = cfg.resolved_colors();

        let wants_glass =
            cfg.mode.uses_backdrop() && effects.transparency && !effects.battery_saver;
        let glass = wants_glass && self.attach_host_backdrop()?;
        self.backdrop.SetIsVisible(glass)?;

        // Without blur, the tint is drawn fully opaque (the "solid" fallback).
        let alpha = if glass { GLASS_TINT_ALPHA } else { 1.0 };
        let use_gradient = matches!(
            cfg.mode,
            BackgroundMode::Gradient | BackgroundMode::AcrylicGradient
        ) && colors.len() > 1;
        if use_gradient {
            let (start, end) = gradient_points(self.size, cfg.angle);
            self.gradient.SetStartPoint(start)?;
            self.gradient.SetEndPoint(end)?;
            let stops = self.gradient.ColorStops()?;
            stops.Clear()?;
            let last = (colors.len() - 1) as f32;
            for (i, &c) in colors.iter().enumerate() {
                let stop = self.compositor.CreateColorGradientStopWithOffsetAndColor(
                    i as f32 / last,
                    to_color(c, alpha),
                )?;
                stops.Append(&stop)?;
            }
            self.tint.SetBrush(&self.gradient)?;
        } else {
            self.tint_color.SetColor(to_color(colors[0], alpha))?;
            self.tint.SetBrush(&self.tint_color)?;
        }

        // Readability guard: check only the colors that are actually shown.
        let shown = if use_gradient {
            &colors[..]
        } else {
            &colors[..1]
        };
        let overlay = guard_overlay(shown, cfg.overlay);
        if overlay > cfg.overlay + f32::EPSILON {
            log_info!(
                "contrast guard raised the overlay from {:.2} to {overlay:.2}",
                cfg.overlay
            );
        }
        self.overlay_color.SetColor(Color {
            A: alpha_byte(overlay),
            R: 0,
            G: 0,
            B: 0,
        })?;

        if wants_glass && !glass {
            log_warn!("acrylic unavailable; using the solid fallback");
        }
        Ok(Applied { glass, overlay })
    }

    /// Puts the HostBackdropBrush on the backdrop layer. Returns false if it
    /// can't be created (then we stay on the solid fallback for good).
    fn attach_host_backdrop(&self) -> Result<bool> {
        if self.host_backdrop_failed.get() {
            return Ok(false);
        }
        let mut cached = self.host_backdrop.borrow_mut();
        if cached.is_none() {
            match self.compositor.CreateHostBackdropBrush() {
                Ok(brush) => {
                    self.backdrop.SetBrush(&brush)?;
                    *cached = Some(brush);
                }
                Err(e) => {
                    log_error!("CreateHostBackdropBrush failed: {e}");
                    self.host_backdrop_failed.set(true);
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

fn alpha_byte(a: f32) -> u8 {
    (a.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn to_color(c: Rgb, alpha: f32) -> Color {
    Color {
        A: alpha_byte(alpha),
        R: c.r,
        G: c.g,
        B: c.b,
    }
}

/// Start and end of the gradient line, CSS style: 0° points up, 90° right,
/// and the line is long enough that the corners get the first/last color.
pub fn gradient_points(size: Vector2, angle_deg: f32) -> (Vector2, Vector2) {
    let t = angle_deg.to_radians();
    let (dx, dy) = (t.sin(), -t.cos());
    let half = (size.X * dx.abs() + size.Y * dy.abs()) / 2.0;
    let (cx, cy) = (size.X / 2.0, size.Y / 2.0);
    (
        Vector2 {
            X: cx - dx * half,
            Y: cy - dy * half,
        },
        Vector2 {
            X: cx + dx * half,
            Y: cy + dy * half,
        },
    )
}

/// WCAG relative luminance of an sRGB color.
pub fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio between two colors (1.0 to 21.0).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The color after a black overlay of `alpha` is drawn on top.
pub fn darken(c: Rgb, alpha: f32) -> Rgb {
    let k = 1.0 - alpha.clamp(0.0, 1.0);
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb::new(f(c.r), f(c.g), f(c.b))
}

/// Raises the overlay in 0.05 steps until `TEXT` has 4.5:1 contrast on every
/// color. Never returns less than the user's overlay.
pub fn guard_overlay(colors: &[Rgb], user_overlay: f32) -> f32 {
    let passes = |o: f32| {
        colors
            .iter()
            .all(|&c| contrast(TEXT, darken(c, o)) >= MIN_CONTRAST)
    };
    let mut steps = 0u32;
    loop {
        let o = user_overlay + steps as f32 * GUARD_STEP;
        if o >= GUARD_MAX || passes(o) {
            return o.min(GUARD_MAX.max(user_overlay));
        }
        steps += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Preset;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn contrast_matches_wcag() {
        let white = Rgb::new(255, 255, 255);
        let black = Rgb::new(0, 0, 0);
        assert!(close(contrast(white, black), 21.0));
        assert!(close(contrast(white, white), 1.0));
        // #777777 on white is the classic ~4.48:1 example.
        assert!(close(contrast(Rgb::new(0x77, 0x77, 0x77), white), 4.48));
    }

    #[test]
    fn dark_presets_need_no_extra_overlay() {
        for preset in Preset::BUILT_IN {
            let colors = preset.colors().unwrap();
            assert!(close(guard_overlay(colors, 0.2), 0.2), "{preset:?}");
        }
    }

    #[test]
    fn bright_colors_get_more_overlay_but_never_less() {
        let bright = [Rgb::new(0xf0, 0xa0, 0x40), Rgb::new(0x40, 0xd0, 0xf0)];
        let o = guard_overlay(&bright, 0.1);
        assert!(o > 0.1);
        for c in bright {
            assert!(contrast(TEXT, darken(c, o)) >= MIN_CONTRAST);
        }
        // The user's overlay is kept when it is already high enough.
        assert!(close(guard_overlay(&bright, 0.6), 0.6));
    }

    #[test]
    fn gradient_angles_follow_css() {
        let size = Vector2 { X: 200.0, Y: 100.0 };
        let (s, e) = gradient_points(size, 90.0); // to the right
        assert!(close(s.X, 0.0) && close(s.Y, 50.0) && close(e.X, 200.0) && close(e.Y, 50.0));
        let (s, e) = gradient_points(size, 180.0); // downwards
        assert!(close(s.X, 100.0) && close(s.Y, 0.0) && close(e.X, 100.0) && close(e.Y, 100.0));
    }
}
