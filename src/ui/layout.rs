//! Where everything goes inside the menu (CLAUDE.md section 5).
//!
//! Pure math, no Windows calls: give it the number of pins and what the
//! middle of the menu shows (the pinned grid, or a list of search results),
//! and it returns every rectangle in DIPs, measured from the menu's top-left
//! corner. The menu size follows the content, so fewer pins means a smaller
//! menu, and a search with 3 results is shorter than one with 8.
//!
//! The width never changes between the grid and the list (only the height
//! does), so the menu doesn't jump sideways while you type.

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

/// Search results: rows 40 tall, at most 8, 12 in from the menu's sides.
pub const ROW_HEIGHT: f32 = 40.0;
pub const MAX_RESULTS: usize = 8;
const LIST_SIDE: f32 = 12.0;

pub const FOOTER_HEIGHT: f32 = 60.0;
pub const AVATAR: f32 = 30.0;
pub const POWER_BUTTON: f32 = 44.0;
/// Inset of the footer's hover areas from the menu edge.
const FOOTER_INSET: f32 = 8.0;
/// Between the Settings gear and the power button.
const BUTTON_GAP: f32 = 4.0;

/// The power strip: a row of 4 buttons above the footer
/// (Lock, Sleep, Restart, Shut down; the owner never signs out, so that one
/// was dropped).
pub const STRIP_HEIGHT: f32 = 64.0;
const STRIP_SIDE: f32 = 12.0;
const STRIP_ABOVE_FOOTER: f32 = 8.0;
pub const STRIP_PAD: f32 = 4.0;
pub const STRIP_BUTTONS: usize = 4;

/// The music card: 12 from the menu's sides and 12 above the footer.
const CARD_MARGIN: f32 = 12.0;
/// Padding inside the card: 10 top/bottom, 12 left/right.
const CARD_PAD_Y: f32 = 10.0;
const CARD_PAD_X: f32 = 12.0;
pub const COVER: f32 = 44.0;
const CARD_ROW_GAP: f32 = 8.0;
/// Row 2: elapsed time, seek bar, total time.
const SEEK_ROW: f32 = 16.0;
const TIME_WIDTH: f32 = 40.0;
pub const TRACK_HEIGHT: f32 = 4.0;
pub const THUMB: f32 = 12.0;
/// Row 3 ("Playing from …" + the "Switch source" pill), only with 2+ sources.
const SOURCE_ROW: f32 = 26.0;
const PILL_WIDTH: f32 = 104.0;
/// Previous / Play-Pause / Next: hit targets of 36 × 44.
pub const MEDIA_BUTTON_W: f32 = 36.0;
const MEDIA_BUTTONS_RIGHT: f32 = 6.0;

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

/// What the middle of the menu shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// The pinned apps (the search box is empty).
    Grid,
    /// Search results: this many rows (0 = the "No apps found" row).
    List(usize),
}

/// Whether the music card shows, and how tall it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Music {
    Hidden,
    /// Cover, title, buttons and the seek bar.
    Card,
    /// The same plus "Playing from …" and "Switch source" (2+ sources).
    CardWithSources,
}

/// The parts of the music card, relative to the card's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CardParts {
    pub cover: Rect,
    /// Title on top, artist below; both end in "…" when too long.
    pub title: Rect,
    pub artist: Rect,
    /// Previous, Play/Pause, Next.
    pub buttons: [Rect; 3],
    pub elapsed: Rect,
    pub total: Rect,
    /// The 4 px track, full width (the thumb's center moves along it).
    pub track: Rect,
    /// Where you can grab the seek bar (taller than the track).
    pub seek_hit: Rect,
    /// "Playing from <app>" (empty rect without the source row).
    pub source: Rect,
    pub pill: Rect,
}

