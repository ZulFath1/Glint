//! The search results list (CLAUDE.md section 9): up to 8 rows of
//! "24 px icon + name", 40 px tall, replacing the pinned grid while the
//! search box has text. Or a single "No apps found" row.
//!
//! The 8 rows (and the message) are created once and reused for every
//! search; typing only repaints the rows whose app changed. Rows never
//! wait for an icon: a row whose icon isn't cached yet is painted without
//! one, and the icon is filled in a moment later (see `Ui::load_icons`).

use std::cell::{Cell, RefCell};

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextFormat,
};
use windows::core::{Interface, Result, h};
use windows_numerics::Vector2;

use super::gfx::{self, Canvas, Gfx, text_primary, text_secondary};
use super::layout::{Layout, MAX_RESULTS, Rect};
use super::shape::RoundRect;
use crate::config::Rgb;

/// Shown in the message row of the list layout when nothing is typed and
/// there is nothing to suggest yet.
pub const TYPE_TO_SEARCH: &str = "Type to search";

/// Icon size and place inside a row (DIPs).
const ICON: f32 = 24.0;
const ICON_LEFT: f32 = 14.0;
const ICON_GAP: f32 = 12.0;
const TEXT_RIGHT: f32 = 12.0;
/// The list layout: "Enter" on the right of the selected row.
const ENTER_HINT: &str = "Enter";
const HINT_W: f32 = 56.0;

/// The rows' icon size for the active theme (bigger in the list layout).
pub fn icon_size() -> f32 {
    if is_list_layout() {
        super::layout::LIST_ICON
    } else {
        ICON
    }
}

/// The list layout highlights the selected row with the accent color and
/// shows the "Enter" hint on it.
fn is_list_layout() -> bool {
    gfx::theme().layout == crate::theme::LayoutKind::List
}
const NAME_SIZE: f32 = 14.0;
const MESSAGE_SIZE: f32 = 13.0;

pub const NO_RESULTS: &str = "No apps found";
/// The list container is this big (DIPs), so its clip only ever cuts at
/// the bottom.
const CLIP_BOX: f32 = 10_000.0;

/// What a row shows right now, so unchanged rows aren't repainted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shown {
    id: String,
    with_icon: bool,
    /// Painted as the selected row (list layout: dark text and the hint).
    selected: bool,
}

