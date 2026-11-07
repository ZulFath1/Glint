//! The right-click menu on an app: "Pin to Glint" or "Unpin from Glint".
//!
//! Drawn by us, in the menu's own style (not the Windows menu): a small
//! dark rounded panel with one row, popping in from the click point with
//! the same animation as the power strip.

use std::cell::{Cell, RefCell};

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    IDWriteTextFormat,
};
use windows::core::{Interface, Result, h};
use windows_numerics::{Vector2, Vector3};

use super::anim::{Animator, POP_FROM_SCALE};
use super::footer::draw_text;
use super::gfx::{self, Canvas, Gfx, text_primary};
use super::layout::Rect;
use super::shape::RoundRect;
use crate::config::Rgb;

const WIDTH: f32 = 184.0;
const HEIGHT: f32 = 44.0;
/// Space around the single row.
const PAD: f32 = 4.0;
const RADIUS: f32 = 12.0;
const ROW_RADIUS: f32 = 8.0;
/// Kept this far from the menu's edges.
const MARGIN: f32 = 8.0;
const FILL: Rgb = Rgb::new(28, 28, 34);
const FILL_ALPHA: f32 = 0.96;
const GLYPH_SIZE: f32 = 16.0;
const LABEL_SIZE: f32 = 13.0;
const PIN_GLYPH: &str = "\u{E718}";
const UNPIN_GLYPH: &str = "\u{E77A}";

/// The app the menu is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuTarget {
    pub id: String,
    /// Already pinned: the menu offers "Unpin".
    pub pinned: bool,
}

/// Where the menu goes for a click at `at`, inside a menu of `menu` size:
/// below and to the right of the click, flipped up or left when it would
/// stick out, and never closer than `MARGIN` to the edges.
pub fn place(at: Vector2, menu: Vector2) -> Rect {
    let max_x = (menu.X - WIDTH - MARGIN).max(MARGIN);
    let x = at.X.clamp(MARGIN, max_x);
    let y = if at.Y + HEIGHT + MARGIN <= menu.Y {
        at.Y
    } else {
        at.Y - HEIGHT
    };
    let max_y = (menu.Y - HEIGHT - MARGIN).max(MARGIN);
    Rect::new(x, y.clamp(MARGIN, max_y), WIDTH, HEIGHT)
}

pub struct ContextMenu {
    root: ContainerVisual,
    hover: RoundRect,
    content: Canvas,
    /// Where it is, in menu coordinates.
    rect: Cell<Rect>,
    target: RefCell<Option<MenuTarget>>,
    glyph_format: IDWriteTextFormat,
    label_format: IDWriteTextFormat,
}