/// The card's parts for a card `w` DIPs wide.
pub fn card_parts(w: f32, sources: bool) -> CardParts {
    let top = CARD_PAD_Y;
    let cover = Rect::new(CARD_PAD_X, top, COVER, COVER);
    let first_button = w - MEDIA_BUTTONS_RIGHT - 3.0 * MEDIA_BUTTON_W;
    let buttons = std::array::from_fn(|i| {
        Rect::new(
            first_button + i as f32 * MEDIA_BUTTON_W,
            top,
            MEDIA_BUTTON_W,
            COVER,
        )
    });
    let text_x = cover.right() + 10.0;
    let text_w = (first_button - 4.0 - text_x).max(0.0);
    let title = Rect::new(text_x, top + 3.0, text_w, 20.0);
    let artist = Rect::new(text_x, top + 23.0, text_w, 18.0);

    let seek_y = cover.bottom() + CARD_ROW_GAP;
    let elapsed = Rect::new(CARD_PAD_X, seek_y, TIME_WIDTH, SEEK_ROW);
    let total = Rect::new(w - CARD_PAD_X - TIME_WIDTH, seek_y, TIME_WIDTH, SEEK_ROW);
    let track_x = elapsed.right() + 6.0;
    let track = Rect::new(
        track_x,
        seek_y + (SEEK_ROW - TRACK_HEIGHT) / 2.0,
        (total.x - 6.0 - track_x).max(0.0),
        TRACK_HEIGHT,
    );
    let seek_hit = Rect::new(
        track.x - THUMB / 2.0,
        seek_y - 4.0,
        track.w + THUMB,
        SEEK_ROW + 8.0,
    );

    let (source, pill) = if sources {
        let y = seek_y + SEEK_ROW + CARD_ROW_GAP;
        let pill = Rect::new(w - CARD_PAD_X - PILL_WIDTH, y, PILL_WIDTH, SOURCE_ROW);
        let source = Rect::new(
            CARD_PAD_X,
            y,
            (pill.x - 8.0 - CARD_PAD_X).max(0.0),
            SOURCE_ROW,
        );
        (source, pill)
    } else {
        (Rect::default(), Rect::default())
    };
    CardParts {
        cover,
        title,
        artist,
        buttons,
        elapsed,
        total,
        track,
        seek_hit,
        source,
        pill,
    }
}

