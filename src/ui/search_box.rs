//! The search box: a small but complete single-line text editor
//! (CLAUDE.md section 7).
//!
//! Two halves:
//! - [`Editor`] is plain Rust with no Windows calls: the text, the caret, the
//!   selection and every editing operation. It is unit tested at the bottom.
//! - [`SearchBox`] shows the editor: the rounded frame, the painted text,
//!   the blinking caret (a composition animation, not a timer), plus the
//!   clipboard and IME (Chinese/Japanese/Korean/Vietnamese input) glue.

use std::cell::{Cell, RefCell};

use windows::UI::Composition::{Compositor, ContainerVisual, SpriteVisual, Visual};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, POINT, RECT};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_RANGE, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_RESULTSTR,
    IME_COMPOSITION_STRING, ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME,
    ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, NI_COMPOSITIONSTR,
};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::Rect;
use super::shape::RoundRect;
use crate::config::Rgb;

// ---------------------------------------------------------------------------
// Editor: text + caret + selection (pure, tested)
// ---------------------------------------------------------------------------

/// What kind of character, for Ctrl+arrow word jumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The text to show, with positions in UTF-16 units (what DirectWrite uses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub text: String,
    pub caret: u32,
    /// Selected range (start, end), if any.
    pub selection: Option<(u32, u32)>,
    /// The IME composition being typed (start, end), shown underlined.
    pub composition: Option<(u32, u32)>,
}