impl ContextMenu {
    pub fn new(c: &Compositor, gfx: &Gfx, parent: &ContainerVisual) -> Result<Self> {
        let size = Vector2 {
            X: WIDTH,
            Y: HEIGHT,
        };
        let root = c.CreateContainerVisual()?;
        root.SetOpacity(0.0)?;
        root.SetIsVisible(false)?;
        root.SetSize(size)?;
        let panel = RoundRect::new(c, RADIUS, 1.0)?;
        panel.set_fill(gfx::color(FILL, FILL_ALPHA))?;
        panel.set_stroke(gfx::color(Rgb::new(255, 255, 255), 0.10))?;
        panel.set_rect(Rect::new(0.0, 0.0, WIDTH, HEIGHT))?;
        let hover = RoundRect::new(c, ROW_RADIUS, 0.0)?;
        hover.set_fill(gfx::color(Rgb::new(255, 255, 255), 0.08))?;
        hover.set_rect(Self::row_local())?;
        hover.show(false)?;
        let content = Canvas::new(c, gfx, size)?;
        root.Children()?.InsertAtTop(&panel.visual)?;
        root.Children()?.InsertAtTop(&hover.visual)?;
        root.Children()?.InsertAtTop(&content.visual)?;
        parent.Children()?.InsertAtTop(&root)?;
        Ok(Self {
            root,
            hover,
            content,
            rect: Cell::new(Rect::new(0.0, 0.0, WIDTH, HEIGHT)),
            target: RefCell::new(None),
            glyph_format: gfx.icon_format(GLYPH_SIZE)?,
            label_format: gfx.text_format(LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
        })
    }

    /// The row, inside the panel.
    fn row_local() -> Rect {
        Rect::new(PAD, PAD, WIDTH - 2.0 * PAD, HEIGHT - 2.0 * PAD)
    }

    pub fn is_open(&self) -> bool {
        self.target.borrow().is_some()
    }

    pub fn target(&self) -> Option<MenuTarget> {
        self.target.borrow().clone()
    }

    /// The whole panel, in menu coordinates.
    pub fn rect(&self) -> Rect {
        self.rect.get()
    }

    /// The row (the one thing to click), in menu coordinates.
    pub fn row(&self) -> Rect {
        let r = self.rect.get();
        let row = Self::row_local();
        Rect::new(r.x + row.x, r.y + row.y, row.w, row.h)
    }

    pub fn set_hover(&self, on: bool) -> Result<()> {
        self.hover.show(on)
    }

    /// Opens (or moves) the menu for `target`, growing out of `at` (menu
    /// coordinates) inside a menu of size `menu`.
    pub fn open(
        &self,
        gfx: &Gfx,
        animator: &Animator,
        target: MenuTarget,
        at: Vector2,
        menu: Vector2,
        reduced_motion: bool,
    ) -> Result<()> {
        let r = place(at, menu);
        self.rect.set(r);
        let s = gfx.scale();
        self.root.SetOffset(Vector3 {
            X: gfx::snap(r.x, s),
            Y: gfx::snap(r.y, s),
            Z: 0.0,
        })?;
        // Grow out of the click point.
        self.root.SetCenterPoint(Vector3 {
            X: (at.X - r.x).clamp(0.0, r.w),
            Y: (at.Y - r.y).clamp(0.0, r.h),
            Z: 0.0,
        })?;
        self.redraw(gfx, &target)?;
        *self.target.borrow_mut() = Some(target);
        self.hover.show(false)?;

        let v: Visual = self.root.cast()?;
        // Start small and see-through every time (also when it was already
        // open somewhere else: it pops in again at the new place).
        v.StopAnimation(h!("Opacity"))?;
        v.StopAnimation(h!("Scale"))?;
        v.SetOpacity(0.0)?;
        v.SetScale(Vector3 {
            X: POP_FROM_SCALE,
            Y: POP_FROM_SCALE,
            Z: 1.0,
        })?;
        v.SetIsVisible(true)?;
        animator.pop_in(&v, reduced_motion)
    }

    /// Repaints at the current DPI scale (after a DPI change).
    pub fn refresh(&self, gfx: &Gfx) -> Result<()> {
        self.content.resize(
            gfx,
            Vector2 {
                X: WIDTH,
                Y: HEIGHT,
            },
        )?;
        match self.target() {
            Some(t) => self.redraw(gfx, &t),
            None => Ok(()),
        }
    }

    fn redraw(&self, gfx: &Gfx, target: &MenuTarget) -> Result<()> {
        let (glyph, label) = if target.pinned {
            (UNPIN_GLYPH, "Unpin from Glint")
        } else {
            (PIN_GLYPH, "Pin to Glint")
        };
        let row = Self::row_local();
        self.content.draw(gfx, |ctx| {
            gfx::with_glow(ctx, |ctx| {
                draw_text(
                    ctx,
                    gfx,
                    glyph,
                    &self.glyph_format,
                    Rect::new(row.x + 6.0, row.y, 24.0, row.h),
                    DWRITE_TEXT_ALIGNMENT_CENTER,
                    text_primary(),
                )?;
                draw_text(
                    ctx,
                    gfx,
                    label,
                    &self.label_format,
                    Rect::new(row.x + 38.0, row.y, row.w - 44.0, row.h),
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    text_primary(),
                )
            })
        })
    }

    /// Shrinks and fades it out.
    pub fn close(&self, animator: &Animator, reduced_motion: bool) -> Result<()> {
        if self.target.borrow_mut().take().is_none() {
            return Ok(());
        }
        self.hover.show(false)?;
        animator.pop_out(&self.root.cast()?, reduced_motion)
    }

    /// Closed at once, no animation (when the menu opens again).
    pub fn reset(&self) -> Result<()> {
        self.target.borrow_mut().take();
        self.hover.show(false)?;
        let v: Visual = self.root.cast()?;
        v.StopAnimation(h!("Opacity"))?;
        v.StopAnimation(h!("Scale"))?;
        v.SetOpacity(0.0)?;
        v.SetIsVisible(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MENU: Vector2 = Vector2 { X: 360.0, Y: 400.0 };

    #[test]
    fn opens_below_right_of_the_click() {
        let r = place(Vector2 { X: 40.0, Y: 100.0 }, MENU);
        assert_eq!((r.x, r.y), (40.0, 100.0));
    }

    #[test]
    fn stays_inside_the_menu() {
        // Near the right edge: shifted left.
        let r = place(Vector2 { X: 350.0, Y: 100.0 }, MENU);
        assert_eq!(r.right(), MENU.X - MARGIN);
        // Near the bottom: flipped above the click.
        let r = place(Vector2 { X: 40.0, Y: 390.0 }, MENU);
        assert_eq!(r.y, 390.0 - HEIGHT);
        assert!(r.bottom() <= MENU.Y - MARGIN);
        // Near the top-left corner: pushed in.
        let r = place(Vector2 { X: 0.0, Y: 0.0 }, MENU);
        assert_eq!((r.x, r.y), (MARGIN, MARGIN));
    }
}
