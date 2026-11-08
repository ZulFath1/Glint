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
pub mod chrome;
pub mod context_menu;
pub mod footer;
pub mod gfx;
pub mod grid;
pub mod layout;
pub mod music;
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
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{GetQueueStatus, KillTimer, QS_INPUT, SetTimer};
use windows::core::{Interface, Result};
use windows_numerics::{Vector2, Vector3};

use crate::apps::AppInfo;
use crate::config::{Config, Rgb, ThemeId};
use crate::icons::IconCache;
use crate::media::{self, MediaCommand, MusicView};
use crate::power::PowerAction;
use crate::search::{self, Query};
use crate::theme::LayoutKind;
use crate::window::{Edge, Placement};
use anim::{Animator, Kind, LayoutMotion};
use background::{Applied, SystemEffects};
use chrome::Chrome;
use context_menu::{ContextMenu, MenuTarget};
use footer::{Footer, PowerStrip, StripState};
use gfx::Gfx;
use grid::Grid;
use layout::{Content, Layout, MAX_RESULTS, Music, Rect};
use music::{CardPart, MusicCard};
use results::Results;
use search_box::SearchBox;
use shape::RoundRect;

/// The timer that fills in result icons a moment after typing (WM_TIMER id).
pub const TIMER_ICONS: usize = 2;
/// At most this long per WM_TIMER spent loading icons, so the thread is
/// never busy for longer than a frame.
const ICON_BUDGET: Duration = Duration::from_millis(8);
/// The timer that refreshes the music card's elapsed time (WM_TIMER id).
/// It only runs while the menu is open and music plays.
pub const TIMER_SEEK: usize = 3;
/// The timer that repaints the terminal title strip's clock, once a
/// minute, only while the menu is open (WM_TIMER id).
pub const TIMER_CLOCK: usize = 9;
/// At most 4 updates a second (the text changes once a second).
const SEEK_TICK_MS: u32 = 250;
/// The app icon next to "Playing from …".
const SOURCE_ICON: f32 = 16.0;

/// Something main.rs should do after an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    OpenAccountSettings,
    /// The footer gear: open the Settings window.
    OpenSettings,
    Power(PowerAction),
    /// Launch the app with this AppsFolder ID (AUMID).
    Launch(String),
    /// A music card button (or its keyboard shortcut).
    Media(MediaCommand),
    /// "Pin to Glint" / "Unpin from Glint" on this app ID (right-click).
    Pin(String),
    Unpin(String),
}

/// Who has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The search box (while there are results, Up/Down move the result
    /// highlight and Enter launches it).
    Search,
    Cell(usize),
    /// A button of the music card.
    Media(CardPart),
    Profile,
    /// The footer gear (Settings).
    Settings,
    /// The sidebar rail's Apps button.
    Apps,
    Power,
    /// A separate footer button: Lock, Restart, Shut down (some themes).
    Quick(usize),
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
    /// A button or the seek bar of the music card.
    Media(CardPart),
    Profile,
    Settings,
    Apps,
    Power,
    Quick(usize),
    Strip(usize),
    /// The power strip, but not on a button.
    StripPanel,
    /// The right-click menu's row ("Pin to Glint" / "Unpin from Glint").
    Context,
    /// The right-click menu, but not on its row.
    ContextPanel,
}

const RING_WIDTH: f32 = 2.0;

/// Corner radius of the hover / focus ring around a grid cell (theme).
fn cell_radius() -> f32 {
    gfx::theme().shape.cell_radius
}

/// Corner radius of the hover / focus ring around a button (theme).
fn button_radius() -> f32 {
    gfx::theme().shape.button_radius
}

/// Every installed app, prepared for searching, plus how often each was
/// launched (saved to `[stats]` in config.toml by main.rs).
#[derive(Default)]
struct Catalog {
    apps: Vec<AppInfo>,
    /// `items[i]` is `apps[i]`, prepared for the matcher.
    items: Vec<search::Item>,
    /// Pinned AUMIDs from the config (pinned apps win ties).
    pinned: Vec<String>,
    /// Lowercased ID → (the ID as written, launches). Looked up without
    /// case, saved with the original spelling.
    launches: HashMap<String, (String, u32)>,
}

impl Catalog {
    fn set_apps(&mut self, apps: Vec<AppInfo>) {
        self.items = apps
            .iter()
            .map(|a| search::Item::new(&a.name, self.is_pinned(&a.id), self.launches(&a.id)))
            .collect();
        self.apps = apps;
    }

    /// Recomputes every item's pinned flag and launch count (new pins, or
    /// launch counts from the config file).
    fn refresh_items(&mut self) {
        for (i, app) in self.apps.iter().enumerate() {
            let (pinned, launches) = (self.is_pinned(&app.id), self.launches(&app.id));
            if let Some(item) = self.items.get_mut(i) {
                item.pinned = pinned;
                item.launches = launches;
            }
        }
    }

    fn is_pinned(&self, id: &str) -> bool {
        self.pinned.iter().any(|p| p.eq_ignore_ascii_case(id))
    }

    fn launches(&self, id: &str) -> u32 {
        self.launches.get(&id.to_lowercase()).map_or(0, |(_, n)| *n)
    }

    fn set_launches(&mut self, counts: &HashMap<String, u32>) {
        self.launches = counts
            .iter()
            .map(|(id, &n)| (id.to_lowercase(), (id.clone(), n)))
            .collect();
        self.refresh_items();
    }

    fn launch_counts(&self) -> HashMap<String, u32> {
        self.launches.values().cloned().collect()
    }

