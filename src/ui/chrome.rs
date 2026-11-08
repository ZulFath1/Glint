//! Extra decoration some themes add around the menu's content
//! (IMPROVE.md 9.1 and 9.2):
//!
//! - terminal themes (gruvbox): a title strip at the top (`user@glint: ~`
//!   on the left, the clock on the right) and a `── pinned ──────` label
//!   between the search box and the grid;
//! - the list layout (nord): a "PINNED" label above the icon strip;
//! - the sidebar layout (tokyo-night): a header with a greeting
//!   ("Good evening, Fathi"), a big clock and the date.
//!
//! Each part is hidden when the layout has no place for it. The clocks are
//! repainted only while the menu is open, once a minute (see
//! `Ui::clock_tick`), so a hidden menu still does nothing at all.

use std::cell::RefCell;

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{Interface, Result};
use windows_numerics::Vector2;

use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::{Layout, Rect};
use crate::config::Rgb;
use crate::theme::LayoutKind;

const TEXT_SIZE: f32 = 12.0;
const SIDE_PAD: f32 = 14.0;
const TERMINAL_LABEL: &str = "\u{2500}\u{2500} pinned";
const LIST_LABEL: &str = "PINNED";
/// The sidebar header: greeting, a 36 px bold clock, the date.
const GREETING_SIZE: f32 = 13.0;
const CLOCK_SIZE: f32 = 36.0;
const GREETING_H: f32 = 20.0;
const CLOCK_H: f32 = 44.0;
/// Room the clock and date take on the right of the header.
const CLOCK_AREA_W: f32 = 150.0;
/// The greeting's line starts this far below the clock's box, so the
/// text lines up with the top of the big digits.
const GREETING_TOP: f32 = 4.0;

fn now() -> SYSTEMTIME {
    // SAFETY: GetLocalTime only fills in a SYSTEMTIME it returns by value.
    unsafe { GetLocalTime() }
}

