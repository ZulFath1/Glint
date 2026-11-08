//! The pinned apps grid (CLAUDE.md section 5).
//!
//! Each pinned app is one cell: an outer container placed by the layout,
//! holding an inner painted picture (icon + label). The open animation
//! moves only the inner one (rise, grow, fade in), so the layout and the
//! animation never fight over the same property.

use std::cell::{Cell as StdCell, RefCell};

use windows::UI::Composition::{CompositionObject, Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::gfx::{self, Canvas, Gfx, text_primary};
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
    compositor: Compositor,
    /// Holds every cell; faded out while search results show.
    root: ContainerVisual,
    /// Changes when pins are added or removed (config reload, right-click).
    cells: RefCell<Vec<Cell>>,
    rects: RefCell<Vec<Rect>>,
    label_format: IDWriteTextFormat,
    /// App names under the icons (`show_names` in config.toml).
    show_names: StdCell<bool>,
}

impl Grid {
    /// One cell per app, added to `parent`.
    pub fn new(
        c: &Compositor,
        gfx: &Gfx,
        parent: &ContainerVisual,
        apps: Vec<App>,
        show_names: bool,
    ) -> Result<Self> {
        let root = c.CreateContainerVisual()?;
        parent.Children()?.InsertAtTop(&root)?;
        let grid = Self {
            compositor: c.clone(),
            root,
            cells: RefCell::new(Vec::with_capacity(apps.len())),
            rects: Default::default(),
            label_format: gfx.text_format(LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?,
            show_names: StdCell::new(show_names),
        };
        let cells = apps
            .into_iter()
            .map(|app| grid.new_cell(gfx, app))
            .collect::<Result<Vec<_>>>()?;
        *grid.cells.borrow_mut() = cells;
        Ok(grid)
    }

    /// A cell for `app`, added to the grid's visual tree (not yet painted
    /// or placed).
    fn new_cell(&self, gfx: &Gfx, app: App) -> Result<Cell> {
        let size = Vector2 { X: CELL, Y: CELL };
        let outer = self.compositor.CreateContainerVisual()?;
        outer.SetSize(size)?;
        let canvas = Canvas::new(&self.compositor, gfx, size)?;
        // Grow from the cell's center.
        canvas.visual.SetCenterPoint(Vector3 {
            X: CELL / 2.0,
            Y: CELL / 2.0,
            Z: 0.0,
        })?;
        outer.Children()?.InsertAtTop(&canvas.visual)?;
        self.root.Children()?.InsertAtTop(&outer)?;
        Ok(Cell { outer, canvas, app })
    }

    /// Replaces the pinned apps with `apps`, in that order. Cells of apps
    /// that stay pinned are kept (the same visuals, so they can glide to
    /// their new place); the others are removed. Returns the indexes of the
    /// new cells: they still need to be placed and painted.
    pub fn set_apps(&self, gfx: &Gfx, apps: Vec<App>) -> Result<Vec<usize>> {
        let mut old = std::mem::take(&mut *self.cells.borrow_mut());
        let mut cells = Vec::with_capacity(apps.len());
        let mut added = Vec::new();
        for app in apps {
            let same = |c: &Cell| c.app.aumid.eq_ignore_ascii_case(&app.aumid);
            match old.iter().position(same) {
                Some(i) => cells.push(old.remove(i)),
                None => {
                    added.push(cells.len());
                    cells.push(self.new_cell(gfx, app)?);
                }
            }
        }
        let children = self.root.Children()?;
        for gone in old {
            children.Remove(&gone.outer)?;
        }
        *self.cells.borrow_mut() = cells;
        Ok(added)
    }

    pub fn len(&self) -> usize {
        self.cells.borrow().len()
    }

    /// The AUMID of pin `i` (what `shell:AppsFolder\…` launches).
    pub fn aumid(&self, i: usize) -> Option<String> {
        self.cells.borrow().get(i).map(|c| c.app.aumid.clone())
    }

    /// The pinned apps, as plain data (the search list before the full app
    /// list has loaded).
    pub fn apps(&self) -> Vec<crate::apps::AppInfo> {
        self.cells
            .borrow()
            .iter()
            .filter_map(|c| c.app.info())
            .collect()
    }

    /// Each cell's outer visual: these glide when the grid is laid out
    /// again (see `LayoutMotion::set_group`).
    pub fn motion_objects(&self) -> Result<Vec<CompositionObject>> {
        self.cells.borrow().iter().map(|c| c.outer.cast()).collect()
    }

    /// The container of all cells (faded out while searching).
    pub fn visual(&self) -> Result<Visual> {
        self.root.cast()
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
        self.cells
            .borrow()
            .iter()
            .map(|c| c.canvas.visual.cast())
            .collect()
    }

    /// Places the cells. `rects` comes from the layout, one per cell.
    pub fn set_rects(&self, gfx: &Gfx, rects: &[Rect]) -> Result<()> {
        let all: Vec<usize> = (0..rects.len()).collect();
        self.place(gfx, rects, &all)?;
        *self.rects.borrow_mut() = rects.to_vec();
        Ok(())
    }

    /// Moves only the cells `which` to their place in `rects` (so a new
    /// cell starts where it belongs instead of gliding in from a corner).
    pub fn place(&self, gfx: &Gfx, rects: &[Rect], which: &[usize]) -> Result<()> {
        let s = gfx.scale();
        let cells = self.cells.borrow();
        for &i in which {
            let (Some(cell), Some(r)) = (cells.get(i), rects.get(i)) else {
                continue;
            };
            cell.outer.SetOffset(Vector3 {
                X: gfx::snap(r.x, s),
                Y: gfx::snap(r.y, s),
                Z: 0.0,
            })?;
        }
        Ok(())
    }

    /// Names under the icons on or off. Repaint afterwards (`redraw`).
    pub fn set_show_names(&self, on: bool) -> bool {
        self.show_names.replace(on) != on
    }

    /// Paints every cell at the current DPI scale.
    pub fn redraw(&self, gfx: &Gfx, icons: &IconCache) -> Result<()> {
        let all: Vec<usize> = (0..self.len()).collect();
        self.redraw_cells(gfx, icons, &all)
    }

    /// Paints the cells `which` at the current DPI scale.
    pub fn redraw_cells(&self, gfx: &Gfx, icons: &IconCache, which: &[usize]) -> Result<()> {
        let size = Vector2 { X: CELL, Y: CELL };
        let icon_px = gfx.px(ICON);
        let names = self.show_names.get();
        // Without a name, the icon sits in the middle of the cell.
        let icon_top = if names { ICON_TOP } else { (CELL - ICON) / 2.0 };
        let cells = self.cells.borrow();
        for cell in which.iter().filter_map(|&i| cells.get(i)) {
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
                let brush = gfx::brush(ctx, text_primary(), 1.0)?;
                gfx::with_glow(ctx, |ctx| {
                    let left = (CELL - ICON) / 2.0;
                    let dest = gfx::d2d_rect(Rect::new(left, icon_top, ICON, ICON));
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
                        if names {
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
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    }
}