/// Positions are byte offsets into `text`, always on a char boundary.
#[derive(Debug, Default, Clone)]
pub struct Editor {
    text: String,
    caret: usize,
    /// The other end of the selection. Equal to `caret` = nothing selected.
    anchor: usize,
    /// IME text being composed (not part of `text` yet).
    composition: String,
    /// First half of a character outside the BMP (emoji...), waiting for
    /// its second half in the next WM_CHAR.
    pending_high: Option<u16>,
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

impl Editor {
    /// The typed text (what search filters the apps with).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Nothing typed and nothing being composed.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.composition.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The selection as (start, end), ordered.
    fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.range();
        &self.text[a..b]
    }

    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.range();
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = a;
        true
    }

    fn move_to(&mut self, pos: usize, extend: bool) {
        self.caret = pos;
        if !extend {
            self.anchor = pos;
        }
    }

    /// Inserts text at the caret, replacing the selection. Line breaks are
    /// stripped (this is a single-line box), tabs become spaces, other
    /// control characters are dropped.
    pub fn insert(&mut self, s: &str) -> bool {
        let clean: String = s
            .chars()
            .filter(|&c| c != '\r' && c != '\n')
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        if clean.is_empty() && !self.has_selection() {
            return false;
        }
        self.delete_selection();
        self.text.insert_str(self.caret, &clean);
        self.move_to(self.caret + clean.len(), false);
        true
    }

    /// One UTF-16 unit from WM_CHAR. Returns true if the text changed.
    pub fn type_unit(&mut self, unit: u16) -> bool {
        match unit {
            0xD800..=0xDBFF => {
                self.pending_high = Some(unit);
                false
            }
            0xDC00..=0xDFFF => match self.pending_high.take() {
                Some(high) => match char::decode_utf16([high, unit]).next() {
                    Some(Ok(c)) => self.insert(c.encode_utf8(&mut [0; 4])),
                    _ => false,
                },
                None => false, // a lone second half: ignore it
            },
            _ => {
                self.pending_high = None;
                match char::from_u32(u32::from(unit)) {
                    Some(c) if !c.is_control() => self.insert(c.encode_utf8(&mut [0; 4])),
                    _ => false,
                }
            }
        }
    }

    fn prev_char(&self, pos: usize) -> usize {
        self.text[..pos]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_char(&self, pos: usize) -> usize {
        self.text[pos..]
            .chars()
            .next()
            .map_or(pos, |c| pos + c.len_utf8())
    }

    /// Ctrl+Left: skip spaces, then the run of same-kind characters.
    fn prev_word(&self, pos: usize) -> usize {
        let mut chars = self.text[..pos].char_indices().rev().peekable();
        while chars.next_if(|&(_, c)| class(c) == Class::Space).is_some() {}
        let Some(&(_, first)) = chars.peek() else {
            return 0;
        };
        let kind = class(first);
        let mut start = pos;
        while let Some((i, _)) = chars.next_if(|&(_, c)| class(c) == kind) {
            start = i;
        }
        start.min(pos)
    }

    /// Ctrl+Right (Windows style): skip the current run, then spaces, so the
    /// caret lands at the start of the next word.
    fn next_word(&self, pos: usize) -> usize {
        let rest = &self.text[pos..];
        let mut chars = rest.char_indices().peekable();
        if let Some(&(_, first)) = chars.peek() {
            let kind = class(first);
            if kind != Class::Space {
                while chars.next_if(|&(_, c)| class(c) == kind).is_some() {}
            }
        }
        while chars.next_if(|&(_, c)| class(c) == Class::Space).is_some() {}
        pos + chars.peek().map_or(rest.len(), |&(i, _)| i)
    }

    pub fn backspace(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        let start = if word {
            self.prev_word(self.caret)
        } else {
            self.prev_char(self.caret)
        };
        self.text.replace_range(start..self.caret, "");
        self.move_to(start, false);
        true
    }

    pub fn delete(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == self.text.len() {
            return false;
        }
        let end = if word {
            self.next_word(self.caret)
        } else {
            self.next_char(self.caret)
        };
        self.text.replace_range(self.caret..end, "");
        true
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        // Plain Left with a selection: go to its start.
        if self.has_selection() && !extend && !word {
            let (a, _) = self.range();
            return self.move_to(a, false);
        }
        let pos = if word {
            self.prev_word(self.caret)
        } else {
            self.prev_char(self.caret)
        };
        self.move_to(pos, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if self.has_selection() && !extend && !word {
            let (_, b) = self.range();
            return self.move_to(b, false);
        }
        let pos = if word {
            self.next_word(self.caret)
        } else {
            self.next_char(self.caret)
        };
        self.move_to(pos, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.text.len(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Ctrl+X: removes the selection and returns it.
    pub fn cut(&mut self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        let s = self.selected_text().to_string();
        self.delete_selection();
        Some(s)
    }

    /// The IME's in-progress text. Typing over a selection replaces it.
    pub fn set_composition(&mut self, s: &str) {
        if !s.is_empty() {
            self.delete_selection();
        }
        self.composition = s.to_string();
    }

    /// Mouse click / drag: put the caret at a UTF-16 position.
    pub fn set_caret_utf16(&mut self, pos: u32, extend: bool) {
        let mut units = 0;
        let mut byte = self.text.len();
        for (i, c) in self.text.char_indices() {
            if units >= pos {
                byte = i;
                break;
            }
            units += c.len_utf16() as u32;
        }
        self.move_to(byte, extend);
    }

    pub fn display(&self) -> Display {
        let caret16 = utf16_len(&self.text[..self.caret]);
        if !self.composition.is_empty() {
            let mut text = self.text.clone();
            text.insert_str(self.caret, &self.composition);
            let end = caret16 + utf16_len(&self.composition);
            return Display {
                text,
                caret: end,
                selection: None,
                composition: Some((caret16, end)),
            };
        }
        let (a, b) = self.range();
        Display {
            text: self.text.clone(),
            caret: caret16,
            selection: self
                .has_selection()
                .then(|| (utf16_len(&self.text[..a]), utf16_len(&self.text[..b]))),
            composition: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/// Text on the clipboard, if any.
pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and always closed again below. The
    // locked memory is read only within its GlobalSize and unlocked again.
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let text = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let global = HGLOBAL(handle.0);
            let ptr = GlobalLock(global) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let max = GlobalSize(global) / 2;
            let units = std::slice::from_raw_parts(ptr, max);
            let len = units.iter().position(|&u| u == 0).unwrap_or(max);
            let text = String::from_utf16_lossy(&units[..len]);
            let _ = GlobalUnlock(global);
            Some(text)
        })();
        let _ = CloseClipboard();
        text
    }
}

pub fn set_clipboard_text(hwnd: HWND, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: allocates movable global memory of exactly the right size,
    // copies into it while locked, then hands it to the clipboard (which
    // then owns it). The clipboard is always closed again.
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) {
            let ptr = GlobalLock(global) as *mut u16;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
                let _ = GlobalUnlock(global);
                if SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(global.0))).is_err() {
                    let _ = windows::Win32::Foundation::GlobalFree(Some(global));
                }
            }
        }
        let _ = CloseClipboard();
    }
}

// ---------------------------------------------------------------------------
// The visible search box
// ---------------------------------------------------------------------------

