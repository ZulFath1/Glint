//! Menu background: solid / gradient / glass / glass-gradient, with the
//! black overlay, the readability (contrast) guard and the fallbacks.
//!
//! Layers inside the menu's rounded clip, bottom to top:
//! backdrop (blur of what is behind the window) → tint or gradient →
//! overlay → sheen (glass only) → border / glass rim.
//!
//! "Glass" is our take on a liquid-glass look within what Windows offers:
//! the system's blurred backdrop, a tint whose strength is configurable
//! (0 = colorless), a bright rim that catches light at the top-left corner,
//! and a soft sheen fading down from the top edge.
//!
//! All brushes are created once and only re-colored afterwards, so switching
//! modes costs nothing and nothing is redrawn per frame.

use std::cell::{Cell, RefCell};

use windows::UI::Color;
use windows::UI::Composition::{
    CompositionBackdropBrush, CompositionColorBrush, CompositionColorSpace,
    CompositionLinearGradientBrush, CompositionMappingMode, CompositionObject,
    CompositionRoundedRectangleGeometry, CompositionSpriteShape, Compositor, ContainerVisual,
    ShapeVisual, SpriteVisual,
};
use windows::core::{Interface, Result, h};
use windows_numerics::Vector2;

use super::anim::{Kind, ms};

/// The album tint cross-fades this long.
const ALBUM_FADE_MS: u32 = 400;
use crate::config::{self, Rgb};

/// Primary text color (#f4f4f5) that must stay readable on the background.
pub const TEXT: Rgb = Rgb::new(0xf4, 0xf4, 0xf5);
const MIN_CONTRAST: f32 = 4.5;
const GUARD_STEP: f32 = 0.05;
const GUARD_MAX: f32 = 0.9;

/// Plain border for the non-glass modes: rgba(255,255,255,0.10), 1 px.
const BORDER_ALPHA: f32 = 0.10;
const BORDER_WIDTH: f32 = 1.0;
/// High contrast border: thicker, so the menu's edge is easy to see.
const HC_BORDER_WIDTH: f32 = 2.0;
/// Glass rim: thicker, with light catching the corners.
const RIM_WIDTH: f32 = 1.5;
/// (offset along the top-left → bottom-right diagonal, white alpha)
const RIM_STOPS: [(f32, f32); 4] = [(0.0, 0.60), (0.3, 0.16), (0.7, 0.06), (1.0, 0.30)];
/// Sheen: white at the top edge fading out by this fraction of the height.
const SHEEN_ALPHA: f32 = 0.14;
const SHEEN_DEPTH: f32 = 0.5;

/// The Windows settings that can force the glass off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemEffects {
    /// Settings > Personalization > Colors > Transparency effects.
    pub transparency: bool,
    pub battery_saver: bool,
    /// Windows high contrast theme is on: its colors replace ours.
    pub high_contrast: Option<HighContrast>,
}

/// The colors of the active Windows high contrast theme
/// (`GetSysColor`), used instead of the configured background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighContrast {
    /// `COLOR_WINDOW`: the menu background.
    pub window: Rgb,
    /// `COLOR_WINDOWTEXT`: text and the border.
    pub text: Rgb,
    /// `COLOR_HIGHLIGHT`: caret, focus ring, focused search border.
    pub highlight: Rgb,
}

/// What `apply` ended up doing (for logging and testing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    pub glass: bool,
    pub overlay: f32,
}

pub struct Background {
    compositor: Compositor,
    size: Cell<Vector2>,
    corner_radius: f32,
    backdrop: SpriteVisual,
    tint: SpriteVisual,
    overlay: SpriteVisual,
    sheen: SpriteVisual,
    sheen_brush: CompositionLinearGradientBrush,
    border_visual: ShapeVisual,
    border_width: Cell<f32>,
    angle: Cell<f32>,
    tint_color: CompositionColorBrush,
    gradient: CompositionLinearGradientBrush,
    overlay_color: CompositionColorBrush,
    border_geometry: CompositionRoundedRectangleGeometry,
    border: CompositionSpriteShape,
    border_color: CompositionColorBrush,
    rim: CompositionLinearGradientBrush,
    /// Created on first use; reused after that.
    host_backdrop: RefCell<Option<CompositionBackdropBrush>>,
    host_backdrop_failed: Cell<bool>,
    /// The album tint: the cover's color, replacing the first color.
    album: Cell<Option<Rgb>>,
    /// The last settings applied, to apply them again with a new album color.
    last: RefCell<Option<(config::Background, SystemEffects)>>,
}