    fn record_launch(&mut self, id: &str) {
        let entry = self
            .launches
            .entry(id.to_lowercase())
            .or_insert_with(|| (id.to_string(), 0));
        entry.1 = entry.1.saturating_add(1);
        let count = entry.1;
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
    compositor: Compositor,
    // Keeps our visual tree attached to the window.
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    shell: shell::Shell,
    menu: Visual,
    /// Rebuilt when `[animation]` in the config changes.
    animator: RefCell<Animator>,
    motion: LayoutMotion,
    gfx: Gfx,
    icons: IconCache,
    max_columns: Cell<usize>,
    layout: RefCell<Layout>,
    search: SearchBox,
    grid: Grid,
    results: Results,
    footer: Footer,
    strip: PowerStrip,
    /// The right-click "Pin / Unpin" menu.
    context: ContextMenu,
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
    music: MusicCard,
    /// Whether the music card would show (with the grid).
    music_mode: Cell<Music>,
    /// Use the album cover's color for the background (CLAUDE.md section 6).
    tint_from_album: Cell<bool>,
    /// Space just played/paused the music: ignore the WM_CHAR ' ' after it.
    swallow_space: Cell<bool>,
    /// Dragging the music card's seek bar.
    seek_drag: Cell<bool>,
    /// The last "Playing from" lookup: (AUMID, app name, app icon).
    source_cache: RefCell<Option<(String, String, Option<ID2D1Bitmap1>)>>,
    /// The high contrast text color in use (None = high contrast is off).
    high_contrast: Cell<Option<Rgb>>,
    /// Terminal themes' title strip and section label.
    chrome: Chrome,
    /// Icons in one color (`icon_tint`).
    icon_tint: Cell<bool>,
    /// The accent color in use (for the music bar after a theme switch).
    accent: Cell<Rgb>,
    /// (grid, rows) as last shown: what `sync_shown` fades in or out.
    shown: Cell<(bool, bool)>,
    /// The sidebar's Apps button is on: the most-used apps show instead
    /// of the pinned grid (while nothing is typed).
    apps_view: Cell<bool>,
}

/// True if a key press or mouse click is waiting in our message queue.
fn input_waiting() -> bool {
    // SAFETY: GetQueueStatus has no preconditions. The high word lists the
    // kinds of messages currently in the queue.
    let status = unsafe { GetQueueStatus(QS_INPUT) };
    (status >> 16) & QS_INPUT.0 != 0
}
impl Ui {
    /// `accent` is the config's accent color, already resolved (a fixed
    /// color, or the Windows one for `accent = "system"`).
    pub fn new(hwnd: HWND, config: &Config, accent: Rgb, scale: f32) -> Result<Self> {
        // Everything is painted with the configured theme from the start.
        gfx::set_theme(config.theme.theme());
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
        let layout = layout::layout(
            apps.len(),
            config.max_columns,
            Content::Grid,
            Music::Hidden,
            gfx::theme(),
        );

        let shell = shell::Shell::new(&compositor, layout.size)?;
        shell.set_theme(gfx::theme(), layout.size)?;
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
        let hover = RoundRect::new(&compositor, cell_radius(), 0.0)?;
        hover.set_fill(gfx::paint(gfx::theme().palette.hover))?;
        hover.show(false)?;
        hover_layer.Children()?.InsertAtTop(&hover.visual)?;
        let search = SearchBox::new(&compositor, &gfx, &front, layout.search, accent)?;
        let grid = Grid::new(&compositor, &gfx, &front, apps, config.show_names)?;
        grid.set_icon_tint(config.icon_tint);
        let chrome = Chrome::new(&compositor, &gfx, &front, footer.user_name())?;
        let results = Results::new(&compositor, &gfx, &front, &layout, &footer.fill_visual()?)?;
        results.set_theme(config.icon_tint)?;
        let music = MusicCard::new(
            &compositor,
            &gfx,
            &front,
            &footer.fill_visual()?,
            accent,
            &layout,
        )?;
        let strip = PowerStrip::new(&compositor, &gfx, &popup, layout.strip)?;
        let context = ContextMenu::new(&compositor, &gfx, &popup)?;
        let ring = RoundRect::new(&compositor, cell_radius(), RING_WIDTH)?;
        ring.set_stroke(gfx::color(accent, 1.0))?;
        ring.show(false)?;
        top.Children()?.InsertAtTop(&ring.visual)?;

        // Everything that moves or resizes when the menu height changes.
        for (object, kind) in shell
            .motion_objects()?
            .into_iter()
            .chain(footer.motion_objects()?)
            .chain(music.motion_objects()?)
        {
            motion.add(object, kind);
        }
        motion.add(strip.motion_object()?, Kind::Visual);
        motion.add(chrome.section_visual()?.cast()?, Kind::Visual);
        // The grid's cells glide when pins are added or removed.
        motion.set_group(grid.motion_objects()?)?;

        // Until the full app list arrives (a moment after startup), search
        // at least finds the pinned apps.
        let mut catalog = Catalog {
            pinned: config.pinned.clone(),
            ..Default::default()
        };
        catalog.set_launches(&config.stats.launch_counts);
        catalog.set_apps(grid.apps());

        let ui = Self {
            hwnd,
            compositor,
            _target: target,
            root,
            shell,
            menu,
            animator: RefCell::new(animator),
            motion,
            gfx,
            icons: IconCache::default(),
            max_columns: Cell::new(config.max_columns),
            layout: RefCell::new(layout),
            search,
            grid,
            results,
            footer,
            strip,
            context,
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
            music,
            music_mode: Cell::new(Music::Hidden),
            tint_from_album: Cell::new(config.background.tint_from_album),
            swallow_space: Cell::new(false),
            seek_drag: Cell::new(false),
            source_cache: RefCell::new(None),
            high_contrast: Cell::new(None),
            chrome,
            icon_tint: Cell::new(config.icon_tint),
            accent: Cell::new(accent),
            shown: Cell::new((true, false)),
            apps_view: Cell::new(false),
        };
        ui.refresh()?;
        // The list layout starts with its most-used apps.
        ui.update_search(true)?;
        ui.sync_shown(false)?;
        Ok(ui)
    }

    /// The largest the menu gets, in DIPs (the window is made this big, so
    /// it never has to move while the menu grows or shrinks).
    pub fn max_size(&self) -> Vector2 {
        layout::max_size(self.grid.len(), self.max_columns.get(), gfx::theme())
    }

    /// The point the power strip grows out of (the power button's top, or
    /// the middle separate footer button's).
    fn strip_origin(l: &Layout) -> Vector2 {
        let b = l.quick.get(1).copied().unwrap_or(l.power);
        Vector2 {
            X: b.x + b.w / 2.0,
            Y: b.y,
        }
    }

