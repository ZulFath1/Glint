//! Extra decoration of terminal-style themes (gruvbox, IMPROVE.md 9.2):
//!
//! - a title strip at the top: `user@glint: ~` on the left, the clock on
//!   the right;
//! - a `── pinned ──────` label between the search box and the grid.
//!
//! Both are hidden for the other themes. The clock is repainted only
//! while the menu is open, once a minute (see `Ui::clock_tick`), so a
//! hidden menu still does nothing at all.

use std::cell::RefCell;

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::{Layout, Rect};
use crate::config::Rgb;

const TEXT_SIZE: f32 = 12.0;
const SIDE_PAD: f32 = 14.0;
const SECTION_LABEL: &str = "\u{2500}\u{2500} pinned";

/// The local time as "HH:MM" (24 hours, like a terminal prompt).
pub fn clock_text() -> String {
    // SAFETY: GetLocalTime only fills in a SYSTEMTIME it returns by value.
    let t = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

/// Milliseconds until the next full minute (at least 50, so a timer set
/// right at :59.999 doesn't fire twice).
pub fn ms_to_next_minute() -> u32 {
    // SAFETY: as above.
    let t = unsafe { GetLocalTime() };
    let into = u32::from(t.wSecond) * 1000 + u32::from(t.wMilliseconds);
    (60_000 - into.min(59_999)).max(50)
}

pub struct Chrome {
    title: Canvas,
    section: Canvas,
    user: String,
    /// The clock as painted (repainted only when the minute changes).
    clock: RefCell<String>,
    format: IDWriteTextFormat,
    bold: IDWriteTextFormat,
}

impl Chrome {
    pub fn new(c: &Compositor, gfx: &Gfx, front: &ContainerVisual) -> Result<Self> {
        let one = Vector2 { X: 1.0, Y: 1.0 };
        let title = Canvas::new(c, gfx, one)?;
        let section = Canvas::new(c, gfx, one)?;
        for v in [&title.visual, &section.visual] {
            v.SetIsVisible(false)?;
            front.Children()?.InsertAtTop(v)?;
        }
        let user = std::env::var("USERNAME")
            .unwrap_or_else(|_| "user".into())
            .to_lowercase();
        Ok(Self {
            title,
            section,
            user,
            clock: RefCell::new(String::new()),
            format: gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            bold: gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
        })
    }

    /// The section label, faded out with the grid while results show.
    pub fn section_visual(&self) -> Result<Visual> {
        self.section.visual.cast()
    }

    /// Is there a title strip (with a clock) in this layout?
    pub fn has_clock(l: &Layout) -> bool {
        l.title.w > 0.0
    }

    /// Places and paints both (or hides them, for non-terminal themes).
    pub fn refresh(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        for (canvas, r) in [(&self.title, l.title), (&self.section, l.section)] {
            let on = r.w > 0.0;
            canvas.visual.SetIsVisible(on)?;
            if on {
                canvas.resize(gfx, r.size())?;
                canvas.set_offset(gfx, r.x, r.y)?;
            }
        }
        if Self::has_clock(l) {
            self.paint_title(gfx)?;
        }
        if l.section.w > 0.0 {
            self.paint_section(gfx)?;
        }
        Ok(())
    }

    /// The minute may have changed: repaint the title strip if it did.
    pub fn tick(&self, gfx: &Gfx) -> Result<()> {
        if !self.title.visual.IsVisible()? || *self.clock.borrow() == clock_text() {
            return Ok(());
        }
        self.paint_title(gfx)
    }

    /// Draws `text` at `x`, returns where it ends.
    fn run(
        &self,
        ctx: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
        gfx: &Gfx,
        text: &str,
        format: &IDWriteTextFormat,
        (x, h): (f32, f32),
        color: Rgb,
    ) -> Result<f32> {
        let layout = gfx.text_layout(
            text,
            format,
            Vector2 { X: 2000.0, Y: h },
            DWRITE_TEXT_ALIGNMENT_LEADING,
            false,
        )?;
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: valid out-pointer to a local; then draws a live layout
        // with a live brush.
        unsafe {
            layout.GetMetrics(&mut m)?;
            let brush = gfx::brush(ctx, color, 1.0)?;
            ctx.DrawTextLayout(Vector2 { X: x, Y: 0.0 }, &layout, &brush, gfx::TEXT_OPTIONS);
        }
        Ok(x + m.widthIncludingTrailingWhitespace)
    }

    fn paint_title(&self, gfx: &Gfx) -> Result<()> {
        let size = self.title.size();
        let clock = clock_text();
        let p = gfx::theme().palette;
        self.title.draw(gfx, |ctx| {
            let full = Rect::new(0.0, 0.0, size.X, size.Y);
            gfx::round_rect(ctx, full, 0.0, Some((p.footer.color, p.footer.alpha)), None)?;
            // The strip's bottom edge.
            let w = gfx::theme().shape.divider;
            let line = Rect::new(0.0, size.Y - w, size.X, w);
            gfx::round_rect(ctx, line, 0.0, Some((p.divider.color, 1.0)), None)?;
            gfx::with_glow(ctx, |ctx| {
                let mut x = SIDE_PAD;
                x = self.run(ctx, gfx, &self.user, &self.bold, (x, size.Y), p.prompt)?;
                x = self.run(ctx, gfx, "@glint", &self.bold, (x, size.Y), text_primary())?;
                x = self.run(ctx, gfx, ": ", &self.format, (x, size.Y), text_primary())?;
                self.run(ctx, gfx, "~", &self.format, (x, size.Y), p.accent)?;
                let r = Rect::new(0.0, 0.0, size.X - SIDE_PAD, size.Y);
                super::footer::draw_text(
                    ctx,
                    gfx,
                    &clock,
                    &self.format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_TRAILING,
                    text_secondary(),
                )
            })
        })?;
        *self.clock.borrow_mut() = clock;
        Ok(())
    }

    fn paint_section(&self, gfx: &Gfx) -> Result<()> {
        let size = self.section.size();
        self.section.draw(gfx, |ctx| {
            let end = self.run(
                ctx,
                gfx,
                SECTION_LABEL,
                &self.format,
                (0.0, size.Y),
                text_secondary(),
            )?;
            // The rule runs on to the right edge: "── pinned ──────".
            let x = end + 8.0;
            let rule = Rect::new(x, (size.Y / 2.0).round(), (size.X - x).max(0.0), 1.0);
            gfx::round_rect(ctx, rule, 0.0, Some((text_secondary(), 0.55)), None)
        })
    }
}
