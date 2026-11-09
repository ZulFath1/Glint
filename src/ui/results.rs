//! The search results list (SUPERSEARCH.md 2.4): up to 10 rows, 48 px
//! tall, each with an icon (the app's own, or a glyph), a title, a smaller
//! subtitle, and on the selected row a hint for Enter ("Open  Enter").
//! Small uppercase section headers ("APPS", "SETTINGS") go between groups
//! of rows from different providers. Or a single "No results" row.
//!
//! The rows (and the message) are created once and reused for every
//! search; typing only repaints the rows whose content changed. Section
//! headers are created the first time they are needed and then kept.
//! Rows never wait for an icon: a row whose icon isn't cached yet is
//! painted without one, and the icon is filled in a moment later (see
//! `Ui::load_icons`).

use std::cell::{Cell, RefCell};

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap1, ID2D1DeviceContext};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextFormat,
};
use windows::core::{Interface, Result, h};
use windows_numerics::Vector2;

use super::footer::draw_text;
use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::{Layout, MAX_RESULTS, Rect};
use super::shape::RoundRect;
use crate::config::Rgb;

/// Shown in the message row of the list layout when nothing is typed and
/// there is nothing to suggest yet.
pub const TYPE_TO_SEARCH: &str = "Type to search";
pub const NO_RESULTS: &str = "No results";

/// Icon size and place inside a row (DIPs).
const ICON: f32 = 24.0;
const ICON_LEFT: f32 = 14.0;
const ICON_GAP: f32 = 12.0;
const TEXT_RIGHT: f32 = 12.0;
/// Glyph icons are drawn a bit smaller than the box they sit in.
const GLYPH_SCALE: f32 = 0.75;

/// Title over subtitle, as a block centered in the row.
const TITLE_SIZE: f32 = 14.0;
const SUBTITLE_SIZE: f32 = 12.0;
const TITLE_H: f32 = 20.0;
const SUBTITLE_H: f32 = 17.0;
const LINE_GAP: f32 = 1.0;

/// The hint on the selected row: "<verb>" then an "Enter" key cap.
const HINT_SIZE: f32 = 12.0;
const KEY_W: f32 = 42.0;
const KEY_H: f32 = 20.0;
const KEY_RADIUS: f32 = 5.0;
const KEY_FILL_ALPHA: f32 = 0.16;
const VERB_W: f32 = 64.0;
const HINT_GAP: f32 = 6.0;
const HINT_W: f32 = VERB_W + HINT_GAP + KEY_W + 8.0;

/// Section headers: small, uppercase, in the subtext color.
const HEADER_SIZE: f32 = 11.0;
const HEADER_LEFT: f32 = 14.0;

const MESSAGE_SIZE: f32 = 13.0;
/// The list container is this big (DIPs), so its clip only ever cuts at
/// the bottom.
const CLIP_BOX: f32 = 10_000.0;

/// The size of a row or header surface that is not in use. Drawing
/// surfaces share big atlas textures, and 10 full rows + headers need an
/// extra atlas page (measured: +5 MB idle). Shrinking unused ones while the
/// menu is closed hands that room back; the objects themselves stay.
const PARKED: Vector2 = Vector2 { X: 1.0, Y: 1.0 };

/// The rows' icon size for the active theme (bigger in the list layout).
pub fn icon_size() -> f32 {
    if is_list_layout() {
        super::layout::LIST_ICON
    } else {
        ICON
    }
}

/// The list layout highlights the selected row with the accent color.
fn is_list_layout() -> bool {
    gfx::theme().layout == crate::theme::LayoutKind::List
}

/// The picture on the left of a row.
#[derive(Debug, Clone, Copy)]
pub enum RowIcon<'a> {
    /// An app's icon (None = not loaded yet).
    App(Option<&'a ID2D1Bitmap1>),
    /// A character of Segoe Fluent Icons.
    Glyph(char),
}

