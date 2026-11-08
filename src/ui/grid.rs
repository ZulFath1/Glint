//! The pinned apps grid (CLAUDE.md section 5).
//!
//! Each pinned app is one cell: an outer container placed by the layout,
//! holding an inner painted picture (icon + label). The open animation
//! moves only the inner one (rise, grow, fade in), so the layout and the
//! animation never fight over the same property.

use std::cell::{Cell as StdCell, RefCell};

use windows::UI::Composition::{CompositionObject, Compositor, ContainerVisual, Visual};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use super::gfx::{self, Canvas, Gfx, text_primary};
use super::layout::{CELL, Rect, STRIP_CELL, STRIP_ICON};
use crate::apps::App;
use crate::icons::IconCache;
use crate::theme::IconTile;

/// Icon size and position inside the 80 × 80 cell (DIPs).
const ICON: f32 = 38.0;
const ICON_TOP: f32 = 11.0;
/// The label: 12 px, one line, "…" when too long.
const LABEL_SIZE: f32 = 12.0;
const LABEL_TOP: f32 = 54.0;
const LABEL_HEIGHT: f32 = 18.0;
const LABEL_SIDE: f32 = 4.0;
/// The tile behind the icon (themes with `IconTile`): the icon plus this
/// much on every side.
const TILE_PAD: f32 = 5.0;

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
    /// Icons in one color (`icon_tint` in config.toml).
    icon_tint: StdCell<bool>,
    /// Small icon-only cells (the list layout's pinned strip).
    small: StdCell<bool>,
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
            icon_tint: StdCell::new(false),
            small: StdCell::new(false),
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

    /// Small icon-only cells (the list layout) or the normal ones. Repaint
    /// afterwards (`redraw`).
    pub fn set_small(&self, on: bool) -> bool {
        self.small.replace(on) != on
    }

    /// The cell size, the icon size and whether there is room for a name.
    fn metrics(&self) -> (f32, f32, bool) {
        if self.small.get() {
            (STRIP_CELL, STRIP_ICON, false)
        } else {
            (CELL, ICON, self.show_names.get())
        }
    }

    /// One-color icons on or off. Repaint afterwards (`redraw`).
    pub fn set_icon_tint(&self, on: bool) -> bool {
        self.icon_tint.replace(on) != on
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
        let (cell_size, icon_size, names) = self.metrics();
        let size = Vector2 {
            X: cell_size,
            Y: cell_size,
        };
        let icon_px = gfx.px(icon_size);
        // Without a name, the icon sits in the middle of the cell.
        let icon_top = if names {
            ICON_TOP
        } else {
            (cell_size - icon_size) / 2.0
        };
        let theme = gfx::theme();
        let tint = self.icon_tint.get().then(text_primary);
        let cells = self.cells.borrow();
        for (i, cell) in which.iter().filter_map(|&i| cells.get(i).map(|c| (i, c))) {
            cell.outer.SetSize(size)?;
            cell.canvas.resize(gfx, size)?;
            // Grow from the cell's center.
            cell.canvas.visual.SetCenterPoint(Vector3 {
                X: size.X / 2.0,
                Y: size.Y / 2.0,
                Z: 0.0,
            })?;
            let icon = icons
                .get(gfx, &cell.app.aumid, &cell.app.item, icon_px)
                .inspect_err(|e| log_warn!("no icon for {}: {e}", cell.app.aumid))
                .ok();
            let name = if theme.terminal {
                cell.app.name.to_lowercase()
            } else {
                cell.app.name.clone()
            };
            let label = gfx.text_layout(
                &name,
                &self.label_format,
                Vector2 {
                    X: cell_size - 2.0 * LABEL_SIDE,
                    Y: LABEL_HEIGHT,
                },
                DWRITE_TEXT_ALIGNMENT_CENTER,
                true,
            )?;
            let left = (cell_size - icon_size) / 2.0;
            let dest = Rect::new(left, icon_top, icon_size, icon_size);
            cell.canvas.draw(gfx, |ctx| {
                paint_tile(ctx, dest, i)?;
                let brush = gfx::brush(ctx, text_primary(), 1.0)?;
                gfx::with_glow(ctx, |ctx| {
                    if let Some(icon) = &icon {
                        gfx::draw_icon(ctx, icon, dest, tint)?;
                    }
                    // SAFETY: draws a live text layout with a live brush.
                    unsafe {
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

/// The tile behind icon number `i` (at `icon`), as the theme wants it.
pub fn paint_tile(
    ctx: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
    icon: Rect,
    i: usize,
) -> Result<()> {
    let theme = gfx::theme();
    if theme.tile == IconTile::None {
        return Ok(());
    }
    let p = theme.palette;
    let r = Rect::new(
        icon.x - TILE_PAD,
        icon.y - TILE_PAD,
        icon.w + 2.0 * TILE_PAD,
        icon.h + 2.0 * TILE_PAD,
    );
    let stroke = match (theme.tile, p.tile_borders) {
        (IconTile::Bordered, borders) if !borders.is_empty() => {
            Some((borders[i % borders.len()], 0.9, 1.0))
        }
        _ => None,
    };
    gfx::round_rect(
        ctx,
        r,
        theme.shape.tile_radius,
        Some((p.tile.color, p.tile.alpha)),
        stroke,
    )
}
