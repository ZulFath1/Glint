//! The pinned apps grid (CLAUDE.md section 5).
//!
//! Each pinned app is one cell: an outer container placed by the layout,
//! holding an inner painted picture (icon + label). The open animation
//! moves only the inner one (rise, grow, fade in), so the layout and the
//! animation never fight over the same property.

use windows::UI::Composition::{Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::gfx::{self, Canvas, Gfx, TEXT_PRIMARY};
use super::layout::{CELL, Rect};
use crate::apps::App;
use crate::icons::IconCache;

/// Icon size and position inside the 80 × 80 cell (DIPs).
const ICON: f32 = 38.0;
const ICON_TOP: f32 = 11.0;
/// The label: 12 px, one line, "…" when too long.
const LABEL_SIZE: f32 = 12.0;
const LABEL_TOP: f32 = 54.0;
const LABEL_HEIGHT: f32 = 18.0;
const LABEL_SIDE: f32 = 4.0;

struct Cell {
    outer: ContainerVisual,
    canvas: Canvas,
    app: App,
}

pub struct Grid {
    cells: Vec<Cell>,
    rects: std::cell::RefCell<Vec<Rect>>,
    label_format: IDWriteTextFormat,
}

impl Grid {
    /// One cell per app, added to `parent`.
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        parent: &ContainerVisual,
        apps: Vec<App>,
    ) -> Result<Self> {
        let size = Vector2 { X: CELL, Y: CELL };
        let mut cells = Vec::with_capacity(apps.len());
        for app in apps {
            let outer = c.CreateContainerVisual()?;
            outer.SetSize(size)?;
            let canvas = Canvas::new(c, gfx, size)?;
            // Grow from the cell's center.
            canvas.visual.SetCenterPoint(Vector3 {
                X: CELL / 2.0,
                Y: CELL / 2.0,
                Z: 0.0,
            })?;
            outer.Children()?.InsertAtTop(&canvas.visual)?;
            parent.Children()?.InsertAtTop(&outer)?;
            cells.push(Cell { outer, canvas, app });
        }
        Ok(Self {
            cells,
            rects: Default::default(),
            label_format: gfx.text_format(LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
        })
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn name(&self, i: usize) -> Option<String> {
        self.cells.get(i).map(|c| c.app.name.clone())
    }

    pub fn rect(&self, i: usize) -> Option<Rect> {
        self.rects.borrow().get(i).copied()
    }

    /// Which cell is under the point (DIPs, menu coordinates).
    pub fn hit(&self, p: Vector2) -> Option<usize> {
        self.rects.borrow().iter().position(|r| r.contains(p))
    }

    /// The inner (animated) visual of every cell, in order.
    pub fn animated_visuals(&self) -> Result<Vec<Visual>> {
        self.cells.iter().map(|c| c.canvas.visual.cast()).collect()
    }

    /// Places the cells. `rects` comes from the layout, one per cell.
    pub fn set_rects(&self, gfx: &Gfx, rects: &[Rect]) -> Result<()> {
        let s = gfx.scale();
        for (cell, r) in self.cells.iter().zip(rects) {
            cell.outer.SetOffset(Vector3 {
                X: gfx::snap(r.x, s),
                Y: gfx::snap(r.y, s),
                Z: 0.0,
            })?;
        }
        *self.rects.borrow_mut() = rects.to_vec();
        Ok(())
    }

    /// Paints every cell at the current DPI scale.
    pub fn redraw(&self, gfx: &Gfx, icons: &IconCache) -> Result<()> {
        let size = Vector2 { X: CELL, Y: CELL };
        let icon_px = gfx.px(ICON);
        for cell in &self.cells {
            cell.canvas.resize(gfx, size)?;
            let icon = icons
                .get(gfx, &cell.app.aumid, &cell.app.item, icon_px)
                .inspect_err(|e| log_warn!("no icon for {}: {e}", cell.app.aumid))
                .ok();
            let label = gfx.text_layout(
                &cell.app.name,
                &self.label_format,
                Vector2 {
                    X: CELL - 2.0 * LABEL_SIDE,
                    Y: LABEL_HEIGHT,
                },
                DWRITE_TEXT_ALIGNMENT_CENTER,
                true,
            )?;
            cell.canvas.draw(gfx, |ctx| {
                let brush = gfx::brush(ctx, TEXT_PRIMARY, 1.0)?;
                gfx::with_glow(ctx, |ctx| {
                    let left = (CELL - ICON) / 2.0;
                    let dest = gfx::d2d_rect(Rect::new(left, ICON_TOP, ICON, ICON));
                    // SAFETY: drawing calls with live objects and local rects.
                    unsafe {
                        if let Some(icon) = &icon {
                            ctx.DrawBitmap(
                                icon,
                                Some(&dest),
                                1.0,
                                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                                None,
                                None,
                            );
                        }
                        ctx.DrawTextLayout(
                            Vector2 {
                                X: LABEL_SIDE,
                                Y: LABEL_TOP,
                            },
                            &label,
                            &brush,
                            gfx::TEXT_OPTIONS,
                        );
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    }
}