fn white(alpha: f32) -> Color {
    Color {
        A: alpha_byte(alpha),
        R: 255,
        G: 255,
        B: 255,
    }
}

impl Background {
    /// Creates the layers and adds them to `parent` (bottom to top).
    pub fn new(
        compositor: &Compositor,
        parent: &ContainerVisual,
        size: Vector2,
        corner_radius: f32,
    ) -> Result<Self> {
        let layer = || -> Result<SpriteVisual> {
            let v = compositor.CreateSpriteVisual()?;
            v.SetSize(size)?;
            parent.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        let backdrop = layer()?;
        let tint = layer()?;
        let overlay = layer()?;
        let sheen = layer()?;

        let tint_color = compositor.CreateColorBrush()?;
        let gradient = compositor.CreateLinearGradientBrush()?;
        gradient.SetMappingMode(CompositionMappingMode::Absolute)?;
        let overlay_color = compositor.CreateColorBrush()?;
        overlay.SetBrush(&overlay_color)?;

        // Sheen: white at the top, fading to nothing.
        let sheen_brush = compositor.CreateLinearGradientBrush()?;
        sheen_brush.SetMappingMode(CompositionMappingMode::Absolute)?;
        sheen_brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
        sheen_brush.SetEndPoint(Vector2 {
            X: 0.0,
            Y: size.Y * SHEEN_DEPTH,
        })?;
        let stops = sheen_brush.ColorStops()?;
        stops.Append(
            &compositor.CreateColorGradientStopWithOffsetAndColor(0.0, white(SHEEN_ALPHA))?,
        )?;
        stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(1.0, white(0.0))?)?;
        sheen.SetBrush(&sheen_brush)?;

        // Border / rim: one rounded stroke whose brush and width change.
        let rim = compositor.CreateLinearGradientBrush()?;
        rim.SetMappingMode(CompositionMappingMode::Absolute)?;
        rim.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
        rim.SetEndPoint(size)?;
        let stops = rim.ColorStops()?;
        for (offset, alpha) in RIM_STOPS {
            stops.Append(
                &compositor.CreateColorGradientStopWithOffsetAndColor(offset, white(alpha))?,
            )?;
        }
        let border_color = compositor.CreateColorBrushWithColor(white(BORDER_ALPHA))?;
        let border_geometry = compositor.CreateRoundedRectangleGeometry()?;
        let border = compositor.CreateSpriteShapeWithGeometry(&border_geometry)?;
        let border_visual = compositor.CreateShapeVisual()?;
        border_visual.SetSize(size)?;
        border_visual.Shapes()?.Append(&border)?;
        parent.Children()?.InsertAtTop(&border_visual)?;

        Ok(Self {
            compositor: compositor.clone(),
            size: Cell::new(size),
            corner_radius,
            backdrop,
            tint,
            overlay,
            sheen,
            sheen_brush,
            border_visual,
            border_width: Cell::new(BORDER_WIDTH),
            angle: Cell::new(0.0),
            tint_color,
            gradient,
            overlay_color,
            border_geometry,
            border,
            border_color,
            rim,
            host_backdrop: RefCell::new(None),
            host_backdrop_failed: Cell::new(false),
            album: Cell::new(None),
            last: RefCell::new(None),
        })
    }

    /// Applies the background settings. Safe to call again at any time
    /// (settings changed, transparency toggled, battery saver...).
    pub fn apply(&self, cfg: &config::Background, effects: SystemEffects) -> Result<Applied> {
        self.apply_with(cfg, effects, false)
    }

    /// The album tint (CLAUDE.md section 6): `Some(cover color)` replaces
    /// the first color with a darkened version of it, `None` goes back to
    /// the normal colors. The change cross-fades over 400 ms.
    pub fn set_album_color(&self, color: Option<Rgb>) -> Result<()> {
        if self.album.replace(color) == color {
            return Ok(());
        }
        let last = self.last.borrow().clone();
        match last {
            Some((cfg, effects)) => self.apply_with(&cfg, effects, true).map(|_| ()),
            None => Ok(()),
        }
    }