    /// Positions and paints everything at the current DPI scale. Instant:
    /// nothing glides.
    fn refresh(&self) -> Result<()> {
        let l = self.layout.borrow().clone();
        self.motion.instant(|| {
            self.search.set_rect(&self.gfx, l.search)?;
            self.grid.set_small(l.kind == LayoutKind::List);
            self.grid.set_rects(&self.gfx, &l.cells)?;
            self.grid.redraw(&self.gfx, &self.icons)?;
            self.footer.refresh(&self.gfx, &l)?;
            self.strip
                .refresh(&self.gfx, l.strip, Self::strip_origin(&l))?;
            self.results.resize(&self.gfx, &l)?;
            self.music.refresh(&self.gfx, &l)?;
            self.context.refresh(&self.gfx)?;
            self.chrome.refresh(&self.gfx, &l)?;
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
        self.music.move_to(&self.gfx, &l)?;
        // Going back to the grid, the rows stay as they are while they fade.
        if let Content::List(n) = l.content {
            self.results.set_rows(&self.gfx, &l, n == 0)?;
        }
        // The list layout's "PINNED" label and icon strip sit under the
        // rows, so they move when the number of rows changes.
        if l.kind == LayoutKind::List {
            self.grid.set_rects(&self.gfx, &l.cells)?;
            self.chrome.move_section(&self.gfx, &l)?;
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
        self.refetch_source_icon();
        self.refresh()
    }

    /// The music card keeps the playing app's icon as a GPU bitmap. After a
    /// device reset it belongs to the dead device (drawing it would fail),
    /// and after a DPI change it has the wrong size: fetch it again.
    fn refetch_source_icon(&self) {
        self.source_cache.borrow_mut().take();
        let icon = self
            .music
            .view()
            .and_then(|v| self.source_app(&v.source_id).1);
        self.music.set_source_icon(icon);
    }

    /// The menu is hidden: let Direct2D free its scratch textures.
    pub fn release_caches(&self) {
        self.gfx.release_caches();
    }

    /// Before opening: make sure the GPU device survived while hidden.
    pub fn ensure_device(&self) {
        self.gfx.ensure_device();
    }

    /// Debug (tray, Shift + right-click; and `--stress`): a fake GPU reset.
    pub fn simulate_device_loss(&self) {
        self.gfx.simulate_device_loss();
    }

    /// The album cover's size in pixels at the current DPI.
    pub fn cover_px(&self) -> u32 {
        self.gfx.px(layout::COVER) as u32
    }

    /// Applies the DPI scale and the menu's position inside the window.
    pub fn set_layout(&self, p: &Placement) -> Result<()> {
        self.placement.set(Some(*p));
        if (p.scale - self.gfx.scale()).abs() > 0.001 {
            log_info!("DPI scale {} -> {}: repainting", self.gfx.scale(), p.scale);
            self.gfx.set_scale(p.scale);
            self.icons.clear();
            self.refetch_source_icon();
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
        let text = effects.high_contrast.map(|hc| hc.text);
        if self.high_contrast.replace(text) != text {
            // Every text color changes: repaint all surfaces.
            gfx::set_high_contrast_text(text);
            self.refresh()?;
        }
        // A theme switch while open cross-fades the colors.
        let animate = self.is_open.get() && !self.reduced_motion.get();
        self.shell
            .background
            .apply(&config.background, config.theme, effects, animate)
    }

    /// A new theme and/or one-color icons (IMPROVE.md milestone 9).
    /// Everything is recolored and repainted; while open, the content fades
    /// in again over 250 ms (the background cross-fades in
    /// `apply_background`, which main.rs calls right after this).
    ///
    /// The window may have to grow first (a terminal theme is taller):
    /// `fit_window` works like in `set_pins`.
    pub fn set_theme(
        &self,
        id: ThemeId,
        icon_tint: bool,
        fit_window: impl FnOnce(Vector2) -> Option<Placement>,
    ) -> Result<()> {
        if gfx::theme().id == id && self.icon_tint.get() == icon_tint {
            return Ok(());
        }
        let theme = id.theme();
        gfx::set_theme(theme);
        self.apps_view.set(false);
        self.icon_tint.set(icon_tint);
        self.grid.set_icon_tint(icon_tint);

        // Things are about to change shape: no popup or highlight left behind.
        self.context.reset()?;
        self.strip.reset()?;
        self.hovered.set(Target::Nothing);
        self.hover.show(false)?;
        if matches!(
            self.focus.get(),
            Focus::Strip(_) | Focus::Power | Focus::Quick(_)
        ) {
            self.focus.set(Focus::Search);
        }

        // Composition colors and shapes (no painting yet).
        self.hover.set_fill(gfx::paint(theme.palette.hover))?;
        self.search.set_theme()?;
        self.footer.set_theme()?;
        self.strip.set_theme()?;
        self.context.set_theme()?;
        self.music.set_theme(self.accent.get())?;
        self.results.set_theme(icon_tint)?;

        if let Some(p) = fit_window(self.max_size()) {
            self.set_layout(&p)?;
        }
        let new = {
            let l = self.layout.borrow();
            layout::layout(
                self.grid.len(),
                self.max_columns.get(),
                l.content,
                l.music,
                theme,
            )
        };
        self.shell.set_theme(theme, new.size)?;
        *self.layout.borrow_mut() = new;
        // Repaint everything in the new colors and font.
        self.refresh()?;
        self.update_search(true)?;
        // A different layout shows different parts: at once.
        self.sync_shown(false)?;
        self.sync_clock();

        if self.is_open.get() && !self.reduced_motion.get() {
            let a = self.compositor.CreateScalarKeyFrameAnimation()?;
            a.InsertKeyFrame(0.0, 0.0)?;
            a.InsertKeyFrame(1.0, 1.0)?;
            a.SetDuration(anim::ms(background::THEME_FADE_MS))?;
            self.shell
                .body()
                .StartAnimation(windows::core::h!("Opacity"), &a)?;
        }
        log_info!("theme: {} (icon tint {icon_tint})", id.name());
        Ok(())
    }

    /// The terminal title strip's clock runs only while the menu is open
    /// (one repaint a minute, aligned to the minute); otherwise its timer
    /// is stopped.
    fn sync_clock(&self) {
        let ticking = self.is_open.get() && Chrome::has_clock(&self.layout.borrow());
        // SAFETY: a timer on our own window, or stopping it.
        unsafe {
            if ticking {
                SetTimer(
                    Some(self.hwnd),
                    TIMER_CLOCK,
                    chrome::ms_to_next_minute(),
                    None,
                );
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
            }
        }
    }

    /// WM_TIMER for [`TIMER_CLOCK`]: a new minute.
    pub fn clock_tick(&self) -> Result<()> {
        self.chrome.tick(&self.gfx)?;
        self.sync_clock();
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Settings that change while running (config.toml reloads)
    // -----------------------------------------------------------------------

    /// New accent color: caret, focused search border, seek bar, focus ring.
    pub fn set_accent(&self, accent: Rgb) -> Result<()> {
        self.accent.set(accent);
        self.results.set_accent(accent)?;
        self.search.set_accent(&self.gfx, accent)?;
        self.music.set_accent(accent)?;
        self.ring.set_stroke(gfx::color(accent, 1.0))
    }

    /// New open/close timings and bounce: the animations are built again
    /// (they are only templates, so nothing running is affected).
    pub fn set_animation(&self, cfg: &crate::config::Animation) -> Result<()> {
        let animator = Animator::new(&self.compositor, cfg)?;
        *self.animator.borrow_mut() = animator;
        Ok(())
    }

    /// App names under the pinned icons on or off.
    pub fn set_show_names(&self, on: bool) -> Result<()> {
        if self.grid.set_show_names(on) {
            self.grid.redraw(&self.gfx, &self.icons)?;
        }
        Ok(())
    }

    /// Plays the open animation again from the start (Settings: "Replay
    /// animation", or after an animation value changed). Only while open.
    pub fn replay_open(&self, reduced_motion: bool) -> Result<()> {
        if !self.is_open.get() {
            return Ok(());
        }
        Animator::reset_closed(&self.menu)?;
        self.animate_open(true, reduced_motion)
    }

    /// The compositor and painting devices, shared with the Settings
    /// window (one GPU device for the whole app).
    pub fn compositor(&self) -> &Compositor {
        &self.compositor
    }

    pub fn gfx(&self) -> &Gfx {
        &self.gfx
    }

    /// Album tint on or off. Call `set_music` afterwards to apply it.
    pub fn set_tint_from_album(&self, on: bool) {
        self.tint_from_album.set(on);
    }

    /// The launch counts, for saving (IDs as first seen).
    pub fn launch_counts(&self) -> HashMap<String, u32> {
        self.catalog.borrow().launch_counts()
    }

    /// Launch counts from the config file (it was edited by hand).
    pub fn set_launch_counts(&self, counts: &HashMap<String, u32>) {
        self.catalog.borrow_mut().set_launches(counts);
    }

    /// New pins and/or column limit. Cells of apps that stay pinned glide
    /// to their new place (220 ms) while the menu changes size; a new pin
    /// pops in. Uninstalled pins are skipped.
    ///
    /// The window may have to grow first: `fit_window` gets the new largest
    /// menu size (DIPs), moves the window if needed and returns its new
    /// placement. That happens before anything glides, so moving the window
    /// can't cut a glide short.
    pub fn set_pins(
        &self,
        pins: &[String],
        max_columns: usize,
        fit_window: impl FnOnce(Vector2) -> Option<Placement>,
    ) -> Result<()> {
        let apps: Vec<crate::apps::App> = pins
            .iter()
            .filter_map(|aumid| crate::apps::find(aumid))
            .collect();
        let count = apps.len();
        self.max_columns.set(max_columns);
        {
            let mut catalog = self.catalog.borrow_mut();
            catalog.pinned = pins.to_vec();
            catalog.refresh_items();
        }
        // Things are about to move: no popup or highlight left behind.
        self.context.reset()?;
        if self.strip.is_open() {
            self.strip.reset()?;
            if matches!(self.focus.get(), Focus::Strip(_)) {
                self.focus.set(Focus::Search);
            }
        }
        self.hovered.set(Target::Nothing);
        self.hover.show(false)?;
        if let Focus::Cell(i) = self.focus.get()
            && i >= count
        {
            self.focus.set(if count > 0 {
                Focus::Cell(count - 1)
            } else {
                Focus::Search
            });
        }

        // The window first (instant, the menu stays where it is on screen).
        if let Some(p) = fit_window(layout::max_size(count, max_columns, gfx::theme())) {
            self.set_layout(&p)?;
        }

        let (content, music, old_width) = {
            let l = self.layout.borrow();
            (l.content, l.music, l.size.X)
        };
        let new = layout::layout(count, max_columns, content, music, gfx::theme());
        let added = self.grid.set_apps(&self.gfx, apps)?;
        // New cells start in their place (painted), then join the gliders.
        self.grid.place(&self.gfx, &new.cells, &added)?;
        self.grid.redraw_cells(&self.gfx, &self.icons, &added)?;
        self.motion.set_group(self.grid.motion_objects()?)?;
        let cells = new.cells.clone();
        let width_changed = (new.size.X - old_width).abs() > 0.5;
        *self.layout.borrow_mut() = new;

        if width_changed {
            // A different number of columns: the search box, footer and
            // results change width too, and those are repainted, so this
            // one happens at once (only with fewer pins than columns).
            self.results.forget();
            self.refresh()?;
            return self.update_search(true);
        }

        let animate = self.is_open.get() && !self.reduced_motion.get();
        let place = || {
            self.grid.set_rects(&self.gfx, &cells)?;
            self.place_content()
        };
        if animate {
            self.motion.animate(place)?;
        } else {
            self.motion.instant(place)?;
        }
        // While open, a new pin pops in like the icons do on open (when
        // closed, the next open's stagger shows it).
        if self.is_open.get() {
            let visuals = self.grid.animated_visuals()?;
            let new_ones: Vec<Visual> = added
                .iter()
                .filter_map(|&i| visuals.get(i).cloned())
                .collect();
            if animate {
                for v in &new_ones {
                    self.animator.borrow().pop_icon(v)?;
                }
            } else {
                Animator::stagger_done(&new_ones)?;
            }
        }
        // Search results show "pinned first" and the list may be wider now.
        if self.is_list() {
            self.results.forget();
            self.update_search(true)?;
        }
        self.update_focus_visuals()
    }

    /// Call before showing a fully hidden window: the menu at scale 0, the
    /// search box empty and the pinned grid back (all instant).
    pub fn reset_closed(&self) -> Result<()> {
        Animator::reset_closed(&self.menu)?;
        self.is_open.set(false);
        // The sidebar opens on the pinned apps again.
        self.apps_view.set(false);
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
            self.context.reset()?;
            self.keyboard_nav.set(false);
            self.pressed.set(Target::Nothing);
            self.dragging.set(false);
            self.hovered.set(Target::Nothing);
            self.hover.show(false)?;
            self.focus.set(Focus::Search);
            let icons = self.grid.animated_visuals()?;
            let card = self.music.animated()?;
            if reduced_motion {
                Animator::stagger_done(&icons)?;
                Animator::stagger_done(&[card])?;
            } else {
                self.animator.borrow().stagger_in(&icons)?;
                if self.music_shown() {
                    // 40 ms after the last icon.
                    self.animator.borrow().pop_card(&card, icons.len())?;
                }
            }
        }
        self.update_focus_visuals()?;
        self.sync_seek()?;
        // The clock may be minutes old: repaint it, then tick each minute.
        self.chrome.tick(&self.gfx)?;
        self.sync_clock();
        self.animator.borrow().open(&self.menu, reduced_motion)
    }

    pub fn animate_close(&self, reduced_motion: bool) -> Result<CompositionScopedBatch> {
        self.is_open.set(false);
        if self.seek_drag.replace(false) {
            // SAFETY: releases the mouse capture we took; no pointers.
            let _ = unsafe { ReleaseCapture() };
            self.music.cancel_drag();
        }
        // The seek bar and the clock stop: nothing runs while hidden.
        self.sync_seek()?;
        self.sync_clock();
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
        self.animator.borrow().close(&self.menu, reduced_motion)
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
        // "Playing from" may find its app (and icon) now.
        self.source_cache.borrow_mut().take();
        self.update_search(true)
    }

    /// Counts a launch (more launches = higher in ties).
    pub fn record_launch(&self, id: &str) {
        self.catalog.borrow_mut().record_launch(id);
    }

    /// Are rows (search results, or the list layout's most-used apps)
    /// showing?
    fn is_list(&self) -> bool {
        matches!(self.layout.borrow().content, Content::List(_))
    }

    fn kind(&self) -> LayoutKind {
        self.layout.borrow().kind
    }

    /// Is the pinned grid showing? (Always in the list layout: there it is
    /// the icon strip under the rows.)
    fn grid_shown(&self) -> bool {
        self.kind() == LayoutKind::List || !self.is_list()
    }

    /// The list layout with nothing typed: the most-launched apps, then
    /// the pinned ones, as indexes into `catalog.apps`.
    fn most_used(&self) -> Vec<usize> {
        let catalog = self.catalog.borrow();
        let mut used: Vec<(usize, u32)> = catalog
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.launches > 0)
            .map(|(i, item)| (i, item.launches))
            .collect();
        // Most launches first; ties alphabetically (stable).
        used.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| catalog.apps[a.0].name.cmp(&catalog.apps[b.0].name))
        });
        let mut list: Vec<usize> = used.into_iter().map(|(i, _)| i).collect();
        for pin in &catalog.pinned {
            if let Some(i) = catalog
                .apps
                .iter()
                .position(|a| a.id.eq_ignore_ascii_case(pin))
                && !list.contains(&i)
            {
                list.push(i);
            }
        }
        list.truncate(layout::MOST_USED);
        list
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
            self.search.set_count(&self.gfx, None)?;
            if self.kind() == LayoutKind::List || self.apps_view.get() {
                // Nothing typed: the list layout (or the sidebar's Apps
                // view) shows the most-used apps.
                let hits = self.most_used();
                let n = hits.len();
                *self.hits.borrow_mut() = hits;
                self.selected.set(0);
                self.results
                    .set_message(&self.gfx, results::TYPE_TO_SEARCH)?;
                self.set_content(Content::List(n))?;
                return self.paint_results();
            }
            self.hits.borrow_mut().clear();
            return self.set_content(Content::Grid);
        }
        self.results.set_message(&self.gfx, results::NO_RESULTS)?;
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
        self.search.set_count(&self.gfx, Some(n))?;
        self.set_content(Content::List(n))?;
        self.paint_results()
    }