const TEXT_SIZE: f32 = 14.0;
const ICON_SIZE: f32 = 16.0;
const MAGNIFIER: &str = "\u{E721}";
const PLACEHOLDER: &str = "Search";
/// Where the text starts inside the box (after the magnifier).
const TEXT_LEFT: f32 = 40.0;
const TEXT_RIGHT_PAD: f32 = 12.0;
const CARET_WIDTH: f32 = 1.5;
const CARET_HEIGHT: f32 = 18.0;
const SELECTION_ALPHA: f32 = 0.35;
/// Terminal themes: a `❯` prompt instead of the magnifier, the text a bit
/// closer to it, and a block caret one character wide.
const PROMPT: &str = "\u{276F}";
const TERMINAL_TEXT_LEFT: f32 = 34.0;
const BLOCK_CARET_WIDTH: f32 = 8.4;
const BLOCK_CARET_ALPHA: f32 = 0.75;

/// Where the text starts inside the box, for the active theme.
fn text_left() -> f32 {
    if gfx::theme().terminal {
        TERMINAL_TEXT_LEFT
    } else {
        TEXT_LEFT
    }
}

/// The caret's width for the active theme.
fn caret_width() -> f32 {
    if gfx::theme().terminal {
        BLOCK_CARET_WIDTH
    } else {
        CARET_WIDTH
    }
}

pub struct SearchBox {
    rect: Cell<Rect>,
    frame: RoundRect,
    icon: Canvas,
    text: Canvas,
    caret: SpriteVisual,
    format: IDWriteTextFormat,
    icon_format: IDWriteTextFormat,
    accent: Cell<Rgb>,
    focused: Cell<bool>,
    pub editor: RefCell<Editor>,
    /// Horizontal scroll of the text (DIPs), to keep the caret visible.
    scroll: Cell<f32>,
    /// Caret x inside the text area, before scrolling.
    caret_x: Cell<f32>,
    /// The last laid-out text, for mouse hit testing.
    layout: RefCell<Option<IDWriteTextLayout>>,
}

