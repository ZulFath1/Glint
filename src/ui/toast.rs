//! A short message in the menu ("Copied"), shown after copying a calc or
//! unit result (SUPERSEARCH.md 3, "calc").
//!
//! One small pill at the right end of the search box. It is created the
//! first time it is needed, then reused. The fade in, the pause and the
//! fade out are one composition animation, so no timer runs.
//!
//! The pill is a compositor shape and only the text is painted: a Direct2D
//! shape would be the menu's first, which costs about 4 MB of idle RAM
//! (measured; see `results.rs`).

use std::cell::RefCell;

use windows::UI::Composition::{Compositor, ContainerVisual};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::core::{Result, h};
use windows_numerics::{Vector2, Vector3};

use super::anim::ms;
use super::footer::draw_text;
use super::gfx::{self, Canvas, Gfx};
use super::layout::Rect;
use super::shape::RoundRect;
use crate::config::Rgb;

const WIDTH: f32 = 92.0;
const HEIGHT: f32 = 26.0;
const SIZE: Vector2 = Vector2 {
    X: WIDTH,
    Y: HEIGHT,
};
/// Space between the pill and the search box's right edge.
const INSET: f32 = 7.0;
const TEXT_SIZE: f32 = 12.0;
/// Fade in, stay, fade out: 1.4 s in all.
const DURATION_MS: u32 = 1400;

/// The parts, made on first use.
struct Parts {
    /// Holds the pill and the text; its opacity fades.
    group: ContainerVisual,
    pill: RoundRect,
    text: Canvas,
    /// What the text canvas shows, in which color.
    painted: String,
    color: Rgb,
}

pub struct Toast {
    compositor: Compositor,
    parent: ContainerVisual,
    parts: RefCell<Option<Parts>>,
}

impl Toast {
    pub fn new(compositor: &Compositor, parent: &ContainerVisual) -> Self {
        Self {
            compositor: compositor.clone(),
            parent: parent.clone(),
            parts: RefCell::new(None),
        }
    }

    fn create(&self, gfx: &Gfx) -> Result<Parts> {
        let group = self.compositor.CreateContainerVisual()?;
        group.SetOpacity(0.0)?;
        group.SetSize(SIZE)?;
        let pill = RoundRect::new(&self.compositor, HEIGHT / 2.0, 0.0)?;
        pill.set_rect(Rect::new(0.0, 0.0, WIDTH, HEIGHT))?;
        let text = Canvas::new(&self.compositor, gfx, SIZE)?;
        group.Children()?.InsertAtTop(&pill.visual)?;
        group.Children()?.InsertAtTop(&text.visual)?;
        self.parent.Children()?.InsertAtTop(&group)?;
        Ok(Parts {
            group,
            pill,
            text,
            painted: String::new(),
            color: Rgb::new(0, 0, 0),
        })
    }

    /// Shows `text` on an `accent` pill inside `search` (menu DIPs).
    pub fn show(&self, gfx: &Gfx, search: Rect, text: &str, accent: Rgb) -> Result<()> {
        let mut slot = self.parts.borrow_mut();
        if slot.is_none() {
            *slot = Some(self.create(gfx)?);
        }
        let Some(p) = slot.as_mut() else {
            return Ok(());
        };
        if p.painted != text || p.color != accent {
            let format = gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let fg = gfx::on_fill(accent);
            p.text.draw(gfx, |ctx| {
                let r = Rect::new(0.0, 0.0, WIDTH, HEIGHT);
                draw_text(ctx, gfx, text, &format, r, DWRITE_TEXT_ALIGNMENT_CENTER, fg)
            })?;
            p.pill.set_fill(gfx::color(accent, 1.0))?;
            p.painted = text.to_string();
            p.color = accent;
        }
        let s = gfx.scale();
        p.group.SetOffset(Vector3 {
            X: gfx::snap(search.right() - INSET - WIDTH, s),
            Y: gfx::snap(search.y + (search.h - HEIGHT) / 2.0, s),
            Z: 0.0,
        })?;
        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
        fade.InsertKeyFrame(0.0, 0.0)?;
        fade.InsertKeyFrame(0.08, 1.0)?;
        fade.InsertKeyFrame(0.8, 1.0)?;
        fade.InsertKeyFrame(1.0, 0.0)?;
        fade.SetDuration(ms(DURATION_MS))?;
        p.group.StartAnimation(h!("Opacity"), &fade)
    }

    /// Hides it at once (the menu closed).
    pub fn hide(&self) -> Result<()> {
        if let Some(p) = self.parts.borrow().as_ref() {
            p.group.StopAnimation(h!("Opacity"))?;
            p.group.SetOpacity(0.0)?;
        }
        Ok(())
    }

    /// New DPI scale or device: resized now, painted again on next use.
    pub fn forget(&self, gfx: &Gfx) -> Result<()> {
        if let Some(p) = self.parts.borrow_mut().as_mut() {
            p.painted.clear();
            p.text.resize(gfx, SIZE)?;
        }
        Ok(())
    }
}