/// How tall the music card is.
pub fn card_height(music: Music) -> f32 {
    let base = 2.0 * CARD_PAD_Y + COVER + CARD_ROW_GAP + SEEK_ROW;
    match music {
        Music::Hidden => 0.0,
        Music::Card => base,
        Music::CardWithSources => base + CARD_ROW_GAP + SOURCE_ROW,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub content: Content,
    /// The music card as it shows (always `Hidden` with search results).
    pub music: Music,
    /// Where the music card is (its full rect, even while hidden, so it can
    /// fade out in place).
    pub card: Rect,
    pub card_parts: CardParts,
    /// The whole menu.
    pub size: Vector2,
    pub search: Rect,
    pub columns: usize,
    pub rows: usize,
    /// One rectangle per pinned app, in order (row by row).
    pub cells: Vec<Rect>,
    /// One rectangle per search result row (empty for the grid).
    pub result_rows: Vec<Rect>,
    /// Where "No apps found" goes.
    pub no_results: Rect,
    pub footer: Rect,
    pub avatar: Rect,
    /// Where the display name may go (it is trimmed with "…" to fit).
    pub name: Rect,
    /// The largest hover area of the profile (shrunk to the name's width).
    pub profile: Rect,
    /// The gear that opens the Settings window.
    pub settings: Rect,
    pub power: Rect,
    pub strip: Rect,
}

/// The window has to fit the tallest menu, so it never has to move or grow
/// while the menu is open: the grid with the biggest music card, or 8
/// results, whichever is taller.
pub fn max_size(pins: usize, max_columns: usize) -> Vector2 {
    let grid = layout(pins, max_columns, Content::Grid, Music::CardWithSources).size;
    let list = layout(pins, max_columns, Content::List(MAX_RESULTS), Music::Hidden).size;
    Vector2 {
        X: grid.X.max(list.X),
        Y: grid.Y.max(list.Y),
    }
}

/// The menu layout for `pins` pinned apps, showing `content`, with the music
/// card as `music` says (the card only shows with the grid, never with search
/// results).
pub fn layout(pins: usize, max_columns: usize, content: Content, music: Music) -> Layout {
    let music = if content == Content::Grid {
        music
    } else {
        Music::Hidden
    };
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

    // The results list starts where the grid starts.
    let row = |i: usize| {
        Rect::new(
            LIST_SIDE,
            grid_top + i as f32 * ROW_HEIGHT,
            width - 2.0 * LIST_SIDE,
            ROW_HEIGHT,
        )
    };
    let (middle_height, result_rows) = match content {
        Content::Grid => (grid_height, Vec::new()),
        Content::List(n) => {
            let n = n.min(MAX_RESULTS);
            // No results still takes one row, for the message.
            (n.max(1) as f32 * ROW_HEIGHT, (0..n).map(row).collect())
        }
    };

    let below_middle = grid_top + middle_height + GRID_BELOW;
    // The card's place, also while hidden (a "Card"-sized one then).
    let card_kind = if music == Music::Hidden {
        Music::Card
    } else {
        music
    };
    let card = Rect::new(
        CARD_MARGIN,
        below_middle,
        width - 2.0 * CARD_MARGIN,
        card_height(card_kind),
    );
    let card_parts = card_parts(card.w, card_kind == Music::CardWithSources);
    let footer_top = if music == Music::Hidden {
        below_middle
    } else {
        card.bottom() + CARD_MARGIN
    };
    let footer = Rect::new(0.0, footer_top, width, FOOTER_HEIGHT);
    let mid = footer_top + FOOTER_HEIGHT / 2.0;
    let power = Rect::new(
        width - FOOTER_INSET - POWER_BUTTON,
        mid - POWER_BUTTON / 2.0,
        POWER_BUTTON,
        POWER_BUTTON,
    );
    // The Settings gear, just left of the power button, same size.
    let settings = Rect::new(
        power.x - BUTTON_GAP - POWER_BUTTON,
        power.y,
        POWER_BUTTON,
        POWER_BUTTON,
    );
    let avatar = Rect::new(16.0, mid - AVATAR / 2.0, AVATAR, AVATAR);
    let name_x = avatar.right() + 10.0;
    let name = Rect::new(name_x, mid - 10.0, settings.x - 12.0 - name_x, 20.0);
    let profile = Rect::new(
        FOOTER_INSET,
        footer_top + FOOTER_INSET,
        settings.x - 8.0 - FOOTER_INSET,
        FOOTER_HEIGHT - 2.0 * FOOTER_INSET,
    );
    let strip = Rect::new(
        STRIP_SIDE,
        footer_top - STRIP_ABOVE_FOOTER - STRIP_HEIGHT,
        width - 2.0 * STRIP_SIDE,
        STRIP_HEIGHT,
    );

    Layout {
        content,
        music,
        card,
        card_parts,
        size: Vector2 {
            X: width,
            Y: footer.bottom(),
        },
        search,
        columns,
        rows,
        cells,
        result_rows,
        no_results: row(0),
        footer,
        avatar,
        name,
        profile,
        settings,
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
        let l = layout(6, 4, Content::Grid, Music::Hidden);
        assert_eq!((l.columns, l.rows), (4, 2));
        assert_eq!(l.size.X, 360.0); // 4 × 84 + 24
        assert_eq!(l.size.Y, 64.0 + 164.0 + 12.0 + 60.0);
        assert_eq!(l.cells[0], Rect::new(14.0, 64.0, 80.0, 80.0));
        assert_eq!(l.cells[5], Rect::new(14.0 + 84.0, 64.0 + 84.0, 80.0, 80.0));
    }

    #[test]
    fn six_and_eight_columns() {
        // The new default: 6 pins in one row of 6, 6 × 84 + 24 = 528 wide.
        let l = layout(6, 6, Content::Grid, Music::Hidden);
        assert_eq!((l.columns, l.rows), (6, 1));
        assert_eq!(l.size.X, 528.0);
        let l = layout(16, 8, Content::Grid, Music::Hidden);
        assert_eq!((l.columns, l.rows), (8, 2));
        assert_eq!(l.size.X, 696.0);
        assert_eq!(l.cells[15].right(), 696.0 - 14.0);
    }

    #[test]
    fn the_gear_sits_left_of_the_power_button() {
        for pins in [0, 3, 6, 16] {
            let l = layout(pins, 6, Content::Grid, Music::Hidden);
            assert_eq!(l.settings.size(), l.power.size());
            assert_eq!(l.settings.y, l.power.y);
            assert_eq!(l.settings.right() + 4.0, l.power.x);
            assert!(l.name.right() < l.settings.x);
            assert!(l.profile.right() < l.settings.x);
            assert!(l.name.w > 100.0, "{pins} pins");
        }
    }

    #[test]
    fn few_pins_keep_the_minimum_width_and_center_the_grid() {
        let l = layout(1, 4, Content::Grid, Music::Hidden);
        assert_eq!((l.columns, l.rows), (1, 1));
        assert_eq!(l.size.X, 320.0);
        assert_eq!(l.cells[0].x, 120.0); // (320 - 80) / 2
        let l3 = layout(3, 4, Content::Grid, Music::Hidden);
        assert_eq!(l3.size.X, 320.0);
        // Fewer rows means a shorter menu.
        assert!(
            layout(4, 4, Content::Grid, Music::Hidden).size.Y
                < layout(5, 4, Content::Grid, Music::Hidden).size.Y
        );
        assert_eq!(layout(4, 4, Content::Grid, Music::Hidden).size.Y, l.size.Y);
    }

    #[test]
    fn many_pins_add_rows_not_columns() {
        let l = layout(9, 4, Content::Grid, Music::Hidden);
        assert_eq!((l.columns, l.rows), (4, 3));
        assert_eq!(l.size.X, 360.0);
        assert_eq!(layout(9, 3, Content::Grid, Music::Hidden).columns, 3);
    }

    #[test]
    fn no_pins_is_search_plus_footer() {
        let l = layout(0, 4, Content::Grid, Music::Hidden);
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
    fn search_results_keep_the_width_and_follow_the_row_count() {
        let grid = layout(6, 4, Content::Grid, Music::Hidden);
        let three = layout(6, 4, Content::List(3), Music::Hidden);
        let eight = layout(6, 4, Content::List(8), Music::Hidden);
        assert_eq!(three.size.X, grid.size.X);
        assert_eq!(three.result_rows.len(), 3);
        assert_eq!(three.result_rows[0], Rect::new(12.0, 64.0, 336.0, 40.0));
        assert_eq!(three.result_rows[2].y, 64.0 + 80.0);
        assert_eq!(three.size.Y, 64.0 + 120.0 + 12.0 + 60.0);
        // 8 rows (320) are taller than 2 rows of icons (164).
        assert_eq!(eight.size.Y, 64.0 + 320.0 + 12.0 + 60.0);
        // More than 8 is capped.
        assert_eq!(
            layout(6, 4, Content::List(20), Music::Hidden).result_rows,
            eight.result_rows
        );
        // No results: one row for the message.
        let none = layout(6, 4, Content::List(0), Music::Hidden);
        assert!(none.result_rows.is_empty());
        assert_eq!(none.size.Y, 64.0 + 40.0 + 12.0 + 60.0);
        assert_eq!(none.no_results, three.result_rows[0]);
        // The pinned grid stays where it was (it's only faded out).
        assert_eq!(three.cells, grid.cells);
    }

    #[test]
    fn the_window_fits_the_tallest_menu() {
        // 2 rows of icons + the tallest card (434) < 8 results (456).
        assert_eq!(
            max_size(6, 4),
            layout(6, 4, Content::List(8), Music::Hidden).size
        );
        // 5 rows of icons + the card are taller than 8 results.
        assert_eq!(
            max_size(20, 4),
            layout(20, 4, Content::Grid, Music::CardWithSources).size
        );
        for pins in 0..13 {
            let max = max_size(pins, 4);
            for music in [Music::Hidden, Music::Card, Music::CardWithSources] {
                assert!(layout(pins, 4, Content::Grid, music).size.Y <= max.Y);
            }
        }
    }

    #[test]
    fn the_music_card_sits_between_the_grid_and_the_footer() {
        let plain = layout(6, 4, Content::Grid, Music::Hidden);
        let card = layout(6, 4, Content::Grid, Music::Card);
        let full = layout(6, 4, Content::Grid, Music::CardWithSources);
        // 12 below the grid, 12 from the sides, 12 above the footer.
        assert_eq!(card.card, Rect::new(12.0, 64.0 + 164.0 + 12.0, 336.0, 88.0));
        assert_eq!(card.footer.y, card.card.bottom() + 12.0);
        assert_eq!(card.size.Y, plain.size.Y + 88.0 + 12.0);
        assert_eq!(full.card.h, 88.0 + 8.0 + 26.0);
        // The grid doesn't move when the card appears.
        assert_eq!(card.cells, plain.cells);
        // Search results hide the card.
        let list = layout(6, 4, Content::List(3), Music::Card);
        assert_eq!(list.music, Music::Hidden);
        assert_eq!(
            list.size,
            layout(6, 4, Content::List(3), Music::Hidden).size
        );
    }

    #[test]
    fn card_parts_fit_inside_the_card() {
        for w in [296.0, 336.0] {
            for sources in [false, true] {
                let p = card_parts(w, sources);
                let h = card_height(if sources {
                    Music::CardWithSources
                } else {
                    Music::Card
                });
                let all = [p.cover, p.title, p.artist, p.elapsed, p.total, p.track];
                for r in all.iter().chain(p.buttons.iter()) {
                    assert!(r.x >= 0.0 && r.right() <= w && r.y >= 0.0 && r.bottom() <= h);
                }
                // Text never runs under the buttons; hit targets ≥ 36 × 44.
                assert!(p.title.right() <= p.buttons[0].x);
                assert!(p.buttons.iter().all(|b| b.w >= 36.0 && b.h >= 44.0));
                assert!(p.track.w > 100.0);
                assert_eq!(p.pill.w > 0.0, sources);
                if sources {
                    assert!(p.pill.right() <= w && p.pill.bottom() <= h);
                    assert!(p.source.right() < p.pill.x);
                }
            }
        }
    }

    #[test]
    fn footer_and_strip_fit_inside_the_menu() {
        let cases = [
            (Content::Grid, Music::Hidden),
            (Content::Grid, Music::CardWithSources),
            (Content::List(0), Music::Hidden),
            (Content::List(8), Music::Card),
        ];
        for (content, music) in cases {
            for pins in 0..13 {
                let l = layout(pins, 4, content, music);
                assert_eq!(l.footer.bottom(), l.size.Y);
                assert!(l.power.right() <= l.size.X && l.power.bottom() <= l.size.Y);
                assert!(l.strip.y >= 0.0, "{pins} pins");
                assert!(l.strip.bottom() < l.footer.y);
                let buttons = strip_buttons(l.strip);
                assert!(buttons[STRIP_BUTTONS - 1].right() <= l.strip.w + 0.01);
                assert!(confirm_buttons(l.strip)[0].x > 100.0);
                for r in &l.result_rows {
                    assert!(r.bottom() < l.footer.y);
                }
            }
        }
    }
}
