//! The menu's visuals and how they react to keys and the mouse.
//!
//! Composition setup: a Compositor + DesktopWindowTarget hosted in our HWND.
//! Everything inside the root visual is laid out in DIPs; the root visual is
//! scaled by the monitor's DPI factor.
//!
//! Inside the menu body, bottom to top:
//! ```text
//! back    footer fill + divider
//! hover   the mouse-hover highlight (one, moved around)
//! front   search box, pinned grid / search results, avatar / name / power
//! popup   the power strip
//! top     the keyboard focus ring (one, moved around)
//! ```
//!
//! The middle of the menu shows either the pinned grid (search box empty)
//! or the search results. Switching cross-fades the two and glides the
//! menu to its new height (220 ms, see `anim::LayoutMotion`).
//!
//! Input methods never call anything that could run a message loop (like
//! ShellExecute). Instead they return an [`Action`] for main.rs to carry
//! out, so the window procedure can't re-enter us halfway through.

pub mod anim;
pub mod background;
pub mod footer;
pub mod gfx;
pub mod grid;
pub mod layout;
pub mod results;
pub mod search_box;
pub mod shape;
pub mod shell;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionScopedBatch, Compositor, ContainerVisual, Visual};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetQueueStatus, KillTimer, QS_INPUT, SetTimer};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use crate::apps::AppInfo;
use crate::config::{Config, Rgb};
use crate::icons::IconCache;
use crate::power::PowerAction;
use crate::search::{self, Query};
use crate::window::{Edge, Placement};
use anim::{Animator, Kind, LayoutMotion};
use background::{Applied, SystemEffects};
use footer::{Footer, PowerStrip, StripState};
use gfx::Gfx;
use grid::Grid;
use layout::{Content, Layout, MAX_RESULTS, Rect};
use results::Results;
use search_box::SearchBox;
use shape::RoundRect;

/// The timer that fills in result icons a moment after typing (WM_TIMER id).
pub const TIMER_ICONS: usize = 2;
/// At most this long per WM_TIMER spent loading icons, so the thread is
/// never busy for longer than a frame.
const ICON_BUDGET: Duration = Duration::from_millis(8);

/// Something main.rs should do after an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    OpenAccountSettings,
    Power(PowerAction),
    /// Launch the app with this AppsFolder ID (AUMID).
    Launch(String),
}

/// Who has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The search box (while there are results, Up/Down move the result
    /// highlight and Enter launches it).
    Search,
    Cell(usize),
    Profile,
    Power,
    /// A button of the power strip.
    Strip(usize),
}

/// What is under the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Nothing,
    Search,
    Cell(usize),
    /// A search result row.
    Row(usize),
    Profile,
    Power,
    Strip(usize),
    /// The power strip, but not on a button.
    StripPanel,
}

const HOVER_ALPHA: f32 = 0.08;
const CELL_RADIUS: f32 = 12.0;
const BUTTON_RADIUS: f32 = 10.0;
const RING_WIDTH: f32 = 2.0;

/// Every installed app, prepared for searching, plus how often each was
/// launched (in memory only for now; saved to the config in milestone 6).
#[derive(Default)]
struct Catalog {
    apps: Vec<AppInfo>,
    /// `items[i]` is `apps[i]`, prepared for the matcher.
    items: Vec<search::Item>,
    /// Pinned AUMIDs from the config (pinned apps win ties).
    pinned: Vec<String>,
    /// Lowercased ID → launches.
    launches: HashMap<String, u32>,
}

impl Catalog {
    fn set_apps(&mut self, apps: Vec<AppInfo>) {
        self.items = apps
            .iter()
            .map(|a| search::Item::new(&a.name, self.is_pinned(&a.id), self.launches(&a.id)))
            .collect();
        self.apps = apps;
    }