    /// Fades the grid, the rows and the section label in or out to match
    /// the layout (the rows replace the grid, except in the list layout,
    /// where both show). `animate` = fade, else at once.
    fn sync_shown(&self, animate: bool) -> Result<()> {
        let (grid, rows) = (self.grid_shown(), self.is_list());
        let (old_grid, old_rows) = self.shown.replace((grid, rows));
        if old_grid != grid {
            self.motion.fade(&self.grid.visual()?, grid, animate)?;
            self.motion
                .fade(&self.chrome.section_visual()?, grid, animate)?;
            if !grid && matches!(self.focus.get(), Focus::Cell(_)) {
                self.focus.set(Focus::Search);
            }
        }
        if old_rows != rows {
            self.motion.fade(&self.results.visual()?, rows, animate)?;
        }
        Ok(())
    }

    /// Switches the middle of the menu to `content`. While open, the menu
    /// glides to its new height and the grid and list cross-fade.
    fn set_content(&self, content: Content) -> Result<()> {
        self.relayout(content, self.music_mode.get())
    }

    /// Lays the menu out for `content` and the music card. While open,
    /// everything glides to its new place (220 ms) and what appears or
    /// disappears fades.
    fn relayout(&self, content: Content, music: Music) -> Result<()> {
        let new = layout::layout(
            self.grid.len(),
            self.max_columns.get(),
            content,
            music,
            gfx::theme(),
        );
        let (old_content, old_music) = {
            let l = self.layout.borrow();
            (l.content, l.music)
        };
        if old_content == new.content && old_music == new.music {
            return Ok(());
        }
        let card_visible = new.music != Music::Hidden;
        *self.layout.borrow_mut() = new;
        let animate = self.is_open.get() && !self.reduced_motion.get();
        if animate {
            self.motion.animate(|| self.place_content())?;
        } else {
            self.motion.instant(|| self.place_content())?;
        }
        self.sync_shown(animate)?;
        if (old_music != Music::Hidden) != card_visible {
            self.motion
                .fade(&self.music.visual()?, card_visible, animate)?;
            if !card_visible && matches!(self.focus.get(), Focus::Media(_)) {
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
        let px = self.gfx.px(results::icon_size());
        let mut missing = false;
        // First: the list layout paints the selected row differently.
        let selected = (!hits.is_empty()).then(|| self.selected.get());
        self.results.set_selected(selected)?;
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
        let px = self.gfx.px(results::icon_size());
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
        // Repaints the rows that change look (list layout); else only the
        // highlight moves.
        self.paint_results()
    }

    // -----------------------------------------------------------------------
    // Music card
    // -----------------------------------------------------------------------

    /// Is the music card showing (music plays or is paused, and no search)?
    fn music_shown(&self) -> bool {
        self.layout.borrow().music != Music::Hidden
    }

    /// New music state from `media.rs` (`None` = nothing playing or
    /// paused). The card appears, disappears or changes; while the menu is
    /// open the menu glides to its new height.
    pub fn set_music(&self, view: Option<MusicView>) -> Result<()> {
        let mode = match &view {
            None => Music::Hidden,
            Some(v) if v.sources >= 2 => Music::CardWithSources,
            Some(_) => Music::Card,
        };
        let album = view
            .as_ref()
            .and_then(|v| v.info.cover.as_ref())
            .and_then(|c| c.color)
            .filter(|_| self.tint_from_album.get());
        let (name, icon) = match &view {
            Some(v) => self.source_app(&v.source_id),
            None => (String::new(), None),
        };
        self.music.set_view(&self.gfx, view, name, icon)?;
        if self.music_mode.replace(mode) != mode {
            let content = self.layout.borrow().content;
            self.relayout(content, mode)?;
        }
        if let Err(e) = self.shell.background.set_album_color(album) {
            log_warn!("album tint: {e}");
        }
        self.sync_seek()
    }

    /// The seek bar moves (and the elapsed time ticks) only while the menu
    /// is open and the card shows; otherwise both stop.
    fn sync_seek(&self) -> Result<()> {
        // The list layout's music row has no seek bar: nothing to move.
        let moving = self.is_open.get() && self.music_shown() && self.music.has_seek_bar();
        self.music.sync_seek(moving)?;
        // SAFETY: a timer on our own window, or stopping it.
        unsafe {
            if moving && self.music.is_playing() {
                SetTimer(Some(self.hwnd), TIMER_SEEK, SEEK_TICK_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_SEEK);
            }
        }
        Ok(())
    }

    /// WM_TIMER for [`TIMER_SEEK`]: refresh the elapsed time.
    pub fn music_tick(&self) -> Result<()> {
        if !self.is_open.get() {
            // SAFETY: stops our own timer.
            let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_SEEK) };
            return Ok(());
        }
        self.music.tick(&self.gfx)
    }