/// The local time as "HH:MM" (24 hours, like a terminal prompt).
pub fn clock_text() -> String {
    let t = now();
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

/// Milliseconds until the next full minute (at least 50, so a timer set
/// right at :59.999 doesn't fire twice).
pub fn ms_to_next_minute() -> u32 {
    let t = now();
    let into = u32::from(t.wSecond) * 1000 + u32::from(t.wMilliseconds);
    (60_000 - into.min(59_999)).max(50)
}

/// "Good morning" (5–11), "Good afternoon" (12–17), "Good evening".
pub fn greeting(hour: u16) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// "Sunday, 8 November".
pub fn date_text(t: &SYSTEMTIME) -> String {
    const DAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let day = DAYS.get(usize::from(t.wDayOfWeek)).unwrap_or(&"");
    let month = MONTHS
        .get(usize::from(t.wMonth).saturating_sub(1))
        .unwrap_or(&"");
    format!("{day}, {} {month}", t.wDay)
}

pub struct Chrome {
    title: Canvas,
    section: Canvas,
    header: Canvas,
    user: String,
    /// The first name, for the greeting.
    first_name: String,
    /// The clock as painted (repainted only when the minute changes).
    clock: RefCell<String>,
    format: IDWriteTextFormat,
    bold: IDWriteTextFormat,
    greeting_format: IDWriteTextFormat,
    clock_format: IDWriteTextFormat,
}

impl Chrome {
    /// `display_name` is the signed-in user's name ("First Last").
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        front: &ContainerVisual,
        display_name: &str,
    ) -> Result<Self> {
        let one = Vector2 { X: 1.0, Y: 1.0 };
        let title = Canvas::new(c, gfx, one)?;
        let section = Canvas::new(c, gfx, one)?;
        let header = Canvas::new(c, gfx, one)?;
        for v in [&title.visual, &section.visual, &header.visual] {
            v.SetIsVisible(false)?;
            front.Children()?.InsertAtTop(v)?;
        }
        let user = std::env::var("USERNAME")
            .unwrap_or_else(|_| "user".into())
            .to_lowercase();
        let first_name = display_name
            .split_whitespace()
            .next()
            .unwrap_or(display_name)
            .to_string();
        Ok(Self {
            title,
            section,
            header,
            user,
            first_name,
            clock: RefCell::new(String::new()),
            format: gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            bold: gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            greeting_format: gfx.text_format(GREETING_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            clock_format: gfx.text_format(CLOCK_SIZE, DWRITE_FONT_WEIGHT_BOLD)?,
        })
    }

    /// The section label. Faded out with the grid while results show (in
    /// the list layout it always shows).
    pub fn section_visual(&self) -> Result<Visual> {
        self.section.visual.cast()
    }

    /// Moves the section label to its place in `l` (no repaint).
    pub fn move_section(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        if l.section.w > 0.0 {
            self.section.set_offset(gfx, l.section.x, l.section.y)?;
        }
        Ok(())
    }

    /// Does this layout show a clock (title strip or sidebar header)?
    pub fn has_clock(l: &Layout) -> bool {
        l.title.w > 0.0 || l.header.w > 0.0
    }

    /// Places and paints every part this layout has (and hides the rest).
    pub fn refresh(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        for (canvas, r) in [
            (&self.title, l.title),
            (&self.section, l.section),
            (&self.header, l.header),
        ] {
            let on = r.w > 0.0;
            canvas.visual.SetIsVisible(on)?;
            if on {
                canvas.resize(gfx, r.size())?;
                canvas.set_offset(gfx, r.x, r.y)?;
            }
        }
        if l.section.w > 0.0 {
            self.paint_section(gfx, l.kind)?;
        }
        self.paint_clocks(gfx)
    }

    /// The minute may have changed: repaint the clocks if it did.
    pub fn tick(&self, gfx: &Gfx) -> Result<()> {
        if *self.clock.borrow() == clock_text() {
            return Ok(());
        }
        self.paint_clocks(gfx)
    }

    fn paint_clocks(&self, gfx: &Gfx) -> Result<()> {
        if self.title.visual.IsVisible()? {
            self.paint_title(gfx)?;
        }
        if self.header.visual.IsVisible()? {
            self.paint_header(gfx)?;
        }
        *self.clock.borrow_mut() = clock_text();
        Ok(())
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
        })
    }

    /// "Good evening, Fathi", the time in big bold digits, the date.
    fn paint_header(&self, gfx: &Gfx) -> Result<()> {
        let size = self.header.size();
        let t = now();
        let hello = format!("{}, {}", greeting(t.wHour), self.first_name);
        let clock = format!("{:02}:{:02}", t.wHour, t.wMinute);
        let date = date_text(&t);
        let accent = gfx::theme().palette.accent;
        self.header.draw(gfx, |ctx| {
            gfx::with_glow(ctx, |ctx| {
                // The clock and the date on the right (right-aligned); the
                // greeting at the top left.
                let (lead, right) = (
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    DWRITE_TEXT_ALIGNMENT_TRAILING,
                );
                let top = ((size.Y - CLOCK_H - GREETING_H) / 2.0).max(0.0);
                super::footer::draw_text(
                    ctx,
                    gfx,
                    &clock,
                    &self.clock_format,
                    Rect::new(0.0, top, size.X, CLOCK_H),
                    right,
                    text_primary(),
                )?;
                super::footer::draw_text(
                    ctx,
                    gfx,
                    &date,
                    &self.greeting_format,
                    Rect::new(0.0, top + CLOCK_H, size.X, GREETING_H),
                    right,
                    accent,
                )?;
                // Top left, level with the top of the clock's digits.
                super::footer::draw_text(
                    ctx,
                    gfx,
                    &hello,
                    &self.greeting_format,
                    Rect::new(
                        0.0,
                        top + GREETING_TOP,
                        (size.X - CLOCK_AREA_W).max(0.0),
                        GREETING_H,
                    ),
                    lead,
                    text_secondary(),
                )
            })
        })
    }

    fn paint_section(&self, gfx: &Gfx, kind: LayoutKind) -> Result<()> {
        let size = self.section.size();
        let label = if kind == LayoutKind::List {
            LIST_LABEL
        } else {
            TERMINAL_LABEL
        };
        self.section.draw(gfx, |ctx| {
            let end = self.run(ctx, gfx, label, &self.bold, (0.0, size.Y), text_secondary())?;
            if kind == LayoutKind::List {
                return Ok(());
            }
            // The rule runs on to the right edge: "── pinned ──────".
            let x = end + 8.0;
            let rule = Rect::new(x, (size.Y / 2.0).round(), (size.X - x).max(0.0), 1.0);
            gfx::round_rect(ctx, rule, 0.0, Some((text_secondary(), 0.55)), None)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_follow_the_time_of_day() {
        assert_eq!(greeting(6), "Good morning");
        assert_eq!(greeting(13), "Good afternoon");
        assert_eq!(greeting(21), "Good evening");
        assert_eq!(greeting(2), "Good evening");
    }

    #[test]
    fn dates_read_naturally() {
        let t = SYSTEMTIME {
            wYear: 2026,
            wMonth: 11,
            wDayOfWeek: 0,
            wDay: 8,
            ..Default::default()
        };
        assert_eq!(date_text(&t), "Sunday, 8 November");
    }
}