    fn is_pinned(&self, id: &str) -> bool {
        self.pinned.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    fn launches(&self, id: &str) -> u32 {
        self.launches.get(&id.to_lowercase()).copied().unwrap_or(0)
    }

    fn record_launch(&mut self, id: &str) {
        let count = self.launches.entry(id.to_lowercase()).or_insert(0);
        *count += 1;
        let count = *count;
        for (app, item) in self.apps.iter().zip(self.items.iter_mut()) {
            if app.id.eq_ignore_ascii_case(id) {
                item.launches = count;
            }
        }
    }
}

/// How fast the search was while the menu was open (logged on close).
#[derive(Default, Clone, Copy)]
struct SearchStats {
    queries: u32,
    slowest: Duration,
}

pub struct Ui {
    hwnd: HWND,
    _compositor: Compositor,
    // Keeps our visual tree attached to the window.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    shell: shell::Shell,
    menu: Visual,
    animator: Animator,
    motion: LayoutMotion,
    gfx: Gfx,
    icons: IconCache,
    max_columns: usize,
    layout: RefCell<Layout>,
    search: SearchBox,
    grid: Grid,
    results: Results,
    footer: Footer,
    strip: PowerStrip,
    hover: RoundRect,
    ring: RoundRect,
    focus: Cell<Focus>,
    /// The focus ring shows only after keyboard navigation (like CSS
    /// `:focus-visible`); the mouse hides it again.
    keyboard_nav: Cell<bool>,
    hovered: Cell<Target>,
    pressed: Cell<Target>,
    /// Selecting text in the search box with the mouse.
    dragging: Cell<bool>,
    placement: Cell<Option<Placement>>,
    is_open: Cell<bool>,
    reduced_motion: Cell<bool>,
    catalog: RefCell<Catalog>,
    /// The search the results are for.
    query: RefCell<String>,
    /// The results: indexes into `catalog.apps`, best first.
    hits: RefCell<Vec<usize>>,
    /// The highlighted result.
    selected: Cell<usize>,
    /// Apps whose icon could not be loaded (not retried).
    failed_icons: RefCell<HashSet<String>>,
    stats: Cell<SearchStats>,
}

fn white(alpha: f32) -> windows::UI::Color {
    gfx::color(Rgb::new(255, 255, 255), alpha)
}

/// True if a key press or mouse click is waiting in our message queue.
fn input_waiting() -> bool {
    // SAFETY: GetQueueStatus has no preconditions. The high word lists the
    // kinds of messages currently in the queue.
    let status = unsafe { GetQueueStatus(QS_INPUT) };
    (status >> 16) & QS_INPUT.0 != 0
}
impl Ui {
    pub fn new(hwnd: HWND, config: &Config, scale: f32) -> Result<Self> {
        let compositor = Compositor::new()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // SAFETY: `hwnd` is our live top-level window, created on this thread.
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, true)? };

        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        // Pinned apps that aren't installed are skipped; the layout follows
        // the number that is left.
        let apps: Vec<crate::apps::App> = config
            .pinned
            .iter()
            .filter_map(|aumid| crate::apps::find(aumid))
            .collect();
        let layout = layout::layout(apps.len(), config.max_columns, Content::Grid);

        let shell = shell::Shell::new(&compositor, layout.size)?;
        root.Children()?.InsertAtTop(shell.host())?;
        let menu = shell.host_visual()?;
        Animator::reset_closed(&menu)?;
        let animator = Animator::new(&compositor, &config.animation)?;
        let motion = LayoutMotion::new(&compositor)?;
        let gfx = Gfx::new(&compositor, hwnd, scale)?;

        let new_layer = || -> Result<ContainerVisual> {
            let v = compositor.CreateContainerVisual()?;
            shell.body().Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        let back = new_layer()?;
        let hover_layer = new_layer()?;
        let front = new_layer()?;
        let popup = new_layer()?;
        let top = new_layer()?;

        let footer = Footer::new(&compositor, &gfx, &back, &front, &layout)?;
        let hover = RoundRect::new(&compositor, CELL_RADIUS, 0.0)?;
        hover.set_fill(white(HOVER_ALPHA))?;
        hover.show(false)?;
        hover_layer.Children()?.InsertAtTop(&hover.visual)?;
        let search = SearchBox::new(&compositor, &gfx, &front, layout.search, config.accent)?;
        let grid = Grid::new(&compositor, &gfx, &front, apps)?;
        let results = Results::new(&compositor, &gfx, &front, &layout, &footer.fill_visual()?)?;
        let strip = PowerStrip::new(&compositor, &gfx, &popup, layout.strip)?;
        let ring = RoundRect::new(&compositor, CELL_RADIUS, RING_WIDTH)?;
        ring.set_stroke(gfx::color(config.accent, 1.0))?;
        ring.show(false)?;
        top.Children()?.InsertAtTop(&ring.visual)?;

        // Everything that moves or resizes when the menu height changes.
        for (object, kind) in shell
            .motion_objects()?
            .into_iter()
            .chain(footer.motion_objects()?)
        {
            motion.add(object, kind);
        }
        motion.add(strip.motion_object()?, Kind::Visual);

        // Until the full app list arrives (a moment after startup), search
        // at least finds the pinned apps.
        let mut catalog = Catalog {
            pinned: config.pinned.clone(),
            ..Default::default()
        };
        catalog.set_apps(grid.apps());

        let ui = Self {
            hwnd,
            _compositor: compositor,
            _target: target,
            root,
            shell,
            menu,
            animator,
            motion,
            gfx,
            icons: IconCache::default(),
            max_columns: config.max_columns,
            layout: RefCell::new(layout),
            search,
            grid,
            results,
            footer,
            strip,
            hover,
            ring,
            focus: Cell::new(Focus::Search),
            keyboard_nav: Cell::new(false),
            hovered: Cell::new(Target::Nothing),
            pressed: Cell::new(Target::Nothing),
            dragging: Cell::new(false),
            placement: Cell::new(None),
            is_open: Cell::new(false),
            reduced_motion: Cell::new(false),
            catalog: RefCell::new(catalog),
            query: RefCell::new(String::new()),
            hits: RefCell::new(Vec::new()),
            selected: Cell::new(0),
            failed_icons: RefCell::new(HashSet::new()),
            stats: Cell::new(SearchStats::default()),
        };
        ui.refresh()?;
        Ok(ui)
    }

    /// The largest the menu gets, in DIPs (the window is made this big, so
    /// it never has to move while the menu grows or shrinks).
    pub fn max_size(&self) -> Vector2 {
        layout::max_size(self.grid.len(), self.max_columns)
    }