    /// The name and icon of the app behind a media session, found in the
    /// same app list search uses.
    fn source_app(&self, aumid: &str) -> (String, Option<ID2D1Bitmap1>) {
        if let Some((id, name, icon)) = self.source_cache.borrow().as_ref()
            && id == aumid
        {
            return (name.clone(), icon.clone());
        }
        let app = {
            let catalog = self.catalog.borrow();
            let apps = catalog
                .apps
                .iter()
                .map(|a| (a.id.as_str(), a.name.as_str()));
            media::match_source(apps, aumid).and_then(|i| catalog.apps.get(i).cloned())
        };
        let (name, icon) = match app {
            Some(app) => {
                let px = self.gfx.px(SOURCE_ICON);
                let icon = app
                    .shell_item()
                    .and_then(|item| self.icons.get(&self.gfx, &app.id, &item, px))
                    .inspect_err(|e| log_warn!("no icon for {}: {e}", app.id))
                    .ok();
                (app.name, icon)
            }
            None => (media::source_fallback_name(aumid), None),
        };
        *self.source_cache.borrow_mut() = Some((aumid.to_string(), name.clone(), icon.clone()));
        (name, icon)
    }

    /// A card button was clicked or pressed with the keyboard.
    fn press_card(&self, part: CardPart) -> Action {
        if !self.music.is_enabled(part) {
            return Action::None;
        }
        part.command().map_or(Action::None, Action::Media)
    }