pub struct Results {
    root: ContainerVisual,
    selection: RoundRect,
    rows: Vec<Canvas>,
    message: Canvas,
    shown: RefCell<Vec<Option<Shown>>>,
    /// Where the visible rows are (menu coordinates).
    rects: RefCell<Vec<Rect>>,
    selected: Cell<Option<usize>>,
    name_format: IDWriteTextFormat,
    message_format: IDWriteTextFormat,
    /// Icons in one color (`icon_tint`).
    icon_tint: Cell<bool>,
    /// The selected row's fill in the list layout.
    accent: Cell<Rgb>,
    /// What the message row says ("No apps found", "Type to search").
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
        let size = l.no_results.size();
        let mut rows = Vec::with_capacity(MAX_RESULTS);
        for _ in 0..MAX_RESULTS {
            let row = Canvas::new(c, gfx, size)?;
            row.visual.SetIsVisible(false)?;
            root.Children()?.InsertAtTop(&row.visual)?;
            rows.push(row);
        }
        let message = Canvas::new(c, gfx, size)?;
        message.visual.SetIsVisible(false)?;
        root.Children()?.InsertAtTop(&message.visual)?;
        Ok(Self {
            root,
            selection,
            rows,
            message,
            shown: RefCell::new(vec![None; MAX_RESULTS]),
            rects: Default::default(),
            selected: Cell::new(None),
            name_format: gfx.text_format(NAME_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            message_format: gfx.text_format(MESSAGE_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            icon_tint: Cell::new(false),
            accent: Cell::new(gfx::theme().palette.accent),
            message_text: RefCell::new(NO_RESULTS),
        })
    }

    /// The active theme or `icon_tint` changed: the highlight's color and
    /// shape. Every row is repainted by the next search (`forget`).
    pub fn set_theme(&self, icon_tint: bool) -> Result<()> {
        self.icon_tint.set(icon_tint);
        self.set_selection_fill()?;
        self.selection.set_radius(row_radius());
        self.forget();
        self.set_selected(self.selected.get())
    }

    /// A new accent color: the list layout's selected row uses it.
    pub fn set_accent(&self, accent: Rgb) -> Result<()> {
        if self.accent.replace(accent) != accent && is_list_layout() {
            self.forget();
            self.set_selection_fill()?;
        }
        Ok(())
    }

    fn set_selection_fill(&self) -> Result<()> {
        let fill = if is_list_layout() {
            gfx::color(self.accent.get(), 1.0)
        } else {
            gfx::paint(gfx::theme().palette.selected)
        };
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
        for row in &self.rows {
            row.resize(gfx, size)?;
        }
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
    }

    /// Places the rows of `l` (showing as many as it has) or the message.
    pub fn set_rows(&self, gfx: &Gfx, l: &Layout, no_results: bool) -> Result<()> {
        for (i, row) in self.rows.iter().enumerate() {
            match l.result_rows.get(i) {
                Some(r) => {
                    row.set_offset(gfx, r.x, r.y)?;
                    row.visual.SetIsVisible(true)?;
                }
                None => row.visual.SetIsVisible(false)?,
            }
        }
        let m = l.no_results;
        self.message.set_offset(gfx, m.x, m.y)?;
        self.message.visual.SetIsVisible(no_results)?;
        *self.rects.borrow_mut() = l.result_rows.clone();
        self.set_selected(self.selected.get())
    }

    /// Highlights row `i` (None = no highlight).
    pub fn set_selected(&self, i: Option<usize>) -> Result<()> {
        self.selected.set(i);
        match i.and_then(|i| self.rect(i)) {
            Some(r) => {
                self.selection.set_rect(r)?;
                self.selection.show(true)
            }
            None => self.selection.show(false),
        }
    }

    /// Is row `i` painted as the selected one? Only the list layout paints
    /// the selected row differently (everywhere else, always false).
    pub fn paints_selected(&self, i: usize) -> bool {
        is_list_layout() && self.selected.get() == Some(i)
    }

    /// True if row `i` already shows app `id` (with an icon, if
    /// `with_icon`, and selected or not as `paints_selected` says).
    pub fn shows(&self, i: usize, id: &str, with_icon: bool) -> bool {
        let selected = self.paints_selected(i);
        self.shown.borrow().get(i).is_some_and(|s| {
            s.as_ref()
                .is_some_and(|s| s.id == id && s.with_icon == with_icon && s.selected == selected)
        })
    }

    /// Paints row `i`: the icon (if there is one yet) and the name; in the
    /// list layout the selected row gets dark text and the "Enter" hint.
    pub fn paint_row(
        &self,
        gfx: &Gfx,
        i: usize,
        id: &str,
        name: &str,
        icon: Option<&ID2D1Bitmap1>,
    ) -> Result<()> {
        let Some(row) = self.rows.get(i) else {
            return Ok(());
        };
        let size = row.size();
        let selected = self.paints_selected(i);
        let icon_size = icon_size();
        let text_left = ICON_LEFT + icon_size + ICON_GAP;
        let hint = if selected { HINT_W } else { 0.0 };
        let label = gfx.text_layout(
            name,
            &self.name_format,
            Vector2 {
                X: (size.X - text_left - TEXT_RIGHT - hint).max(0.0),
                Y: size.Y,
            },
            DWRITE_TEXT_ALIGNMENT_LEADING,
            true,
        )?;
        let text = if selected {
            gfx::on_fill(self.accent.get())
        } else {
            text_primary()
        };
        let tint = self.icon_tint.get().then_some(text);
        let paint = |ctx: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext| {
            let brush = gfx::brush(ctx, text, 1.0)?;
            let top = (size.Y - icon_size) / 2.0;
            if let Some(icon) = icon {
                let dest = Rect::new(ICON_LEFT, top, icon_size, icon_size);
                gfx::draw_icon(ctx, icon, dest, tint)?;
            }
            // SAFETY: draws a live text layout with a live brush.
            unsafe {
                ctx.DrawTextLayout(
                    Vector2 {
                        X: text_left,
                        Y: 0.0,
                    },
                    &label,
                    &brush,
                    gfx::TEXT_OPTIONS,
                );
            }
            if selected {
                let r = Rect::new(size.X - TEXT_RIGHT - HINT_W, 0.0, HINT_W, size.Y);
                super::footer::draw_text(
                    ctx,
                    gfx,
                    ENTER_HINT,
                    &self.message_format,
                    r,
                    DWRITE_TEXT_ALIGNMENT_TRAILING,
                    text,
                )?;
            }
            Ok(())
        };
        row.draw(gfx, |ctx| {
            // Dark text on the accent fill needs no glow.
            if selected {
                paint(ctx)
            } else {
                gfx::with_glow(ctx, paint)
            }
        })?;
        if let Some(slot) = self.shown.borrow_mut().get_mut(i) {
            *slot = Some(Shown {
                id: id.to_string(),
                with_icon: icon.is_some(),
                selected,
            });
        }
        Ok(())
    }
}

/// The corner radius of the result highlight and hover, for the theme.
pub fn row_radius() -> f32 {
    gfx::theme().shape.button_radius
}