    /// The point the power strip grows out of (the power button's top).
    fn strip_origin(l: &Layout) -> Vector2 {
        Vector2 {
            X: l.power.x + l.power.w / 2.0,
            Y: l.power.y,
        }
    }

    /// Positions and paints everything at the current DPI scale. Instant:
    /// nothing glides.
    fn refresh(&self) -> Result<()> {
        let l = self.layout.borrow().clone();
        self.motion.instant(|| {
            self.search.set_rect(&self.gfx, l.search)?;
            self.grid.set_rects(&self.gfx, &l.cells)?;
            self.grid.redraw(&self.gfx, &self.icons)?;
            self.footer.refresh(&self.gfx, &l)?;
            self.strip
                .refresh(&self.gfx, l.strip, Self::strip_origin(&l))?;
            self.results.resize(&self.gfx, &l)?;
            self.place_content()
        })?;
        self.paint_results()?;
        self.update_focus_visuals()
    }

    /// Moves everything that depends on the menu's height (no repainting).
    /// Inside `motion.animate` these glide; inside `motion.instant` they jump.
    fn place_content(&self) -> Result<()> {
        let l = self.layout.borrow().clone();
        self.shell.set_size(l.size)?;
        self.footer.move_to(&self.gfx, &l)?;
        self.strip
            .move_to(&self.gfx, l.strip, Self::strip_origin(&l))?;
        // Going back to the grid, the rows stay as they are while they fade.
        if let Content::List(n) = l.content {
            self.results.set_rows(&self.gfx, &l, n == 0)?;
        }
        self.place_menu()
    }

    /// The menu's position inside the window, and its scale origin on the
    /// edge nearest the taskbar. A menu shorter than the window keeps its
    /// taskbar-side edge where it is.
    fn place_menu(&self) -> Result<()> {
        let Some(p) = self.placement.get() else {
            return Ok(());
        };
        let s = self.layout.borrow().size;
        self.menu.SetOffset(Vector3 {
            X: p.menu_x as f32 / p.scale,
            Y: p.menu_top(s.Y) as f32 / p.scale,
            Z: 0.0,
        })?;
        let (x, y) = match p.edge {
            Edge::Bottom => (s.X / 2.0, s.Y),
            Edge::Top => (s.X / 2.0, 0.0),
            // Vertical taskbars: grow from the corner next to Start.
            Edge::Left => (0.0, 0.0),
            Edge::Right => (s.X, 0.0),
        };
        self.menu.SetCenterPoint(Vector3 { X: x, Y: y, Z: 0.0 })
    }

    /// The GPU device was replaced: every surface is empty, every cached
    /// bitmap useless. Paint it all again.
    pub fn device_replaced(&self) -> Result<()> {
        self.icons.clear();
        self.refresh()
    }

    /// Applies the DPI scale and the menu's position inside the window.
    pub fn set_layout(&self, p: &Placement) -> Result<()> {
        self.placement.set(Some(*p));
        if (p.scale - self.gfx.scale()).abs() > 0.001 {
            log_info!("DPI scale {} -> {}: repainting", self.gfx.scale(), p.scale);
            self.gfx.set_scale(p.scale);
            self.refresh()?;
        }
        self.root.SetScale(Vector3 {
            X: p.scale,
            Y: p.scale,
            Z: 1.0,
        })?;
        self.motion.instant(|| self.place_menu())
    }

    pub fn apply_background(&self, config: &Config, effects: SystemEffects) -> Result<Applied> {
        self.shell.background.apply(&config.background, effects)
    }

    /// Call before showing a fully hidden window: the menu at scale 0, the
    /// search box empty and the pinned grid back (all instant).
    pub fn reset_closed(&self) -> Result<()> {
        Animator::reset_closed(&self.menu)?;
        self.is_open.set(false);
        self.search.editor.borrow_mut().clear();
        self.search.redraw_text(&self.gfx)?;
        self.update_search(true)
    }

    /// Is this point (window pixels) on the menu, rather than on the
    /// transparent space around it?
    pub fn menu_contains(&self, x: i32, y: i32) -> bool {
        let h = self.layout.borrow().size.Y;
        self.placement
            .get()
            .is_some_and(|p| p.menu_contains(x, y, h))
    }

    /// Gets the content ready and starts the open animation. `fresh` = the
    /// window was hidden (not a reopen that interrupts a close).
    pub fn animate_open(&self, fresh: bool, reduced_motion: bool) -> Result<()> {
        self.reduced_motion.set(reduced_motion);
        self.is_open.set(true);
        if fresh {
            // A fresh start: keyboard in the search box, nothing hovered.
            self.strip.reset()?;
            self.keyboard_nav.set(false);
            self.pressed.set(Target::Nothing);
            self.dragging.set(false);
            self.hovered.set(Target::Nothing);
            self.hover.show(false)?;
            self.focus.set(Focus::Search);
            let icons = self.grid.animated_visuals()?;
            if reduced_motion {
                Animator::stagger_done(&icons)?;
            } else {
                self.animator.stagger_in(&icons)?;
            }
        }
        self.update_focus_visuals()?;
        self.animator.open(&self.menu, reduced_motion)
    }