/// What one row shows.
#[derive(Debug, Clone, Copy)]
pub struct RowView<'a> {
    /// Tells two rows with the same text apart (an app's ID).
    pub key: &'a str,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub icon: RowIcon<'a>,
    /// The word in the selected row's hint ("Open", "Copy", "Run").
    pub verb: &'a str,
    /// Waiting for a second Enter: this replaces the subtitle.
    pub confirm: Option<&'a str>,
}

impl RowView<'_> {
    /// Everything that changes how the row looks, as one string.
    fn fingerprint(&self, selected: bool) -> String {
        let icon = match self.icon {
            RowIcon::App(Some(_)) => "app+".to_string(),
            RowIcon::App(None) => "app-".to_string(),
            RowIcon::Glyph(c) => c.to_string(),
        };
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{icon}\u{1}{}\u{1}{}\u{1}{selected}",
            self.key,
            self.title,
            self.subtitle,
            self.verb,
            self.confirm.unwrap_or("")
        )
    }
}

pub struct Results {
    compositor: Compositor,
    root: ContainerVisual,
    selection: RoundRect,
    /// The "Enter" key cap on the selected row: drawn by the compositor,
    /// not Direct2D. It would be the menu's only Direct2D shape, and the
    /// first one costs about 4 MB of idle RAM (measured), so text and
    /// bitmaps are all the rows paint.
    key: RoundRect,
    rows: Vec<Canvas>,
    /// How big a row is (DIPs). Rows not in use are "parked" at `PARKED`
    /// size, see [`Results::park`].
    row_size: Cell<Vector2>,
    /// Created on first use; one per header place.
    headers: RefCell<Vec<Canvas>>,
    /// What each header shows (to skip repainting it).
    header_text: RefCell<Vec<String>>,
    message: Canvas,
    shown: RefCell<Vec<Option<String>>>,
    /// Where the visible rows are (menu coordinates).
    rects: RefCell<Vec<Rect>>,
    selected: Cell<Option<usize>>,
    title_format: IDWriteTextFormat,
    subtitle_format: IDWriteTextFormat,
    hint_format: IDWriteTextFormat,
    header_format: IDWriteTextFormat,
    message_format: IDWriteTextFormat,
    /// Glyph icons, sized for the grid / list layout's icon box.
    glyph_format: RefCell<(f32, IDWriteTextFormat)>,
    /// Icons in one color (`icon_tint`).
    icon_tint: Cell<bool>,
    /// The selected row's fill in the list layout, and the confirm text.
    accent: Cell<Rgb>,
    /// What the message row says ("No results", "Type to search").
    message_text: RefCell<&'static str>,
}