impl SearchBox {
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        parent: &ContainerVisual,
        rect: Rect,
        accent: Rgb,
    ) -> Result<Self> {
        let frame = RoundRect::new(c, 10.0, 1.0)?;
        let icon = Canvas::new(c, gfx, Vector2 { X: 20.0, Y: rect.h })?;
        let text = Canvas::new(c, gfx, text_size(rect))?;
        let caret_brush = c.CreateColorBrushWithColor(gfx::color(accent, 1.0))?;
        let caret = c.CreateSpriteVisual()?;
        caret.SetBrush(&caret_brush)?;
        caret.SetSize(Vector2 {
            X: CARET_WIDTH,
            Y: CARET_HEIGHT,
        })?;
        caret.SetOpacity(0.0)?;
        let children = parent.Children()?;
        children.InsertAtTop(&frame.visual)?;
        children.InsertAtTop(&icon.visual)?;
        children.InsertAtTop(&text.visual)?;
        children.InsertAtTop(&caret)?;

        let s = Self {
            rect: Cell::new(rect),
            frame,
            icon,
            text,
            caret,
            format: gfx.text_format(TEXT_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            icon_format: gfx.icon_format(ICON_SIZE)?,
            accent: Cell::new(accent),
            focused: Cell::new(true),
            editor: RefCell::new(Editor::default()),
            scroll: Cell::new(0.0),
            caret_x: Cell::new(0.0),
            layout: RefCell::new(None),
        };
        s.set_theme()?;
        Ok(s)
    }

    /// The active theme changed: fill, border, corner radius and caret.
    /// Call `set_rect` afterwards (it repaints).
    pub fn set_theme(&self) -> Result<()> {
        let t = gfx::theme();
        self.frame.set_stroke_width(t.shape.search_border)?;
        self.frame.set_fill(gfx::paint(t.palette.field))?;
        self.caret.SetSize(Vector2 {
            X: caret_width(),
            Y: CARET_HEIGHT,
        })?;
        self.set_focused(self.focused.get())?;
        self.set_caret_color()
    }

    fn set_caret_color(&self) -> Result<()> {
        let alpha = if gfx::theme().terminal {
            BLOCK_CARET_ALPHA
        } else {
            1.0
        };
        self.caret
            .Brush()?
            .cast::<windows::UI::Composition::CompositionColorBrush>()?
            .SetColor(gfx::color(self.accent.get(), alpha))
    }

    pub fn rect(&self) -> Rect {
        self.rect.get()
    }

    pub fn caret_visual(&self) -> Result<Visual> {
        self.caret.cast()
    }

    /// A new accent color (config reload): caret, focused border, selection.
    pub fn set_accent(&self, gfx: &Gfx, accent: Rgb) -> Result<()> {
        self.accent.set(accent);
        self.set_caret_color()?;
        self.set_focused(self.focused.get())?;
        self.redraw_text(gfx)
    }

    /// The search box has the keyboard: accent border, caret shown.
    pub fn set_focused(&self, focused: bool) -> Result<()> {
        self.focused.set(focused);
        let border = if focused {
            gfx::color(self.accent.get(), 0.9)
        } else {
            gfx::paint(gfx::theme().palette.field_border)
        };
        self.frame.set_stroke(border)?;
        self.caret.SetIsVisible(focused)
    }

    /// Positions everything (after a layout or DPI change) and repaints.
    pub fn set_rect(&self, gfx: &Gfx, rect: Rect) -> Result<()> {
        self.rect.set(rect);
        self.frame.set_radius(gfx::theme().search_radius(rect.h));
        self.frame.set_rect(rect)?;
        self.icon.resize(gfx, Vector2 { X: 20.0, Y: rect.h })?;
        self.icon.set_offset(gfx, rect.x + 12.0, rect.y)?;
        self.text.resize(gfx, text_size(rect))?;
        self.text.set_offset(gfx, rect.x + text_left(), rect.y)?;
        self.redraw(gfx)
    }

    pub fn redraw(&self, gfx: &Gfx) -> Result<()> {
        let size = self.icon.size();
        let terminal = gfx::theme().terminal;
        let (glyph, format, color, alpha) = if terminal {
            (PROMPT, &self.format, gfx::theme().palette.prompt, 1.0)
        } else {
            (MAGNIFIER, &self.icon_format, text_secondary(), 0.9)
        };
        self.icon.draw(gfx, |ctx| {
            let layout =
                gfx.text_layout(glyph, format, size, DWRITE_TEXT_ALIGNMENT_LEADING, false)?;
            let brush = gfx::brush(ctx, color, alpha)?;
            // SAFETY: draws a live text layout with a live brush.
            unsafe { ctx.DrawTextLayout(Vector2::zero(), &layout, &brush, gfx::TEXT_OPTIONS) };
            Ok(())
        })?;
        self.redraw_text(gfx)
    }

    /// Repaints the text after an edit and moves the caret.
    pub fn redraw_text(&self, gfx: &Gfx) -> Result<()> {
        let d = self.editor.borrow().display();
        let size = self.text.size();
        let view = size.X;
        let empty = d.text.is_empty();
        let placeholder = if gfx::theme().terminal {
            "search apps"
        } else {
            PLACEHOLDER
        };
        let shown = if empty { placeholder } else { &d.text };
        let layout = gfx.text_layout(
            shown,
            &self.format,
            Vector2 {
                X: 10_000.0,
                Y: size.Y,
            },
            DWRITE_TEXT_ALIGNMENT_LEADING,
            false,
        )?;
        if let Some((a, b)) = d.composition {
            // SAFETY: plain COM call on a live layout.
            unsafe {
                layout.SetUnderline(
                    true,
                    DWRITE_TEXT_RANGE {
                        startPosition: a,
                        length: b - a,
                    },
                )?;
            }
        }
        let caret_x = if empty { 0.0 } else { x_at(&layout, d.caret)? };
        let width = if empty { 0.0 } else { text_width(&layout)? };

        // Keep the caret in view, and don't leave empty space on the right
        // after deleting.
        let mut scroll = self.scroll.get();
        if caret_x - scroll > view - 2.0 {
            scroll = caret_x - view + 2.0;
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        scroll = scroll.min((width - view + 2.0).max(0.0)).max(0.0);
        self.scroll.set(scroll);
        self.caret_x.set(caret_x);

        let selection = match d.selection {
            Some((a, b)) => Some((x_at(&layout, a)?, x_at(&layout, b)?)),
            None => None,
        };
        let accent = self.accent.get();
        self.text.draw(gfx, |ctx| {
            if let Some((a, b)) = selection {
                let r = Rect::new(a - scroll, (size.Y - 20.0) / 2.0, b - a, 20.0);
                let brush = gfx::brush(ctx, accent, SELECTION_ALPHA)?;
                // SAFETY: plain drawing call with live objects.
                unsafe { ctx.FillRectangle(&gfx::d2d_rect(r), &brush) };
            }
            let (color, alpha) = if empty {
                (text_secondary(), 0.75)
            } else {
                (text_primary(), 1.0)
            };
            let brush = gfx::brush(ctx, color, alpha)?;
            gfx::with_glow(ctx, |ctx| {
                // SAFETY: draws a live text layout with a live brush.
                unsafe {
                    ctx.DrawTextLayout(
                        Vector2 { X: -scroll, Y: 0.0 },
                        &layout,
                        &brush,
                        gfx::TEXT_OPTIONS,
                    )
                };
                Ok(())
            })
        })?;
        *self.layout.borrow_mut() = (!empty).then_some(layout);

        let r = self.rect.get();
        let s = gfx.scale();
        self.caret.SetOffset(Vector3 {
            X: gfx::snap(r.x + text_left() + caret_x - scroll, s),
            Y: gfx::snap(r.y + (r.h - CARET_HEIGHT) / 2.0, s),
            Z: 0.0,
        })
    }

    /// The UTF-16 text position under a point (DIPs, menu coordinates).
    pub fn position_at(&self, p: Vector2) -> Option<u32> {
        let layout = self.layout.borrow();
        let Some(layout) = layout.as_ref() else {
            return Some(0);
        };
        let r = self.rect.get();
        let x = p.X - r.x - text_left() + self.scroll.get();
        let mut trailing = windows::core::BOOL::default();
        let mut inside = windows::core::BOOL::default();
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: valid out-pointers to locals.
        unsafe { layout.HitTestPoint(x, r.h / 2.0, &mut trailing, &mut inside, &mut m) }.ok()?;
        Some(m.textPosition + if trailing.as_bool() { m.length } else { 0 })
    }

    /// The caret rectangle in DIPs (menu coordinates), for the IME windows.
    pub fn caret_rect(&self) -> Rect {
        let r = self.rect.get();
        Rect::new(
            r.x + text_left() + self.caret_x.get() - self.scroll.get(),
            r.y + (r.h - CARET_HEIGHT) / 2.0,
            caret_width(),
            CARET_HEIGHT,
        )
    }
}

fn text_size(rect: Rect) -> Vector2 {
    Vector2 {
        X: (rect.w - text_left() - TEXT_RIGHT_PAD).max(1.0),
        Y: rect.h,
    }
}

/// x of a UTF-16 position in a laid-out line.
fn x_at(layout: &IDWriteTextLayout, pos: u32) -> Result<f32> {
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    // SAFETY: valid out-pointers to locals.
    unsafe { layout.HitTestTextPosition(pos, false, &mut x, &mut y, &mut m)? };
    Ok(x)
}

fn text_width(layout: &IDWriteTextLayout) -> Result<f32> {
    let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    // SAFETY: valid out-pointer to a local.
    unsafe { layout.GetMetrics(&mut m)? };
    Ok(m.widthIncludingTrailingWhitespace)
}

// ---------------------------------------------------------------------------
// IME
// ---------------------------------------------------------------------------

/// Reads one of the IME's strings (the composition or the result).
pub fn ime_string(hwnd: HWND, which: IME_COMPOSITION_STRING) -> Option<String> {
    // SAFETY: the context is released again below; the buffer is sized
    // from the IME's own answer (in bytes).
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return None;
        }
        let bytes = ImmGetCompositionStringW(himc, which, None, 0);
        let text = if bytes > 0 {
            let mut buf = vec![0u16; bytes as usize / 2];
            let got =
                ImmGetCompositionStringW(himc, which, Some(buf.as_mut_ptr().cast()), bytes as u32);
            (got > 0).then(|| String::from_utf16_lossy(&buf[..got as usize / 2]))
        } else {
            Some(String::new())
        };
        let _ = ImmReleaseContext(hwnd, himc);
        text
    }
}