    pub fn animate_close(&self, reduced_motion: bool) -> Result<CompositionScopedBatch> {
        self.is_open.set(false);
        // No blinking caret while hidden: zero work for DWM.
        Animator::stop_blink(&self.search.caret_visual()?, true)?;
        search_box::ime_cancel(self.hwnd);
        if self.dragging.replace(false) {
            // SAFETY: releases the mouse capture we took; no pointers.
            let _ = unsafe { ReleaseCapture() };
        }
        let stats = self.stats.take();
        if stats.queries > 0 {
            log_info!(
                "search: {} queries, slowest {} us",
                stats.queries,
                stats.slowest.as_micros()
            );
        }
        self.animator.close(&self.menu, reduced_motion)
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    /// A new list of installed apps (startup, or after an install or
    /// uninstall). If a search is showing, it is run again.
    pub fn set_apps(&self, apps: Vec<AppInfo>) -> Result<()> {
        if apps.is_empty() {
            log_warn!("the app list came back empty; keeping the old one");
            return Ok(());
        }
        self.catalog.borrow_mut().set_apps(apps);
        self.results.forget();
        self.update_search(true)
    }

    /// Counts a launch (more launches = higher in ties).
    pub fn record_launch(&self, id: &str) {
        self.catalog.borrow_mut().record_launch(id);
    }

    fn is_list(&self) -> bool {
        matches!(self.layout.borrow().content, Content::List(_))
    }

    /// Runs the search for the text in the box, unless it is the same as
    /// last time (`force` runs it anyway, e.g. for a new app list).
    fn update_search(&self, force: bool) -> Result<()> {
        let text = self.search.editor.borrow().text().to_string();
        if !force && *self.query.borrow() == text {
            return Ok(());
        }
        let q = Query::new(&text);
        *self.query.borrow_mut() = text;
        if q.is_empty() {
            self.hits.borrow_mut().clear();
            return self.set_content(Content::Grid);
        }
        let started = Instant::now();
        let hits = search::rank(&self.catalog.borrow().items, &q, MAX_RESULTS);
        let took = started.elapsed();
        let mut stats = self.stats.get();
        stats.queries += 1;
        stats.slowest = stats.slowest.max(took);
        self.stats.set(stats);

        let n = hits.len();
        *self.hits.borrow_mut() = hits;
        self.selected.set(0);
        self.set_content(Content::List(n))?;
        self.paint_results()
    }

    /// Switches the middle of the menu to `content`. While open, the menu
    /// glides to its new height and the grid and list cross-fade.
    fn set_content(&self, content: Content) -> Result<()> {
        let old = self.layout.borrow().content;
        if old == content {
            return Ok(());
        }
        *self.layout.borrow_mut() = layout::layout(self.grid.len(), self.max_columns, content);
        let animate = self.is_open.get() && !self.reduced_motion.get();
        if animate {
            self.motion.animate(|| self.place_content())?;
        } else {
            self.motion.instant(|| self.place_content())?;
        }
        let list = matches!(content, Content::List(_));
        if matches!(old, Content::List(_)) != list {
            self.motion.fade(&self.grid.visual()?, !list, animate)?;
            self.motion.fade(&self.results.visual()?, list, animate)?;
            if list && matches!(self.focus.get(), Focus::Cell(_)) {
                self.focus.set(Focus::Search);
            }
        }
        // Things moved under the mouse: the next mouse move finds the new
        // target.
        self.hovered.set(Target::Nothing);
        self.hover.show(false)?;
        self.update_focus_visuals()
    }

    /// Paints the result rows that changed. Rows never wait for an icon:
    /// missing ones are loaded a moment later (`load_icons`).
    fn paint_results(&self) -> Result<()> {
        let hits = self.hits.borrow();
        let px = self.gfx.px(results::ICON);
        let mut missing = false;
        {
            let catalog = self.catalog.borrow();
            let failed = self.failed_icons.borrow();
            for (row, &i) in hits.iter().enumerate() {
                let Some(app) = catalog.apps.get(i) else {
                    continue;
                };
                let icon = self.icons.peek(&app.id, px);
                if icon.is_none() && !failed.contains(&app.id) {
                    missing = true;
                }
                if !self.results.shows(row, &app.id, icon.is_some()) {
                    self.results
                        .paint_row(&self.gfx, row, &app.id, &app.name, icon.as_ref())?;
                }
            }
        }
        let selected = (!hits.is_empty()).then(|| self.selected.get());
        self.results.set_selected(selected)?;
        if missing {
            // SAFETY: a timer on our own window. WM_TIMER only comes when no
            // other messages (typing!) are waiting.
            unsafe { SetTimer(Some(self.hwnd), TIMER_ICONS, 1, None) };
        }
        Ok(())
    }

    /// WM_TIMER for [`TIMER_ICONS`]: loads the icons the results are
    /// missing, one by one, stopping as soon as a key press or click is
    /// waiting (it comes first). When none are left, the timer is stopped.
    pub fn load_icons(&self) {
        let more = self.load_some_icons().unwrap_or_else(|e| {
            log_error!("loading result icons failed: {e}");
            false
        });
        if !more {
            // SAFETY: stops the timer started in `paint_results`.
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_ICONS) };
        }
    }

    fn load_some_icons(&self) -> Result<bool> {
        if !self.is_open.get() || !self.is_list() {
            return Ok(false);
        }
        let deadline = Instant::now() + ICON_BUDGET;
        let px = self.gfx.px(results::ICON);
        loop {
            if input_waiting() {
                return Ok(true);
            }
            // The first row still without its icon.
            let next = {
                let catalog = self.catalog.borrow();
                let failed = self.failed_icons.borrow();
                self.hits
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter_map(|(row, &i)| catalog.apps.get(i).map(|a| (row, a)))
                    .find(|(_, a)| !failed.contains(&a.id) && self.icons.peek(&a.id, px).is_none())
                    .map(|(row, a)| (row, a.clone()))
            };
            let Some((row, app)) = next else {
                return Ok(false);
            };
            let icon = app
                .shell_item()
                .and_then(|item| self.icons.get(&self.gfx, &app.id, &item, px));
            match icon {
                Ok(icon) => {
                    self.results
                        .paint_row(&self.gfx, row, &app.id, &app.name, Some(&icon))?
                }
                Err(e) => {
                    log_warn!("no icon for {}: {e}", app.id);
                    self.failed_icons.borrow_mut().insert(app.id);
                }
            }
            if Instant::now() >= deadline {
                return Ok(true);
            }
        }
    }

    /// The ID of result `row`, to launch it.
    fn result_id(&self, row: usize) -> Option<String> {
        let i = *self.hits.borrow().get(row)?;
        self.catalog.borrow().apps.get(i).map(|a| a.id.clone())
    }

    fn launch_result(&self, row: usize) -> Action {
        self.result_id(row).map_or(Action::None, Action::Launch)
    }

    fn launch_pin(&self, i: usize) -> Action {
        self.grid.aumid(i).map_or(Action::None, Action::Launch)
    }

    /// Moves the result highlight (wrapping around).
    fn move_selection(&self, delta: isize) -> Result<()> {
        let n = self.hits.borrow().len();
        if n == 0 {
            return Ok(());
        }
        self.selected
            .set(search::step(self.selected.get(), n, delta));
        self.results.set_selected(Some(self.selected.get()))
    }

    // -----------------------------------------------------------------------
    // Focus, hover, ring
    // -----------------------------------------------------------------------

    fn set_focus(&self, focus: Focus) -> Result<()> {
        self.focus.set(focus);
        self.update_focus_visuals()
    }

    fn focus_rect(&self, focus: Focus) -> Option<(Rect, f32)> {
        match focus {
            Focus::Search => None, // the search box shows focus with its border
            Focus::Cell(i) => self.grid.rect(i).map(|r| (r, CELL_RADIUS)),
            Focus::Profile => Some((self.footer.profile_rect(), BUTTON_RADIUS)),
            Focus::Power => Some((self.footer.power_rect(), BUTTON_RADIUS)),
            Focus::Strip(i) => self.strip.buttons().get(i).map(|&r| (r, BUTTON_RADIUS)),
        }
    }

    fn update_focus_visuals(&self) -> Result<()> {
        let focus = self.focus.get();
        let in_search = focus == Focus::Search;
        self.search.set_focused(in_search)?;
        let caret = self.search.caret_visual()?;
        if in_search && self.is_open.get() {
            self.animator.blink(&caret)?;
        } else {
            Animator::stop_blink(&caret, in_search)?;
        }
        match self.focus_rect(focus) {
            Some((r, radius)) if self.keyboard_nav.get() => {
                self.ring.set_radius(radius);
                self.ring.set_rect(r)?;
                self.ring.show(true)
            }
            _ => self.ring.show(false),
        }
    }

    fn set_hovered(&self, target: Target) -> Result<()> {
        if self.hovered.replace(target) == target {
            return Ok(());
        }
        let strip_button = match target {
            Target::Strip(i) => Some(i),
            _ => None,
        };
        self.strip.set_hover(strip_button)?;
        let highlight = match target {
            Target::Cell(i) => self.grid.rect(i).map(|r| (r, CELL_RADIUS)),
            Target::Row(i) => self.results.rect(i).map(|r| (r, results::ROW_RADIUS)),
            Target::Profile => Some((self.footer.profile_rect(), BUTTON_RADIUS)),
            Target::Power => Some((self.footer.power_rect(), BUTTON_RADIUS)),
            _ => None,
        };
        match highlight {
            Some((r, radius)) => {
                self.hover.set_radius(radius);
                self.hover.set_rect(r)?;
                self.hover.show(true)
            }
            None => self.hover.show(false),
        }
    }

    fn hit(&self, p: Vector2) -> Target {
        if self.strip.is_open() {
            if let Some(i) = self.strip.hit(p) {
                return Target::Strip(i);
            }
            if self.strip.rect().contains(p) {
                return Target::StripPanel;
            }
        }
        if self.search.rect().contains(p) {
            Target::Search
        } else if let Some(i) = self.results.hit(p).filter(|_| self.is_list()) {
            Target::Row(i)
        } else if let Some(i) = self.grid.hit(p).filter(|_| !self.is_list()) {
            Target::Cell(i)
        } else if self.footer.profile_rect().contains(p) {
            Target::Profile
        } else if self.footer.power_rect().contains(p) {
            Target::Power
        } else {
            Target::Nothing
        }
    }

    /// Window (client) pixels → menu DIPs.
    fn to_menu(&self, x: i32, y: i32) -> Option<Vector2> {
        let p = self.placement.get()?;
        Some(Vector2 {
            X: (x - p.menu_x) as f32 / p.scale,
            Y: (y - p.menu_top(self.layout.borrow().size.Y)) as f32 / p.scale,
        })
    }

    // -----------------------------------------------------------------------
    // Power strip
    // -----------------------------------------------------------------------

    fn open_strip(&self) -> Result<()> {
        self.strip
            .open(&self.gfx, &self.animator, self.reduced_motion.get())?;
        self.set_focus(Focus::Strip(0))
    }

    fn close_strip(&self, focus_after: Focus) -> Result<()> {
        self.strip
            .close(&self.animator, self.reduced_motion.get())?;
        if matches!(self.hovered.get(), Target::Strip(_) | Target::StripPanel) {
            self.set_hovered(Target::Nothing)?;
        }
        self.set_focus(focus_after)
    }

    /// A strip button was clicked or Enter was pressed on it.
    fn press_strip(&self, i: usize) -> Result<Action> {
        match self.strip.state() {
            StripState::Closed => Ok(Action::None),
            StripState::Buttons => {
                let Some(&action) = PowerAction::ALL.get(i) else {
                    return Ok(Action::None);
                };
                if !action.needs_confirm() {
                    return Ok(Action::Power(action));
                }
                // "Are you sure?": focus starts on Cancel.
                self.strip
                    .set_state(&self.gfx, StripState::Confirm(action))?;
                self.hovered.set(Target::Nothing);
                self.set_focus(Focus::Strip(0))?;
                Ok(Action::None)
            }
            StripState::Confirm(action) => {
                if i == 1 {
                    return Ok(Action::Power(action));
                }
                // Cancel: back to the buttons, focus on the one we came from.
                self.strip.set_state(&self.gfx, StripState::Buttons)?;
                self.hovered.set(Target::Nothing);
                let back = PowerAction::ALL.iter().position(|&a| a == action);
                self.set_focus(Focus::Strip(back.unwrap_or(0)))?;
                Ok(Action::None)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    /// WM_KEYDOWN. `ctrl` / `shift` = those keys are held.
    pub fn key_down(&self, vk: u16, ctrl: bool, shift: bool) -> Result<Action> {
        let vk = VIRTUAL_KEY(vk);
        if vk == VK_ESCAPE {
            // Esc closes the innermost thing: strip, then text, then menu.
            if self.strip.is_open() {
                // Back to where the strip was opened from: the power button
                // when using the keyboard, else the search box.
                let back = if self.keyboard_nav.get() {
                    Focus::Power
                } else {
                    Focus::Search
                };
                self.close_strip(back)?;
                return Ok(Action::None);
            }
            if !self.search.editor.borrow().is_empty() {
                self.search.editor.borrow_mut().clear();
                self.after_edit()?;
                self.set_focus(Focus::Search)?;
                return Ok(Action::None);
            }
            return Ok(Action::Close);
        }

        let focus = self.focus.get();
        let is_nav = matches!(vk, VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_TAB);
        if focus != Focus::Search && is_nav {
            self.keyboard_nav.set(true);
        }
        match focus {
            Focus::Search => self.search_key(vk, ctrl, shift),
            Focus::Cell(i) => self.grid_key(i, vk, shift),
            Focus::Profile | Focus::Power => self.footer_key(focus, vk, shift),
            Focus::Strip(i) => self.strip_key(i, vk, shift),
        }
    }

    fn search_key(&self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Result<Action> {
        const KEY_A: VIRTUAL_KEY = VIRTUAL_KEY(b'A' as u16);
        const KEY_C: VIRTUAL_KEY = VIRTUAL_KEY(b'C' as u16);
        const KEY_V: VIRTUAL_KEY = VIRTUAL_KEY(b'V' as u16);
        const KEY_X: VIRTUAL_KEY = VIRTUAL_KEY(b'X' as u16);

        // With results showing: Up/Down move the highlight (wrapping
        // around), Enter launches it, Tab goes to the footer.
        if self.is_list() {
            match vk {
                VK_DOWN => return self.move_selection(1).map(|()| Action::None),
                VK_UP => return self.move_selection(-1).map(|()| Action::None),
                VK_RETURN => return Ok(self.launch_result(self.selected.get())),
                VK_TAB => {
                    self.keyboard_nav.set(true);
                    let next = if shift { Focus::Power } else { Focus::Profile };
                    return self.set_focus(next).map(|()| Action::None);
                }
                _ => {}
            }
        }

        // Leaving the search box.
        match vk {
            VK_DOWN | VK_TAB if !shift => {
                self.keyboard_nav.set(true);
                let next = if self.grid.len() > 0 {
                    Focus::Cell(0)
                } else {
                    Focus::Profile
                };
                return self.set_focus(next).map(|()| Action::None);
            }
            VK_TAB => {
                self.keyboard_nav.set(true);
                return self.set_focus(Focus::Power).map(|()| Action::None);
            }
            // Nothing typed, nothing to launch.
            VK_RETURN => return Ok(Action::None),
            _ => {}
        }

        // The clipboard is read before borrowing the editor.
        let paste = if ctrl && vk == KEY_V {
            search_box::clipboard_text(self.hwnd)
        } else {
            None
        };
        let mut copy = None;
        let changed = {
            let mut e = self.search.editor.borrow_mut();
            match vk {
                VK_BACK => e.backspace(ctrl),
                VK_DELETE => e.delete(ctrl),
                VK_LEFT => {
                    e.left(ctrl, shift);
                    true
                }
                VK_RIGHT => {
                    e.right(ctrl, shift);
                    true
                }
                VK_HOME => {
                    e.home(shift);
                    true
                }
                VK_END => {
                    e.end(shift);
                    true
                }
                KEY_A if ctrl => {
                    e.select_all();
                    true
                }
                KEY_C if ctrl => {
                    copy = e.has_selection().then(|| e.selected_text().to_string());
                    false
                }
                KEY_X if ctrl => {
                    copy = e.cut();
                    copy.is_some()
                }
                KEY_V if ctrl => paste.is_some_and(|text| e.insert(&text)),
                _ => false,
            }
        };
        if let Some(text) = copy {
            search_box::set_clipboard_text(self.hwnd, &text);
        }
        if changed {
            self.after_edit()?;
        }
        Ok(Action::None)
    }

    fn grid_key(&self, i: usize, vk: VIRTUAL_KEY, shift: bool) -> Result<Action> {
        let n = self.grid.len();
        let cols = self.layout.borrow().columns.max(1);
        let last_row = (n - 1) / cols;
        let next = match vk {
            VK_LEFT => Focus::Cell(i.saturating_sub(1)),
            VK_RIGHT => Focus::Cell((i + 1).min(n - 1)),
            VK_UP if i < cols => Focus::Search,
            VK_UP => Focus::Cell(i - cols),
            VK_DOWN if i + cols < n => Focus::Cell(i + cols),
            // The row below is shorter: go to its last icon.
            VK_DOWN if i / cols < last_row => Focus::Cell(n - 1),
            VK_DOWN => Focus::Profile,
            VK_HOME => Focus::Cell(0),
            VK_END => Focus::Cell(n - 1),
            VK_TAB if shift && i == 0 => Focus::Search,
            VK_TAB if shift => Focus::Cell(i - 1),
            VK_TAB if i + 1 < n => Focus::Cell(i + 1),
            VK_TAB => Focus::Profile,
            VK_RETURN => return Ok(self.launch_pin(i)),
            _ => return Ok(Action::None),
        };
        self.set_focus(next)?;
        Ok(Action::None)
    }

    fn footer_key(&self, focus: Focus, vk: VIRTUAL_KEY, shift: bool) -> Result<Action> {
        let n = self.grid.len();
        // Above the footer: the last pin, or the search box (no pins, or
        // search results showing).
        let above = if n > 0 && !self.is_list() {
            Focus::Cell(n - 1)
        } else {
            Focus::Search
        };
        let next = match (focus, vk) {
            (Focus::Profile, VK_RETURN) => return Ok(Action::OpenAccountSettings),
            (Focus::Power, VK_RETURN) => {
                self.keyboard_nav.set(true);
                self.open_strip()?;
                return Ok(Action::None);
            }
            (_, VK_UP) => above,
            (Focus::Profile, VK_TAB) if shift => above,
            (Focus::Profile, VK_RIGHT | VK_TAB) => Focus::Power,
            (Focus::Power, VK_LEFT) => Focus::Profile,
            (Focus::Power, VK_TAB) if shift => Focus::Profile,
            (Focus::Power, VK_TAB) => Focus::Search,
            _ => return Ok(Action::None),
        };
        self.set_focus(next)?;
        Ok(Action::None)
    }

    fn strip_key(&self, i: usize, vk: VIRTUAL_KEY, shift: bool) -> Result<Action> {
        let n = self.strip.buttons().len();
        let next = match vk {
            VK_LEFT => i.saturating_sub(1),
            VK_RIGHT => (i + 1).min(n - 1),
            VK_TAB if shift => (i + n - 1) % n,
            VK_TAB => (i + 1) % n,
            VK_RETURN => {
                self.keyboard_nav.set(true);
                return self.press_strip(i);
            }
            _ => return Ok(Action::None),
        };
        self.set_focus(Focus::Strip(next))?;
        Ok(Action::None)
    }

    /// WM_CHAR: typing always goes to the search box.
    pub fn char_typed(&self, unit: u16) -> Result<()> {
        // Control characters (Backspace, Tab, Enter, Esc, Ctrl+letters...)
        // are handled as keys in `key_down`.
        let is_text = unit >= 0x20 && unit != 0x7F;
        if !is_text {
            return Ok(());
        }
        if self.focus.get() != Focus::Search {
            if self.strip.is_open() {
                self.close_strip(Focus::Search)?;
            }
            self.keyboard_nav.set(false);
            self.set_focus(Focus::Search)?;
        }
        if self.search.editor.borrow_mut().type_unit(unit) {
            self.after_edit()?;
        }
        Ok(())
    }

    /// The text or caret changed: repaint, restart the blink (solid while
    /// typing), and search again if the text changed.
    fn after_edit(&self) -> Result<()> {
        self.search.redraw_text(&self.gfx)?;
        if self.is_open.get() && self.focus.get() == Focus::Search {
            self.animator.blink(&self.search.caret_visual()?)?;
        }
        self.update_search(false)
    }

    // -----------------------------------------------------------------------
    // IME
    // -----------------------------------------------------------------------

    /// The caret in window pixels.
    fn caret_px(&self) -> Option<RECT> {
        let p = self.placement.get()?;
        let r = self.search.caret_rect();
        let s = p.scale;
        let menu_top = p.menu_top(self.layout.borrow().size.Y);
        let left = p.menu_x + (r.x * s).round() as i32;
        let top = menu_top + (r.y * s).round() as i32;
        Some(RECT {
            left,
            top,
            right: left + (r.w * s).ceil().max(1.0) as i32,
            bottom: menu_top + (r.bottom() * s).round() as i32,
        })
    }

    fn place_ime(&self) {
        if let Some(caret) = self.caret_px() {
            search_box::ime_place(self.hwnd, caret);
        }
    }

    pub fn ime_start(&self) -> Result<()> {
        if self.focus.get() != Focus::Search {
            if self.strip.is_open() {
                self.close_strip(Focus::Search)?;
            }
            self.set_focus(Focus::Search)?;
        }
        self.place_ime();
        Ok(())
    }

    /// WM_IME_COMPOSITION. `flags` says which strings changed.
    pub fn ime_composition(&self, flags: u32) -> Result<()> {
        if flags & search_box::IME_RESULT.0 != 0
            && let Some(result) = search_box::ime_string(self.hwnd, search_box::IME_RESULT)
        {
            let mut e = self.search.editor.borrow_mut();
            e.set_composition("");
            e.insert(&result);
        }
        let composing = if flags & search_box::IME_COMPOSITION.0 != 0 {
            search_box::ime_string(self.hwnd, search_box::IME_COMPOSITION).unwrap_or_default()
        } else {
            String::new()
        };
        self.search.editor.borrow_mut().set_composition(&composing);
        self.after_edit()?;
        self.place_ime();
        Ok(())
    }

    pub fn ime_end(&self) -> Result<()> {
        self.search.editor.borrow_mut().set_composition("");
        self.after_edit()
    }

    // -----------------------------------------------------------------------
    // Mouse (window pixels)
    // -----------------------------------------------------------------------

    pub fn mouse_move(&self, x: i32, y: i32) -> Result<()> {
        let Some(p) = self.to_menu(x, y) else {
            return Ok(());
        };
        if self.dragging.get() {
            if let Some(pos) = self.search.position_at(p) {
                self.search.editor.borrow_mut().set_caret_utf16(pos, true);
                self.after_edit()?;
            }
            return Ok(());
        }
        self.set_hovered(self.hit(p))
    }

    pub fn mouse_leave(&self) -> Result<()> {
        self.set_hovered(Target::Nothing)
    }

    /// True when the mouse is over the search box (show the I-beam cursor).
    pub fn over_text(&self, x: i32, y: i32) -> bool {
        self.to_menu(x, y)
            .is_some_and(|p| self.hit(p) == Target::Search)
    }

    pub fn mouse_down(&self, x: i32, y: i32, shift: bool) -> Result<Action> {
        let Some(p) = self.to_menu(x, y) else {
            return Ok(Action::None);
        };
        // The mouse is in charge now: hide the focus ring.
        self.keyboard_nav.set(false);
        let target = self.hit(p);
        if self.strip.is_open() && !matches!(target, Target::Strip(_) | Target::StripPanel) {
            // A click outside the strip only closes the strip.
            self.close_strip(Focus::Search)?;
            self.pressed.set(Target::Nothing);
            return Ok(Action::None);
        }
        self.pressed.set(target);
        if target == Target::Search {
            self.set_focus(Focus::Search)?;
            if let Some(pos) = self.search.position_at(p) {
                self.search.editor.borrow_mut().set_caret_utf16(pos, shift);
                self.after_edit()?;
            }
            self.dragging.set(true);
            // SAFETY: captures the mouse for our own window while selecting.
            unsafe { SetCapture(self.hwnd) };
        } else {
            self.update_focus_visuals()?;
        }
        Ok(Action::None)
    }

    pub fn mouse_up(&self, x: i32, y: i32) -> Result<Action> {
        if self.dragging.replace(false) {
            // SAFETY: releases the capture taken in `mouse_down`.
            let _ = unsafe { ReleaseCapture() };
        }
        let pressed = self.pressed.replace(Target::Nothing);
        let Some(p) = self.to_menu(x, y) else {
            return Ok(Action::None);
        };
        // A click = pressed and released on the same thing.
        if self.hit(p) != pressed {
            return Ok(Action::None);
        }
        match pressed {
            Target::Cell(i) => Ok(self.launch_pin(i)),
            Target::Row(i) => Ok(self.launch_result(i)),
            Target::Profile => Ok(Action::OpenAccountSettings),
            Target::Power if self.strip.is_open() => {
                self.close_strip(Focus::Search)?;
                Ok(Action::None)
            }
            Target::Power => {
                self.open_strip()?;
                Ok(Action::None)
            }
            Target::Strip(i) => self.press_strip(i),
            _ => Ok(Action::None),
        }
    }
}