    /// Sets a brush's or gradient stop's `Color`: at once, or (`fade`)
    /// cross-fading from the current color.
    fn set_color(
        &self,
        target: &CompositionObject,
        to: Color,
        fade: bool,
        set: impl FnOnce(Color) -> Result<()>,
    ) -> Result<()> {
        if fade {
            let a = self.compositor.CreateColorKeyFrameAnimation()?;
            a.SetInterpolationColorSpace(CompositionColorSpace::Rgb)?;
            a.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            a.InsertKeyFrame(1.0, to)?;
            a.SetDuration(ms(ALBUM_FADE_MS))?;
            return target.StartAnimation(h!("Color"), &a);
        }
        target.StopAnimation(h!("Color"))?;
        set(to)
    }

    fn apply_with(
        &self,
        cfg: &config::Background,
        effects: SystemEffects,
        fade: bool,
    ) -> Result<Applied> {
        *self.last.borrow_mut() = Some((cfg.clone(), effects));
        if let Some(hc) = effects.high_contrast {
            return self.apply_high_contrast(hc);
        }
        let mut colors = cfg.resolved_colors();
        if let Some(album) = self.album.get() {
            colors[0] = album_tint(album, cfg.overlay);
        }

        let wants_glass =
            cfg.mode.uses_backdrop() && effects.transparency && !effects.battery_saver;
        let glass = wants_glass && self.attach_host_backdrop()?;
        self.backdrop.SetIsVisible(glass)?;
        self.sheen.SetIsVisible(glass)?;

        // Without blur, the tint is drawn fully opaque (the solid fallback).
        let alpha = if glass { cfg.tint } else { 1.0 };
        let use_gradient = cfg.mode.uses_gradient() && colors.len() > 1;
        if use_gradient {
            self.angle.set(cfg.angle);
            let (start, end) = gradient_points(self.size.get(), cfg.angle);
            self.gradient.SetStartPoint(start)?;
            self.gradient.SetEndPoint(end)?;
            // The stops are reused (so the first one can cross-fade);
            // only a different number of colors makes new ones.
            let stops = self.gradient.ColorStops()?;
            if stops.Size()? as usize != colors.len() {
                stops.Clear()?;
                for _ in &colors {
                    stops.Append(&self.compositor.CreateColorGradientStop()?)?;
                }
            }
            let last = (colors.len() - 1) as f32;
            for (i, &c) in colors.iter().enumerate() {
                let stop = stops.GetAt(i as u32)?;
                stop.SetOffset(i as f32 / last)?;
                let fade_this = fade && i == 0;
                self.set_color(&stop.cast()?, to_color(c, alpha), fade_this, |c| {
                    stop.SetColor(c)
                })?;
            }
            self.tint.SetBrush(&self.gradient)?;
        } else {
            let brush = &self.tint_color;
            self.set_color(&brush.cast()?, to_color(colors[0], alpha), fade, |c| {
                brush.SetColor(c)
            })?;
            self.tint.SetBrush(&self.tint_color)?;
        }

        // Readability guard: check the colors that are actually shown.
        // Colorless glass has no color to check.
        let shown: &[Rgb] = if glass && cfg.tint < 0.01 {
            &[]
        } else if use_gradient {
            &colors
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

        if glass {
            self.set_border(RIM_WIDTH)?;
            self.border.SetStrokeBrush(&self.rim)?;
        } else {
            self.set_border(BORDER_WIDTH)?;
            self.border.SetStrokeBrush(&self.border_color)?;
            self.border_color.SetColor(white(BORDER_ALPHA))?;
        }

        if wants_glass && !glass {
            log_warn!("glass unavailable; using the solid fallback");
        }
        Ok(Applied { glass, overlay })
    }

    /// High contrast: one flat system color, a full-strength border in the
    /// text color, no blur, no gradient, no sheen, no overlay.
    fn apply_high_contrast(&self, hc: HighContrast) -> Result<Applied> {
        self.backdrop.SetIsVisible(false)?;
        self.sheen.SetIsVisible(false)?;
        self.tint_color.StopAnimation(h!("Color"))?;
        self.tint_color.SetColor(to_color(hc.window, 1.0))?;
        self.tint.SetBrush(&self.tint_color)?;
        self.overlay_color.SetColor(Color::default())?;
        self.set_border(HC_BORDER_WIDTH)?;
        self.border_color.SetColor(to_color(hc.text, 1.0))?;
        self.border.SetStrokeBrush(&self.border_color)?;
        Ok(Applied {
            glass: false,
            overlay: 0.0,
        })
    }

    /// The menu changed size: resize every layer. The colors stay.
    pub fn set_size(&self, size: Vector2) -> Result<()> {
        self.size.set(size);
        for layer in [&self.backdrop, &self.tint, &self.overlay, &self.sheen] {
            layer.SetSize(size)?;
        }
        self.border_visual.SetSize(size)?;
        self.sheen_brush.SetEndPoint(Vector2 {
            X: 0.0,
            Y: size.Y * SHEEN_DEPTH,
        })?;
        self.rim.SetEndPoint(size)?;
        let (start, end) = gradient_points(size, self.angle.get());
        self.gradient.SetStartPoint(start)?;
        self.gradient.SetEndPoint(end)?;
        self.set_border(self.border_width.get())
    }

    /// Everything that changes when the menu's height changes, for the
    /// 220 ms layout animation.
    pub fn motion_objects(&self) -> Result<Vec<(CompositionObject, Kind)>> {
        let mut list = Vec::new();
        for layer in [&self.backdrop, &self.tint, &self.overlay, &self.sheen] {
            list.push((layer.cast()?, Kind::Visual));
        }
        list.push((self.border_visual.cast()?, Kind::Visual));
        list.push((self.border_geometry.cast()?, Kind::Geometry));
        for brush in [&self.gradient, &self.sheen_brush, &self.rim] {
            list.push((brush.cast()?, Kind::Brush));
        }
        Ok(list)
    }

    /// The top layer (border / glass rim). The menu's content goes just
    /// below it.
    pub fn border_visual(&self) -> &ShapeVisual {
        &self.border_visual
    }

    /// Sizes the stroke so it sits fully inside the rounded clip.
    fn set_border(&self, width: f32) -> Result<()> {
        self.border_width.set(width);
        let size = self.size.get();
        let inset = width / 2.0;
        self.border_geometry
            .SetOffset(Vector2 { X: inset, Y: inset })?;
        self.border_geometry.SetSize(Vector2 {
            X: size.X - width,
            Y: size.Y - width,
        })?;
        let r = self.corner_radius - inset;
        self.border_geometry
            .SetCornerRadius(Vector2 { X: r, Y: r })?;
        self.border.SetStrokeThickness(width)
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

/// The album color made dark enough: darkened in 5% steps until `TEXT` has
/// 4.5:1 contrast on it (with the user's overlay on top).
pub fn album_tint(c: Rgb, overlay: f32) -> Rgb {
    (0..=20)
        .map(|step| darken(c, step as f32 * GUARD_STEP))
        .find(|&candidate| contrast(TEXT, darken(candidate, overlay)) >= MIN_CONTRAST)
        .unwrap_or(Rgb::new(0, 0, 0))
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
    fn album_colors_are_darkened_until_text_is_readable() {
        // A bright yellow cover gets much darker...
        let yellow = Rgb::new(0xf5, 0xd0, 0x20);
        let tint = album_tint(yellow, 0.2);
        assert!(contrast(TEXT, darken(tint, 0.2)) >= MIN_CONTRAST);
        assert!(luminance(tint) < luminance(yellow));
        // ...and keeps its hue (red stays the biggest channel).
        assert!(tint.r >= tint.b);
        // A dark cover color is used as it is.
        let navy = Rgb::new(0x10, 0x20, 0x50);
        assert_eq!(album_tint(navy, 0.2), navy);
        // So the readability guard never has to step in for it.
        assert!(close(guard_overlay(&[tint], 0.2), 0.2));
    }

    #[test]
    fn colorless_glass_keeps_the_user_overlay() {
        assert!(close(guard_overlay(&[], 0.2), 0.2));
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