    /// Where focus goes below the pinned grid: the card's first button, or
    /// the profile.
    fn below_grid(&self) -> Focus {
        match self.music.focus_order().first() {
            Some(&part) if self.music_shown() && self.music.has_seek_bar() => Focus::Media(part),
            _ => self.footer_first(),
        }
    }

    /// The first footer stop after the music card (or its row).
    fn after_music(&self) -> Focus {
        self.footer_order()
            .into_iter()
            .find(|f| !matches!(f, Focus::Media(_)))
            .unwrap_or(Focus::Settings)
    }

    fn media_key(&self, part: CardPart, vk: VIRTUAL_KEY, shift: bool) -> Result<Action> {
        let order = self.music.focus_order();
        let i = order.iter().position(|&p| p == part);
        let before = i
            .and_then(|i| i.checked_sub(1))
            .and_then(|j| order.get(j))
            .map(|&p| Focus::Media(p));
        let after = i.and_then(|i| order.get(i + 1)).map(|&p| Focus::Media(p));
        let n = self.grid.len();
        let above = if n > 0 && self.grid_shown() {
            Focus::Cell(n - 1)
        } else {
            Focus::Search
        };
        let below = self.after_music();
        let next = match vk {
            VK_RETURN => return Ok(self.press_card(part)),
            VK_SPACE => {
                self.swallow_space.set(true);
                return Ok(self.press_card(part));
            }
            VK_LEFT => before.unwrap_or(Focus::Media(part)),
            VK_RIGHT => after.unwrap_or(if self.music.has_seek_bar() {
                Focus::Media(part)
            } else {
                below
            }),
            VK_UP => above,
            VK_DOWN => below,
            VK_TAB if shift => before.unwrap_or(above),
            VK_TAB => after.unwrap_or(below),
            _ => return Ok(Action::None),
        };
        self.set_focus(next)?;
        Ok(Action::None)
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
            Focus::Cell(i) => self.grid.rect(i).map(|r| (r, cell_radius())),
            Focus::Media(part) => Some((self.music.part_rect(part), self.music.part_radius(part))),
            Focus::Profile => Some((self.footer.profile_rect(), button_radius())),
            Focus::Settings => Some((self.footer.settings_rect(), button_radius())),
            Focus::Apps => Some((self.footer.apps_rect(), button_radius())),
            Focus::Power => Some((self.footer.power_rect(), button_radius())),
            Focus::Quick(i) => self
                .footer
                .quick_rects()
                .get(i)
                .map(|&r| (r, button_radius())),
            Focus::Strip(i) => self.strip.buttons().get(i).map(|&r| (r, button_radius())),
        }
    }

    fn update_focus_visuals(&self) -> Result<()> {
        let focus = self.focus.get();
        let in_search = focus == Focus::Search;
        self.search.set_focused(in_search)?;
        let caret = self.search.caret_visual()?;
        if in_search && self.is_open.get() {
            self.animator.borrow().blink(&caret)?;
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
        self.context.set_hover(target == Target::Context)?;
        // The card has its own highlight (drawn above its dark panel).
        self.music.set_hover(match target {
            Target::Media(part) => Some(part),
            _ => None,
        })?;
        let highlight = match target {
            Target::Cell(i) => self.grid.rect(i).map(|r| (r, cell_radius())),
            Target::Row(i) => self.results.rect(i).map(|r| (r, results::row_radius())),
            Target::Profile => Some((self.footer.profile_rect(), button_radius())),
            Target::Settings => Some((self.footer.settings_rect(), button_radius())),
            Target::Apps => Some((self.footer.apps_rect(), button_radius())),
            Target::Power => Some((self.footer.power_rect(), button_radius())),
            Target::Quick(i) => self
                .footer
                .quick_rects()
                .get(i)
                .map(|&r| (r, button_radius())),
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
        if self.context.is_open() {
            if self.context.row().contains(p) {
                return Target::Context;
            }
            if self.context.rect().contains(p) {
                return Target::ContextPanel;
            }
        }
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
        } else if let Some(i) = self.grid.hit(p).filter(|_| self.grid_shown()) {
            Target::Cell(i)
        } else if let Some(part) = self.music.hit(p).filter(|_| self.music_shown()) {
            Target::Media(part)
        } else if self.footer.apps_rect().contains(p) {
            Target::Apps
        } else if self.footer.profile_rect().contains(p) {
            Target::Profile
        } else if self.footer.settings_rect().contains(p) {
            Target::Settings
        } else if let Some(i) = self.footer.quick_rects().iter().position(|r| r.contains(p)) {
            Target::Quick(i)
        } else if !self.layout.borrow().separate_power_buttons()
            && self.footer.power_rect().contains(p)
        {
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
    // Right-click menu (Pin / Unpin)
    // -----------------------------------------------------------------------

    /// WM_CONTEXTMENU: a right-click at (`x`, `y`) window pixels, or `None`
    /// for the keyboard (Shift+F10 or the Menu key), which uses the focused
    /// icon or the highlighted search result. Opens the menu for that app,
    /// or closes it when there is no app there.
    pub fn context_menu(&self, at: Option<(i32, i32)>) -> Result<()> {
        if !self.is_open.get() {
            return Ok(());
        }
        let found = match at {
            Some((x, y)) => {
                let Some(p) = self.to_menu(x, y) else {
                    return Ok(());
                };
                let id = match self.hit(p) {
                    // A right-click on the menu itself does nothing.
                    Target::Context | Target::ContextPanel => return Ok(()),
                    Target::Cell(i) => self.grid.aumid(i),
                    Target::Row(i) => self.result_id(i),
                    _ => None,
                };
                id.map(|id| (id, p))
            }
            None => {
                let (id, rect) = match self.focus.get() {
                    Focus::Cell(i) => (self.grid.aumid(i), self.grid.rect(i)),
                    Focus::Search if self.is_list() => {
                        let row = self.selected.get();
                        (self.result_id(row), self.results.rect(row))
                    }
                    _ => (None, None),
                };
                // Out of the lower part of the icon or row.
                id.zip(rect).map(|(id, r)| {
                    let p = Vector2 {
                        X: r.x + r.w / 2.0,
                        Y: r.y + r.h * 0.75,
                    };
                    (id, p)
                })
            }
        };
        let Some((id, p)) = found else {
            return self.close_context();
        };
        if self.strip.is_open() {
            self.close_strip(self.focus.get())?;
        }
        let pinned = self.catalog.borrow().is_pinned(&id);
        let size = self.layout.borrow().size;
        self.context.open(
            &self.gfx,
            &self.animator.borrow(),
            MenuTarget { id, pinned },
            p,
            size,
            self.reduced_motion.get(),
        )?;
        // The mouse may already be over the new row.
        self.hovered.set(Target::Nothing);
        Ok(())
    }

    fn close_context(&self) -> Result<()> {
        if !self.context.is_open() {
            return Ok(());
        }
        self.context
            .close(&self.animator.borrow(), self.reduced_motion.get())?;
        if matches!(self.hovered.get(), Target::Context | Target::ContextPanel) {
            self.set_hovered(Target::Nothing)?;
        }
        Ok(())
    }

    /// The menu's row was clicked (or Enter): pin or unpin, and close it.
    fn activate_context(&self) -> Result<Action> {
        let Some(target) = self.context.target() else {
            return Ok(Action::None);
        };
        self.close_context()?;
        Ok(if target.pinned {
            Action::Unpin(target.id)
        } else {
            Action::Pin(target.id)
        })
    }

    // -----------------------------------------------------------------------
    // Power strip
    // -----------------------------------------------------------------------

    fn open_strip(&self) -> Result<()> {
        self.strip.open(
            &self.gfx,
            &self.animator.borrow(),
            self.reduced_motion.get(),
        )?;
        self.set_focus(Focus::Strip(0))
    }

    fn close_strip(&self, focus_after: Focus) -> Result<()> {
        self.strip
            .close(&self.animator.borrow(), self.reduced_motion.get())?;
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
                // Cancel with separate footer buttons: close the strip,
                // back to the button that opened it.
                if self.layout.borrow().separate_power_buttons() {
                    let home = self.strip_home();
                    self.close_strip(home)?;
                    return Ok(Action::None);
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
        if vk != VK_SPACE {
            self.swallow_space.set(false);
        }
        // The right-click menu takes Enter / Space / Esc; any other key
        // closes it and then does its normal job.
        if self.context.is_open() {
            match vk {
                VK_RETURN => return self.activate_context(),
                VK_SPACE => {
                    // Its WM_CHAR must not type a space.
                    self.swallow_space.set(true);
                    return self.activate_context();
                }
                VK_ESCAPE => {
                    self.close_context()?;
                    return Ok(Action::None);
                }
                _ => self.close_context()?,
            }
        }
        if vk == VK_ESCAPE {
            // Esc closes the innermost thing: strip, then text, then menu.
            if self.strip.is_open() {
                // Back to where the strip was opened from: the power button
                // when using the keyboard, else the search box.
                let back = if self.keyboard_nav.get() {
                    self.strip_home()
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
            // The sidebar's Apps view: back to the pinned apps first.
            if self.apps_view.get() {
                return self.go_to_apps();
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
            Focus::Media(part) => self.media_key(part, vk, shift),
            Focus::Profile | Focus::Settings | Focus::Apps | Focus::Power | Focus::Quick(_) => {
                self.footer_key(focus, vk, shift)
            }
            Focus::Strip(i) => self.strip_key(i, vk, shift),
        }
    }

    fn search_key(&self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool) -> Result<Action> {
        const KEY_A: VIRTUAL_KEY = VIRTUAL_KEY(b'A' as u16);
        const KEY_C: VIRTUAL_KEY = VIRTUAL_KEY(b'C' as u16);
        const KEY_V: VIRTUAL_KEY = VIRTUAL_KEY(b'V' as u16);
        const KEY_X: VIRTUAL_KEY = VIRTUAL_KEY(b'X' as u16);

        // Music shortcuts, only while nothing is typed: Space = play/pause,
        // Ctrl+Left / Ctrl+Right = previous / next.
        if self.music_shown() && self.search.editor.borrow().is_empty() {
            let part = match vk {
                VK_SPACE => Some(CardPart::PlayPause),
                VK_LEFT if ctrl => Some(CardPart::Previous),
                VK_RIGHT if ctrl => Some(CardPart::Next),
                _ => None,
            };
            if let Some(part) = part {
                // The space must not also be typed into the search box.
                self.swallow_space.set(vk == VK_SPACE);
                return Ok(self.press_card(part));
            }
        }

        // With results showing: Up/Down move the highlight (wrapping
        // around), Enter launches it, Tab goes to the footer.
        if self.is_list() {
            match vk {
                VK_DOWN => return self.move_selection(1).map(|()| Action::None),
                VK_UP => return self.move_selection(-1).map(|()| Action::None),
                VK_RETURN => return Ok(self.launch_result(self.selected.get())),
                VK_TAB => {
                    self.keyboard_nav.set(true);
                    // The list layout's pinned strip comes next.
                    let next = if shift {
                        self.footer_last()
                    } else if self.grid_shown() && self.grid.len() > 0 {
                        Focus::Cell(0)
                    } else {
                        self.footer_first()
                    };
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
                    self.below_grid()
                };
                return self.set_focus(next).map(|()| Action::None);
            }
            VK_TAB => {
                self.keyboard_nav.set(true);
                return self.set_focus(self.footer_last()).map(|()| Action::None);
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
            VK_DOWN => self.below_grid(),
            VK_HOME => Focus::Cell(0),
            VK_END => Focus::Cell(n - 1),
            VK_TAB if shift && i == 0 => Focus::Search,
            VK_TAB if shift => Focus::Cell(i - 1),
            VK_TAB if i + 1 < n => Focus::Cell(i + 1),
            VK_TAB => self.below_grid(),
            VK_RETURN => return Ok(self.launch_pin(i)),
            _ => return Ok(Action::None),
        };
        self.set_focus(next)?;
        Ok(Action::None)
    }

    fn footer_key(&self, focus: Focus, vk: VIRTUAL_KEY, shift: bool) -> Result<Action> {
        let n = self.grid.len();
        // Above the footer: the music card's last button, the last pin, or
        // the search box (no pins, or search results showing).
        // (The list layout's music row is part of the footer itself.)
        let card_last = self
            .music
            .focus_order()
            .last()
            .copied()
            .filter(|_| self.music_shown() && self.music.has_seek_bar());
        let above = if let Some(part) = card_last {
            Focus::Media(part)
        } else if n > 0 && self.grid_shown() {
            Focus::Cell(n - 1)
        } else {
            Focus::Search
        };
        let next = match (focus, vk) {
            (Focus::Profile, VK_RETURN) => return Ok(Action::OpenAccountSettings),
            (Focus::Settings, VK_RETURN) => return Ok(Action::OpenSettings),
            (Focus::Apps, VK_RETURN) => return self.go_to_apps(),
            (Focus::Power, VK_RETURN) => {
                self.keyboard_nav.set(true);
                self.open_strip()?;
                return Ok(Action::None);
            }
            (Focus::Quick(i), VK_RETURN) => {
                self.keyboard_nav.set(true);
                return self.press_quick(i);
            }
            // The sidebar's rail runs top to bottom: Up/Down move along
            // it, Right goes to the search box.
            (_, VK_RIGHT) if self.kind() == LayoutKind::Sidebar => Focus::Search,
            (_, VK_UP) if self.kind() != LayoutKind::Sidebar => above,
            // Left to right: profile, gear, then power (or the separate
            // buttons); Tab past the last one wraps to the search box.
            _ => {
                let order = self.footer_order();
                let i = order.iter().position(|&f| f == focus).unwrap_or(0);
                let rail = self.kind() == LayoutKind::Sidebar;
                match vk {
                    VK_LEFT if !rail => order[i.saturating_sub(1)],
                    VK_RIGHT => order[(i + 1).min(order.len() - 1)],
                    VK_UP => order[i.saturating_sub(1)],
                    VK_DOWN if rail => order[(i + 1).min(order.len() - 1)],
                    VK_TAB if shift => i.checked_sub(1).map_or(above, |j| order[j]),
                    VK_TAB => order.get(i + 1).copied().unwrap_or(Focus::Search),
                    _ => return Ok(Action::None),
                }
            }
        };
        self.set_focus(next)?;
        Ok(Action::None)
    }

    /// The footer's focus stops, left to right (the sidebar's rail: top
    /// to bottom). Parts the layout doesn't show are left out.
    fn footer_order(&self) -> Vec<Focus> {
        let mut order = Vec::new();
        if self.music_shown() && !self.music.has_seek_bar() {
            // The list layout's music row, in the bottom bar.
            order.extend(self.music.focus_order().into_iter().map(Focus::Media));
        }
        if self.footer.profile_rect().w > 0.0 {
            order.push(Focus::Profile);
        }
        if self.footer.apps_rect().w > 0.0 {
            order.push(Focus::Apps);
        }
        order.push(Focus::Settings);
        let quick = self.footer.quick_rects().len();
        order.extend((0..quick).map(Focus::Quick));
        if !self.layout.borrow().separate_power_buttons() {
            order.push(Focus::Power);
        }
        order
    }

    /// The footer's first focus stop (below the grid / the music card).
    fn footer_first(&self) -> Focus {
        self.footer_order()
            .first()
            .copied()
            .unwrap_or(Focus::Settings)
    }

    /// The rail's Apps button: switches between the most-used apps and the
    /// pinned grid, with the keyboard in the (emptied) search box.
    fn go_to_apps(&self) -> Result<Action> {
        // Shows the most-used apps, or (pressed again) the pinned apps.
        self.apps_view.set(!self.apps_view.get());
        if !self.search.editor.borrow().is_empty() {
            self.search.editor.borrow_mut().clear();
            self.search.redraw_text(&self.gfx)?;
        }
        self.update_search(true)?;
        self.keyboard_nav.set(false);
        self.set_focus(Focus::Search)?;
        Ok(Action::None)
    }

    /// The footer's last focus stop (Shift+Tab from the search box).
    fn footer_last(&self) -> Focus {
        self.footer_order().last().copied().unwrap_or(Focus::Power)
    }

    /// Where the keyboard goes back to when the strip closes: the button
    /// that opened it.
    fn strip_home(&self) -> Focus {
        match self.strip.state() {
            StripState::Confirm(action) if self.layout.borrow().separate_power_buttons() => {
                footer::QUICK_ACTIONS
                    .iter()
                    .position(|&a| a == action)
                    .map_or(Focus::Search, Focus::Quick)
            }
            _ => Focus::Power,
        }
    }

    /// A separate footer button (Lock / Restart / Shut down) was pressed.
    /// Restart and Shut down ask first, in the strip.
    fn press_quick(&self, i: usize) -> Result<Action> {
        let Some(&action) = footer::QUICK_ACTIONS.get(i) else {
            return Ok(Action::None);
        };
        if !action.needs_confirm() {
            return Ok(Action::Power(action));
        }
        self.strip.open_confirm(
            &self.gfx,
            &self.animator.borrow(),
            action,
            self.reduced_motion.get(),
        )?;
        self.hovered.set(Target::Nothing);
        // Focus starts on Cancel.
        self.set_focus(Focus::Strip(0))?;
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
        // This space already played or paused the music.
        if unit == u16::from(b' ') && self.swallow_space.replace(false) {
            return Ok(());
        }
        // Control characters (Backspace, Tab, Enter, Esc, Ctrl+letters...)
        // are handled as keys in `key_down`.
        let is_text = unit >= 0x20 && unit != 0x7F;
        if !is_text {
            return Ok(());
        }
        self.close_context()?;
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
            self.animator.borrow().blink(&self.search.caret_visual()?)?;
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
        if self.seek_drag.get() {
            return self.music.drag_to(&self.gfx, p.X);
        }
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
        if self.context.is_open() && !matches!(target, Target::Context | Target::ContextPanel) {
            // A click outside the right-click menu only closes it.
            self.close_context()?;
            self.pressed.set(Target::Nothing);
            return Ok(Action::None);
        }
        if self.strip.is_open() && !matches!(target, Target::Strip(_) | Target::StripPanel) {
            // A click outside the strip only closes the strip.
            self.close_strip(Focus::Search)?;
            self.pressed.set(Target::Nothing);
            return Ok(Action::None);
        }
        self.pressed.set(target);
        if target == Target::Media(CardPart::Seek) {
            // Grab the seek bar: it follows the mouse until released.
            self.seek_drag.set(true);
            self.music.drag_to(&self.gfx, p.X)?;
            // SAFETY: captures the mouse for our own window while dragging.
            unsafe { SetCapture(self.hwnd) };
            self.update_focus_visuals()?;
        } else if target == Target::Search {
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
        if self.seek_drag.replace(false) {
            // SAFETY: releases the capture taken in `mouse_down`.
            let _ = unsafe { ReleaseCapture() };
            // Seek where it was let go (wherever the mouse is now).
            let seek = self.music.end_drag();
            self.sync_seek()?;
            return Ok(seek.map_or(Action::None, |t| Action::Media(MediaCommand::Seek(t))));
        }
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
            Target::Media(part) => Ok(self.press_card(part)),
            Target::Profile => Ok(Action::OpenAccountSettings),
            Target::Settings => Ok(Action::OpenSettings),
            Target::Apps => self.go_to_apps(),
            Target::Power if self.strip.is_open() => {
                self.close_strip(Focus::Search)?;
                Ok(Action::None)
            }
            Target::Power => {
                self.open_strip()?;
                Ok(Action::None)
            }
            Target::Quick(i) => self.press_quick(i),
            Target::Strip(i) => self.press_strip(i),
            Target::Context => self.activate_context(),
            _ => Ok(Action::None),
        }
    }
}
