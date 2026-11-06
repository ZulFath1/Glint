//! Where everything goes inside the menu (CLAUDE.md section 5).
//!
//! Pure math, no Windows calls: give it the number of pins and it returns
//! every rectangle in DIPs, measured from the menu's top-left corner. The
//! menu size follows the content, so fewer pins means a smaller menu.

use windows_numerics::Vector2;

/// Search bar: 16 from the top and the sides, 40 tall, 8 below.
pub const SEARCH_PAD: f32 = 16.0;
pub const SEARCH_HEIGHT: f32 = 40.0;
const SEARCH_BELOW: f32 = 8.0;

/// Grid cells are 80 × 80 with a gap of 4 (so one step is 84).
pub const CELL: f32 = 80.0;
const CELL_GAP: f32 = 4.0;
const CELL_STEP: f32 = CELL + CELL_GAP;
/// Menu width = max(320, columns × 84 + 24).
const MIN_WIDTH: f32 = 320.0;
const WIDTH_EXTRA: f32 = 24.0;
/// Space between the grid and the footer.
const GRID_BELOW: f32 = 12.0;

pub const FOOTER_HEIGHT: f32 = 60.0;
pub const AVATAR: f32 = 30.0;
pub const POWER_BUTTON: f32 = 44.0;
/// Inset of the footer's hover areas from the menu edge.
const FOOTER_INSET: f32 = 8.0;

/// The power strip: a row of 4 buttons above the footer
/// (Lock, Sleep, Restart, Shut down; the owner never signs out, so that one
/// was dropped).
pub const STRIP_HEIGHT: f32 = 64.0;
const STRIP_SIDE: f32 = 12.0;
const STRIP_ABOVE_FOOTER: f32 = 8.0;
pub const STRIP_PAD: f32 = 4.0;
pub const STRIP_BUTTONS: usize = 4;