pub const IME_RESULT: IME_COMPOSITION_STRING = GCS_RESULTSTR;
pub const IME_COMPOSITION: IME_COMPOSITION_STRING = GCS_COMPSTR;

/// Puts the IME's candidate list (and composition window, for IMEs that
/// use one) right at the caret. `caret` is in window pixels.
pub fn ime_place(hwnd: HWND, caret: RECT) {
    // SAFETY: the context is released again below; the structs are locals.
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return;
        }
        let at = POINT {
            x: caret.left,
            y: caret.bottom,
        };
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT,
            ptCurrentPos: POINT {
                x: caret.left,
                y: caret.top,
            },
            rcArea: RECT::default(),
        };
        let _ = ImmSetCompositionWindow(himc, &composition);
        // CFS_EXCLUDE: show the list near `at`, but never over the caret.
        let candidate = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE,
            ptCurrentPos: at,
            rcArea: caret,
        };
        let _ = ImmSetCandidateWindow(himc, &candidate);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

/// Throws away a half-typed composition (the menu is closing).
pub fn ime_cancel(hwnd: HWND) {
    // SAFETY: the context is released again below.
    unsafe {
        let himc = ImmGetContext(hwnd);
        if himc.is_invalid() {
            return;
        }
        let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
        let _ = ImmReleaseContext(hwnd, himc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Editor {
        let mut e = Editor::default();
        for u in s.encode_utf16() {
            e.type_unit(u);
        }
        e
    }

    #[test]
    fn typing_and_backspace() {
        let mut e = typed("hello");
        assert_eq!(e.text(), "hello");
        assert!(e.backspace(false));
        assert_eq!(e.text(), "hell");
        e.home(false);
        assert!(!e.backspace(false)); // nothing before the caret
        assert!(e.delete(false));
        assert_eq!(e.text(), "ell");
    }

    #[test]
    fn surrogate_pairs_make_one_character() {
        let mut e = typed("a😀b");
        assert_eq!(e.text(), "a😀b");
        e.left(false, false); // before b
        e.backspace(false); // removes the whole emoji
        assert_eq!(e.text(), "ab");
        // A lone second half is ignored.
        assert!(!e.type_unit(0xDE00));
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn control_characters_are_not_typed() {
        let mut e = Editor::default();
        for u in [0x08u16, 0x09, 0x0D, 0x1B, 0x01, 0x7F] {
            assert!(!e.type_unit(u));
        }
        assert!(e.is_empty());
    }

    #[test]
    fn word_jumps_like_windows() {
        let mut e = typed("open the  file.txt");
        e.left(true, false);
        assert_eq!(&e.text()[e.caret..], "txt");
        e.left(true, false);
        assert_eq!(&e.text()[e.caret..], ".txt");
        e.left(true, false);
        assert_eq!(&e.text()[e.caret..], "file.txt");
        e.left(true, false);
        assert_eq!(&e.text()[e.caret..], "the  file.txt");
        e.right(true, false);
        assert_eq!(&e.text()[e.caret..], "file.txt");
        e.home(false);
        e.right(true, false);
        assert_eq!(&e.text()[e.caret..], "the  file.txt");
    }

    #[test]
    fn ctrl_backspace_and_ctrl_delete_remove_words() {
        let mut e = typed("hello big world");
        e.backspace(true);
        assert_eq!(e.text(), "hello big ");
        e.backspace(true);
        assert_eq!(e.text(), "hello ");
        e.home(false);
        e.delete(true);
        assert_eq!(e.text(), "");
    }

    #[test]
    fn shift_selection_and_replace() {
        let mut e = typed("abcdef");
        e.left(false, true);
        e.left(false, true);
        assert_eq!(e.selected_text(), "ef");
        e.type_unit(u16::from(b'X'));
        assert_eq!(e.text(), "abcdX");
        e.home(true);
        assert_eq!(e.selected_text(), "abcdX");
        // Plain Right collapses the selection to its end.
        e.right(false, false);
        assert!(!e.has_selection());
        assert_eq!(e.caret, 5);
    }

    #[test]
    fn select_all_cut_and_paste() {
        let mut e = typed("one two");
        e.select_all();
        assert_eq!(e.cut().as_deref(), Some("one two"));
        assert!(e.is_empty());
        assert!(e.cut().is_none());
        e.insert("line1\r\nline2\n\tx");
        assert_eq!(e.text(), "line1line2 x");
    }

    #[test]
    fn ime_composition_is_shown_but_not_committed() {
        let mut e = typed("ab");
        e.left(false, false);
        e.set_composition("にほ");
        let d = e.display();
        assert_eq!(d.text, "aにほb");
        assert_eq!(d.composition, Some((1, 3)));
        assert_eq!(d.caret, 3);
        assert_eq!(e.text(), "ab");
        assert!(!e.is_empty());
        e.set_composition("");
        e.insert("日本");
        assert_eq!(e.text(), "a日本b");
    }

    #[test]
    fn display_positions_are_utf16() {
        let mut e = typed("😀x");
        e.home(false);
        e.right(false, true);
        let d = e.display();
        assert_eq!(d.selection, Some((0, 2))); // the emoji is 2 UTF-16 units
        e.set_caret_utf16(2, false);
        assert_eq!(&e.text()[e.caret..], "x");
        e.set_caret_utf16(99, false);
        assert_eq!(e.caret, e.text().len());
    }
}