impl Results {
    /// Creates the rows (hidden: the menu starts with the grid).
    /// `footer` is the footer's background visual: the list is cut off at
    /// its top edge, so rows never show over the footer while the menu
    /// glides to a new height.
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        parent: &ContainerVisual,
        l: &Layout,
        footer: &Visual,
    ) -> Result<Self> {
        let root = c.CreateContainerVisual()?;
        root.SetOpacity(0.0)?;
        // A big box, cut at the bottom by "how far the footer's top is from
        // our bottom". The compositor works this out every frame by itself,
        // also while the footer glides.
        root.SetSize(Vector2 {
            X: CLIP_BOX,
            Y: CLIP_BOX,
        })?;
        let clip = c.CreateInsetClip()?;
        let cut =
            c.CreateExpressionAnimationWithExpression(h!("Max(0, root.Size.Y - footer.Offset.Y)"))?;
        cut.SetReferenceParameter(h!("root"), &root)?;
        cut.SetReferenceParameter(h!("footer"), footer)?;
        clip.StartAnimation(h!("BottomInset"), &cut)?;
        root.SetClip(&clip)?;
        parent.Children()?.InsertAtTop(&root)?;
        let selection = RoundRect::new(c, row_radius(), 0.0)?;
        selection.set_fill(gfx::paint(gfx::theme().palette.selected))?;
        selection.show(false)?;
        root.Children()?.InsertAtTop(&selection.visual)?;
        // Above the highlight, under the rows' text.
        let key = RoundRect::new(c, KEY_RADIUS, 0.0)?;
        key.set_fill(gfx::color(text_secondary(), KEY_FILL_ALPHA))?;
        key.show(false)?;
        root.Children()?.InsertAtTop(&key.visual)?;
        let size = l.no_results.size();
        let mut rows = Vec::with_capacity(MAX_RESULTS);
        for _ in 0..MAX_RESULTS {
            // Parked until the first search needs it.
            let row = Canvas::new(c, gfx, PARKED)?;
            row.visual.SetIsVisible(false)?;
            root.Children()?.InsertAtTop(&row.visual)?;
            rows.push(row);
        }
        let message = Canvas::new(c, gfx, size)?;
        message.visual.SetIsVisible(false)?;
        root.Children()?.InsertAtTop(&message.visual)?;
        let glyph_size = (icon_size() * GLYPH_SCALE).round();
        Ok(Self {
            compositor: c.clone(),
            root,
            selection,
            key,
            rows,
            row_size: Cell::new(size),
            headers: RefCell::new(Vec::new()),
            header_text: RefCell::new(Vec::new()),
            message,
            shown: RefCell::new(vec![None; MAX_RESULTS]),
            rects: Default::default(),
            selected: Cell::new(None),
            title_format: gfx.text_format(TITLE_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            subtitle_format: gfx.text_format(SUBTITLE_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            hint_format: gfx.text_format(HINT_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            header_format: gfx.text_format(HEADER_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            message_format: gfx.text_format(MESSAGE_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            glyph_format: RefCell::new((glyph_size, gfx.icon_format(glyph_size)?)),
            icon_tint: Cell::new(false),
            accent: Cell::new(gfx::theme().palette.accent),
            message_text: RefCell::new(NO_RESULTS),
        })
    }

    /// The active theme or `icon_tint` changed: the highlight's color and
    /// shape. Every row is repainted by the next search (`forget`).
    pub fn set_theme(&self, gfx: &Gfx, icon_tint: bool) -> Result<()> {
        self.icon_tint.set(icon_tint);
        self.set_selection_fill()?;
        self.selection.set_radius(row_radius());
        let glyph_size = (icon_size() * GLYPH_SCALE).round();
        if self.glyph_format.borrow().0 != glyph_size {
            *self.glyph_format.borrow_mut() = (glyph_size, gfx.icon_format(glyph_size)?);
        }
        self.forget();
        self.set_selected(self.selected.get())
    }

    /// A new accent color: the list layout's selected row and the confirm
    /// text use it.
    pub fn set_accent(&self, accent: Rgb) -> Result<()> {
        if self.accent.replace(accent) != accent {
            self.forget();
            self.set_selection_fill()?;
        }
        Ok(())
    }

    fn set_selection_fill(&self) -> Result<()> {
        let (fill, key) = if is_list_layout() {
            let accent = self.accent.get();
            (gfx::color(accent, 1.0), gfx::on_fill(accent))
        } else {
            (gfx::paint(gfx::theme().palette.selected), text_secondary())
        };
        self.key.set_fill(gfx::color(key, KEY_FILL_ALPHA))?;
        self.selection.set_fill(fill)
    }

    /// The message row's text. Repaints it when it changes.
    pub fn set_message(&self, gfx: &Gfx, text: &'static str) -> Result<()> {
        if self.message_text.replace(text) != text {
            self.paint_message(gfx)?;
        }
        Ok(())
    }

    /// The container of everything (cross-faded with the grid).
    pub fn visual(&self) -> Result<Visual> {
        self.root.cast()
    }

    pub fn rect(&self, i: usize) -> Option<Rect> {
        self.rects.borrow().get(i).copied()
    }

    /// Which row is under the point (menu coordinates).
    pub fn hit(&self, p: Vector2) -> Option<usize> {
        self.rects.borrow().iter().position(|r| r.contains(p))
    }

    /// New DPI scale (or first use): resize every row and repaint the
    /// message. The rows get repainted by the next `paint_row`s.
    pub fn resize(&self, gfx: &Gfx, l: &Layout) -> Result<()> {
        let size = l.no_results.size();
        self.row_size.set(size);
        // Parked ones get their size when they are next used.
        for row in self.rows.iter().filter(|r| r.size() != PARKED) {
            row.resize(gfx, size)?;
        }
        let header = Self::header_size(size);
        for h in self.headers.borrow().iter().filter(|h| h.size() != PARKED) {
            h.resize(gfx, header)?;
        }
        self.header_text
            .borrow_mut()
            .iter_mut()
            .for_each(String::clear);
        self.forget();
        self.message.resize(gfx, size)?;
        self.paint_message(gfx)
    }

    fn paint_message(&self, gfx: &Gfx) -> Result<()> {
        let size = self.message.size();
        let format = &self.message_format;
        let text = *self.message_text.borrow();
        self.message.draw(gfx, |ctx| {
            let layout = gfx.text_layout(text, format, size, DWRITE_TEXT_ALIGNMENT_CENTER, true)?;
            let brush = gfx::brush(ctx, text_secondary(), 1.0)?;
            gfx::with_glow(ctx, |ctx| {
                // SAFETY: draws a live text layout with a live brush.
                unsafe { ctx.DrawTextLayout(Vector2::zero(), &layout, &brush, gfx::TEXT_OPTIONS) };
                Ok(())
            })
        })
    }

    /// Every row has to be painted again (DPI change, device lost).
    pub fn forget(&self) {
        self.shown.borrow_mut().iter_mut().for_each(|s| *s = None);
        self.header_text
            .borrow_mut()
            .iter_mut()
            .for_each(String::clear);
    }

    /// Places the rows of `l` (showing as many as it has) or the message,
    /// and the section headers, painted with `titles` (one per header).
    pub fn set_rows(&self, gfx: &Gfx, l: &Layout, no_results: bool, titles: &[&str]) -> Result<()> {
        for (i, row) in self.rows.iter().enumerate() {
            match l.result_rows.get(i) {
                Some(r) => {
                    self.unpark(gfx, i)?;
                    row.set_offset(gfx, r.x, r.y)?;
                    row.visual.SetIsVisible(true)?;
                }
                None => row.visual.SetIsVisible(false)?,
            }
        }
        self.set_headers(gfx, &l.result_headers, titles)?;
        let m = l.no_results;
        self.message.set_offset(gfx, m.x, m.y)?;
        self.message.visual.SetIsVisible(no_results)?;
        *self.rects.borrow_mut() = l.result_rows.clone();
        self.set_selected(self.selected.get())
    }

    /// Shows a header at each of `rects` with the matching title (missing
    /// header visuals are created now), and hides the rest.
    fn set_headers(&self, gfx: &Gfx, rects: &[Rect], titles: &[&str]) -> Result<()> {
        let mut headers = self.headers.borrow_mut();
        let mut texts = self.header_text.borrow_mut();
        while headers.len() < rects.len() {
            let canvas = Canvas::new(&self.compositor, gfx, rects[0].size())?;
            self.root.Children()?.InsertAtTop(&canvas.visual)?;
            headers.push(canvas);
            texts.push(String::new());
        }
        for (i, canvas) in headers.iter().enumerate() {
            let (Some(r), Some(title)) = (rects.get(i), titles.get(i)) else {
                canvas.visual.SetIsVisible(false)?;
                continue;
            };
            if canvas.size() != r.size() {
                canvas.resize(gfx, r.size())?;
                texts[i].clear();
            }
            canvas.set_offset(gfx, r.x, r.y)?;
            canvas.visual.SetIsVisible(true)?;
            let text = title.to_uppercase();
            if texts[i] != text {
                self.paint_header(gfx, canvas, &text)?;
                texts[i] = text;
            }
        }
        Ok(())
    }

    /// A section header is half a row tall.
    fn header_size(row: Vector2) -> Vector2 {
        Vector2 {
            X: row.X,
            Y: row.Y / 2.0,
        }
    }

    /// Gives row `i` its real size again if it was parked (it is then
    /// painted from scratch).
    fn unpark(&self, gfx: &Gfx, i: usize) -> Result<()> {
        let Some(row) = self.rows.get(i) else {
            return Ok(());
        };
        let size = self.row_size.get();
        if row.size() != size {
            row.resize(gfx, size)?;
            if let Some(slot) = self.shown.borrow_mut().get_mut(i) {
                *slot = None;
            }
        }
        Ok(())
    }

    /// The menu is hidden: rows from `keep` on, and every section header,
    /// shrink to almost nothing until a search needs them again (see
    /// `PARKED`). The list layout keeps its most-used rows (`keep`), which
    /// show as soon as the menu opens. A parked row that is still listed is
    /// sized and painted again by the next `paint_row` (every search paints
    /// all its rows that changed, and a parked row always counts as
    /// changed).
    pub fn park(&self, gfx: &Gfx, keep: usize) -> Result<()> {
        for (i, row) in self.rows.iter().enumerate().skip(keep) {
            if row.size() != PARKED {
                row.resize(gfx, PARKED)?;
                if let Some(slot) = self.shown.borrow_mut().get_mut(i) {
                    *slot = None;
                }
            }
        }
        let headers = self.headers.borrow();
        let mut texts = self.header_text.borrow_mut();
        for (h, text) in headers.iter().zip(texts.iter_mut()) {
            if h.size() != PARKED {
                h.resize(gfx, PARKED)?;
                h.visual.SetIsVisible(false)?;
                text.clear();
            }
        }
        Ok(())
    }

    /// Repaints the first `titles.len()` headers whose title changed (the
    /// rows kept their places, but a section is now another provider's).
    pub fn retitle(&self, gfx: &Gfx, titles: &[&str]) -> Result<()> {
        let headers = self.headers.borrow();
        let mut texts = self.header_text.borrow_mut();
        for ((canvas, painted), title) in headers.iter().zip(texts.iter_mut()).zip(titles) {
            if canvas.size() == PARKED {
                continue;
            }
            let text = title.to_uppercase();
            if *painted != text {
                self.paint_header(gfx, canvas, &text)?;
                *painted = text;
            }
        }
        Ok(())
    }

    fn paint_header(&self, gfx: &Gfx, canvas: &Canvas, text: &str) -> Result<()> {
        let size = canvas.size();
        let r = Rect::new(HEADER_LEFT, 2.0, size.X - 2.0 * HEADER_LEFT, size.Y - 2.0);
        canvas.draw(gfx, |ctx| {
            gfx::with_glow(ctx, |ctx| {
                draw_text(
                    ctx,
                    gfx,
                    text,
                    &self.header_format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    text_secondary(),
                )
            })
        })
    }

    /// Highlights row `i` (None = no highlight); the Enter key cap goes
    /// with it.
    pub fn set_selected(&self, i: Option<usize>) -> Result<()> {
        self.selected.set(i);
        match i.and_then(|i| self.rect(i)) {
            Some(r) => {
                self.selection.set_rect(r)?;
                self.key.set_rect(key_rect(r))?;
                self.selection.show(true)?;
                self.key.show(true)
            }
            None => {
                self.selection.show(false)?;
                self.key.show(false)
            }
        }
    }

    /// True if row `i` already shows `view` (and is selected or not, the
    /// way it is now), so it needn't be painted again.
    pub fn shows(&self, i: usize, view: &RowView) -> bool {
        let selected = self.selected.get() == Some(i);
        let print = view.fingerprint(selected);
        self.shown
            .borrow()
            .get(i)
            .is_some_and(|s| s.as_deref() == Some(print.as_str()))
    }

    /// Paints row `i`. The selected row shows the Enter hint; in the list
    /// layout it also has dark text on the accent fill.
    pub fn paint_row(&self, gfx: &Gfx, i: usize, view: &RowView) -> Result<()> {
        let Some(row) = self.rows.get(i) else {
            return Ok(());
        };
        self.unpark(gfx, i)?;
        let size = row.size();
        let selected = self.selected.get() == Some(i);
        let on_accent = selected && is_list_layout();
        let icon_size = icon_size();
        let text_left = ICON_LEFT + icon_size + ICON_GAP;
        let hint = if selected { HINT_W } else { 0.0 };
        let text_w = (size.X - text_left - TEXT_RIGHT - hint).max(0.0);
        let (primary, secondary) = if on_accent {
            let c = gfx::on_fill(self.accent.get());
            (c, c)
        } else {
            (text_primary(), text_secondary())
        };
        let (second_line, second_color) = match view.confirm {
            Some(question) => (
                question,
                if on_accent {
                    primary
                } else {
                    self.accent.get()
                },
            ),
            None => (view.subtitle, secondary),
        };
        let tint = self.icon_tint.get().then_some(primary);
        let glyph_format = self.glyph_format.borrow().1.clone();
        let paint = |ctx: &ID2D1DeviceContext| -> Result<()> {
            let icon_box = Rect::new(ICON_LEFT, (size.Y - icon_size) / 2.0, icon_size, icon_size);
            match view.icon {
                RowIcon::App(Some(icon)) => gfx::draw_icon(ctx, icon, icon_box, tint)?,
                RowIcon::App(None) => {}
                RowIcon::Glyph(c) => draw_text(
                    ctx,
                    gfx,
                    &c.to_string(),
                    &glyph_format,
                    icon_box,
                    DWRITE_TEXT_ALIGNMENT_CENTER,
                    primary,
                )?,
            }
            if second_line.is_empty() {
                let r = Rect::new(text_left, 0.0, text_w, size.Y);
                draw_text(
                    ctx,
                    gfx,
                    view.title,
                    &self.title_format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    primary,
                )?;
            } else {
                let top = ((size.Y - TITLE_H - LINE_GAP - SUBTITLE_H) / 2.0).round();
                let r = Rect::new(text_left, top, text_w, TITLE_H);
                draw_text(
                    ctx,
                    gfx,
                    view.title,
                    &self.title_format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    primary,
                )?;
                let r = Rect::new(text_left, top + TITLE_H + LINE_GAP, text_w, SUBTITLE_H);
                draw_text(
                    ctx,
                    gfx,
                    second_line,
                    &self.subtitle_format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                    second_color,
                )?;
            }
            if selected {
                self.paint_hint(ctx, gfx, size, view, secondary)?;
            }
            Ok(())
        };
        row.draw(gfx, |ctx| {
            // Dark text on the accent fill needs no glow.
            if on_accent {
                paint(ctx)
            } else {
                gfx::with_glow(ctx, paint)
            }
        })?;
        if let Some(slot) = self.shown.borrow_mut().get_mut(i) {
            *slot = Some(view.fingerprint(selected));
        }
        Ok(())
    }

    /// "<verb>  [Enter]" at the right end of the selected row.
    fn paint_hint(
        &self,
        ctx: &ID2D1DeviceContext,
        gfx: &Gfx,
        size: Vector2,
        view: &RowView,
        color: Rgb,
    ) -> Result<()> {
        // The key itself is the `key` shape behind the row (see there).
        let key = key_rect(Rect::new(0.0, 0.0, size.X, size.Y));
        draw_text(
            ctx,
            gfx,
            "Enter",
            &self.hint_format,
            key,
            DWRITE_TEXT_ALIGNMENT_CENTER,
            color,
        )?;
        let verb = if view.confirm.is_some() {
            "Confirm"
        } else {
            view.verb
        };
        let r = Rect::new(key.x - HINT_GAP - VERB_W, 0.0, VERB_W, size.Y);
        draw_text(
            ctx,
            gfx,
            verb,
            &self.hint_format,
            r,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
            color,
        )
    }
}

/// Where the "Enter" key cap goes in `row` (same coordinates as `row`).
fn key_rect(row: Rect) -> Rect {
    Rect::new(
        row.right() - TEXT_RIGHT - KEY_W,
        row.y + ((row.h - KEY_H) / 2.0).round(),
        KEY_W,
        KEY_H,
    )
}

/// The corner radius of the result highlight and hover, for the theme.
pub fn row_radius() -> f32 {
    gfx::theme().shape.button_radius
}