/// A rectangle in DIPs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, p: Vector2) -> bool {
        p.X >= self.x && p.X < self.x + self.w && p.Y >= self.y && p.Y < self.y + self.h
    }

    pub fn size(&self) -> Vector2 {
        Vector2 {
            X: self.w,
            Y: self.h,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The whole menu.
    pub size: Vector2,
    pub search: Rect,
    pub columns: usize,
    pub rows: usize,
    /// One rectangle per pinned app, in order (row by row).
    pub cells: Vec<Rect>,
    pub footer: Rect,
    pub avatar: Rect,
    /// Where the display name may go (it is trimmed with "…" to fit).
    pub name: Rect,
    /// The largest hover area of the profile (shrunk to the name's width).
    pub profile: Rect,
    pub power: Rect,
    pub strip: Rect,
}

/// The menu layout for `pins` pinned apps.
pub fn layout(pins: usize, max_columns: usize) -> Layout {
    let columns = pins.min(max_columns.max(1));
    let rows = if columns == 0 {
        0
    } else {
        pins.div_ceil(columns)
    };
    let width = MIN_WIDTH.max(columns as f32 * CELL_STEP + WIDTH_EXTRA);

    let search = Rect::new(
        SEARCH_PAD,
        SEARCH_PAD,
        width - 2.0 * SEARCH_PAD,
        SEARCH_HEIGHT,
    );

    // The grid is centered; its height is the rows minus the last gap.
    let grid_top = search.bottom() + SEARCH_BELOW;
    let grid_width = columns as f32 * CELL_STEP - CELL_GAP;
    let grid_left = ((width - grid_width) / 2.0).round();
    let cells = (0..pins)
        .map(|i| {
            let (row, col) = (i / columns, i % columns);
            Rect::new(
                grid_left + col as f32 * CELL_STEP,
                grid_top + row as f32 * CELL_STEP,
                CELL,
                CELL,
            )
        })
        .collect();
    let grid_height = if rows == 0 {
        0.0
    } else {
        rows as f32 * CELL_STEP - CELL_GAP
    };

    let footer_top = grid_top + grid_height + GRID_BELOW;
    let footer = Rect::new(0.0, footer_top, width, FOOTER_HEIGHT);
    let mid = footer_top + FOOTER_HEIGHT / 2.0;
    let power = Rect::new(
        width - FOOTER_INSET - POWER_BUTTON,
        mid - POWER_BUTTON / 2.0,
        POWER_BUTTON,
        POWER_BUTTON,
    );
    let avatar = Rect::new(16.0, mid - AVATAR / 2.0, AVATAR, AVATAR);
    let name_x = avatar.right() + 10.0;
    let name = Rect::new(name_x, mid - 10.0, power.x - 12.0 - name_x, 20.0);
    let profile = Rect::new(
        FOOTER_INSET,
        footer_top + FOOTER_INSET,
        power.x - 8.0 - FOOTER_INSET,
        FOOTER_HEIGHT - 2.0 * FOOTER_INSET,
    );
    let strip = Rect::new(
        STRIP_SIDE,
        footer_top - STRIP_ABOVE_FOOTER - STRIP_HEIGHT,
        width - 2.0 * STRIP_SIDE,
        STRIP_HEIGHT,
    );

    Layout {
        size: Vector2 {
            X: width,
            Y: footer.bottom(),
        },
        search,
        columns,
        rows,
        cells,
        footer,
        avatar,
        name,
        profile,
        power,
        strip,
    }
}

/// The 5 power buttons, relative to the strip's top-left corner.
pub fn strip_buttons(strip: Rect) -> [Rect; STRIP_BUTTONS] {
    let w = (strip.w - 2.0 * STRIP_PAD) / STRIP_BUTTONS as f32;
    std::array::from_fn(|i| {
        Rect::new(
            STRIP_PAD + i as f32 * w,
            STRIP_PAD,
            w,
            strip.h - 2.0 * STRIP_PAD,
        )
    })
}

/// The confirm view ("Restart now?  [Cancel] [Restart]"): the two buttons,
/// relative to the strip's top-left corner. Index 0 = Cancel, 1 = confirm.
pub fn confirm_buttons(strip: Rect) -> [Rect; 2] {
    let h = 36.0;
    let y = (strip.h - h) / 2.0;
    let confirm = Rect::new(strip.w - 12.0 - 92.0, y, 92.0, h);
    let cancel = Rect::new(confirm.x - 8.0 - 80.0, y, 80.0, h);
    [cancel, confirm]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_pins_make_a_4_by_2_grid() {
        let l = layout(6, 4);
        assert_eq!((l.columns, l.rows), (4, 2));
        assert_eq!(l.size.X, 360.0); // 4 × 84 + 24
        assert_eq!(l.size.Y, 64.0 + 164.0 + 12.0 + 60.0);
        assert_eq!(l.cells[0], Rect::new(14.0, 64.0, 80.0, 80.0));
        assert_eq!(l.cells[5], Rect::new(14.0 + 84.0, 64.0 + 84.0, 80.0, 80.0));
    }

    #[test]
    fn few_pins_keep_the_minimum_width_and_center_the_grid() {
        let l = layout(1, 4);
        assert_eq!((l.columns, l.rows), (1, 1));
        assert_eq!(l.size.X, 320.0);
        assert_eq!(l.cells[0].x, 120.0); // (320 - 80) / 2
        let l3 = layout(3, 4);
        assert_eq!(l3.size.X, 320.0);
        // Fewer rows means a shorter menu.
        assert!(layout(4, 4).size.Y < layout(5, 4).size.Y);
        assert_eq!(layout(4, 4).size.Y, l.size.Y);
    }

    #[test]
    fn many_pins_add_rows_not_columns() {
        let l = layout(9, 4);
        assert_eq!((l.columns, l.rows), (4, 3));
        assert_eq!(l.size.X, 360.0);
        assert_eq!(layout(9, 3).columns, 3);
    }

    #[test]
    fn no_pins_is_search_plus_footer() {
        let l = layout(0, 4);
        assert_eq!((l.columns, l.rows), (0, 0));
        assert!(l.cells.is_empty());
        assert_eq!(
            l.size,
            Vector2 {
                X: 320.0,
                Y: 64.0 + 12.0 + 60.0
            }
        );
    }

    #[test]
    fn footer_and_strip_fit_inside_the_menu() {
        for pins in 0..13 {
            let l = layout(pins, 4);
            assert_eq!(l.footer.bottom(), l.size.Y);
            assert!(l.power.right() <= l.size.X && l.power.bottom() <= l.size.Y);
            assert!(l.strip.y >= 0.0, "{pins} pins");
            assert!(l.strip.bottom() < l.footer.y);
            let buttons = strip_buttons(l.strip);
            assert!(buttons[STRIP_BUTTONS - 1].right() <= l.strip.w + 0.01);
            assert!(confirm_buttons(l.strip)[0].x > 100.0);
        }
    }
}
