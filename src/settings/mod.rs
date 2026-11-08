//! The Settings window (milestone 8).
//!
//! A normal app window (title bar, taskbar button, Alt+Tab) drawn with the
//! same stack as the menu: Windows.UI.Composition visuals, painted with
//! Direct2D / DirectWrite. It shares the menu's compositor and graphics
//! device (one GPU device for the whole app). Everything else, its visuals,
//! surfaces and fonts, is created when it opens and dropped when it closes,
//! so a closed Settings window costs nothing.
//!
//! ```text
//! root (scaled by the DPI)
//! ├── background (the menu's colors, no blur; see ui/background.rs)
//! ├── left panel fill, selected-section highlight, hover highlight
//! ├── section list (one painted canvas)
//! ├── page: title + one painted canvas per row
//! └── keyboard focus ring
//! ```
//!
//! Like the menu, input methods never run anything that could start a
//! message loop (the color picker, ShellExecute...). They return an
//! [`Action`] and main.rs carries it out.

pub mod page;

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Controls::Dialogs::{CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_DELETE,
    VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE,
    VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClientRect, GetWindowRect, IDC_ARROW, LoadCursorW,
    MINMAXINFO, RegisterClassW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
    WM_APP, WNDCLASSW, WNDPROC, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};
use windows::core::{Interface, Result, w};
use windows_numerics::{Vector2, Vector3};

use crate::config::{Accent, Config, Hotkey, Preset, Rgb, ThemeId};
use crate::ui::background::{Background, SystemEffects};
use crate::ui::gfx::{self, Canvas, Gfx};
use crate::ui::layout::Rect;
use crate::ui::shape::RoundRect;
use page::{ColorTarget, Fonts, Item, Kind, Section, StopPart, View};

/// Posted to the menu window: open Settings (the tray and the footer gear
/// use it, and it lets tests open Settings).
pub const WM_APP_OPEN_SETTINGS: u32 = WM_APP + 14;

const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const HOVER_ALPHA: f32 = 0.08;
const SIDE_FILL_ALPHA: f32 = 0.22;
const RING_WIDTH: f32 = 2.0;

/// Something main.rs should do after an input event.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Action {
    #[default]
    None,
    /// A setting changed: apply it to the menu now, save it soon.
    Apply(Box<Config>),
    /// Open the Windows color picker; hand the result to `color_picked`.
    PickColor(ColorTarget),
    /// Play the menu's open animation again.
    Replay,
    /// The hotkey field started (`true`) or stopped waiting for keys:
    /// the current hotkey is switched off meanwhile, so pressing it
    /// records it instead of toggling the menu.
    Recording(bool),
    OpenConfigFile,
    OpenUrl(&'static str),
    /// Esc: close the Settings window.
    Close,
}

/// A place you can point at or focus: a section in the left list, or
/// part `p` of row `r`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    Side(usize),
    Part(usize, usize),
}

struct Row {
    item: Item,
    rect: Rect,
    canvas: Canvas,
}

/// Errors while painting are logged; the window keeps working.
fn logged<T: Default>(what: &str, r: Result<T>) -> T {
    r.unwrap_or_else(|e| {
        log_error!("settings: {what} failed: {e}");
        T::default()
    })
}

pub struct SettingsWindow {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    compositor: Compositor,
    root: ContainerVisual,
    background: Background,
    side_fill: SpriteVisual,
    side_selected: RoundRect,
    hover: RoundRect,
    ring: RoundRect,
    sidebar: Canvas,
    page_layer: ContainerVisual,
    title: Canvas,
    rows: RefCell<Vec<Row>>,
    fonts: Fonts,
    section: Cell<Section>,
    config: RefCell<Config>,
    accent: Cell<Rgb>,
    /// DPI scale of the monitor the window is on.
    scale: Cell<f32>,
    /// Client area in DIPs.
    size: Cell<Vector2>,
    focus: Cell<Spot>,
    /// The focus ring shows only after keyboard use (like the menu).
    keyboard_nav: Cell<bool>,
    hovered: Cell<Option<Spot>>,
    pressed: Cell<Option<Spot>>,
    /// The slider (row index) being dragged.
    dragging: Cell<Option<usize>>,
    recording: Cell<bool>,
    hotkey_note: RefCell<Option<String>>,
    confirm_reset: Cell<bool>,
    ram: RefCell<String>,
    tracking_mouse: Cell<bool>,
}

static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

fn register_class(wndproc: WNDPROC) -> Result<()> {
    if CLASS_REGISTERED.load(Ordering::Relaxed) {
        return Ok(());
    }
    // SAFETY: registers a window class with valid, static arguments.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: instance.into(),
            lpszClassName: w!("GlintSettings"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Glint's own icon (shared, never destroyed), as for the tray.
            hIcon: crate::window::app_icon(false),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
    }
    CLASS_REGISTERED.store(true, Ordering::Relaxed);
    Ok(())
}

/// The window's DPI scale (1.0 = 100%).
fn dpi_scale(hwnd: HWND) -> f32 {
    // SAFETY: a plain query on our own window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

/// The outer window size for a client area of `client` DIPs at `dpi`.
fn outer_size(client: Vector2, dpi: u32) -> (i32, i32) {
    let scale = dpi as f32 / 96.0;
    let mut r = RECT {
        left: 0,
        top: 0,
        right: (client.X * scale).round() as i32,
        bottom: (client.Y * scale).round() as i32,
    };
    // SAFETY: valid in/out pointer to a local rect.
    let _ = unsafe {
        AdjustWindowRectExForDpi(
            &mut r,
            WS_OVERLAPPEDWINDOW,
            false,
            WS_EX_NOREDIRECTIONBITMAP,
            dpi,
        )
    };
    (r.right - r.left, r.bottom - r.top)
}

/// Where Settings opens: on the cursor's monitor, near the taskbar, with
/// room beside it for the live preview of a `menu_width` DIPs wide menu.
/// The pair is centered on the screen.
pub fn initial_rect(menu_width: f32) -> RECT {
    let (_, work, scale) = crate::window::cursor_monitor();
    let dpi = (scale * 96.0).round() as u32;
    let (w, h) = outer_size(page::DEFAULT_SIZE, dpi);
    let gap = (24.0 * scale) as i32;
    let menu = ((menu_width + 2.0 * crate::window::MARGIN) * scale) as i32;
    let x = work.left + (work.right - work.left - (w + gap + menu)) / 2;
    let x = x.clamp(work.left + gap, (work.right - w).max(work.left));
    let y = (work.bottom - gap - h).max(work.top);
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// WM_GETMINMAXINFO: the window can't be made smaller than every page needs.
pub fn min_max_info(hwnd: HWND, lparam: LPARAM) {
    // SAFETY: a plain query on our own window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let (w, h) = outer_size(page::MIN_SIZE, dpi);
    // SAFETY: for WM_GETMINMAXINFO, lParam points to a MINMAXINFO that
    // Windows owns for the duration of the message.
    if let Some(info) = unsafe { (lparam.0 as *mut MINMAXINFO).as_mut() } {
        info.ptMinTrackSize = POINT { x: w, y: h };
    }
}

/// The Windows color picker, starting at `initial`. It runs its own
/// message loop until closed. `None` = cancelled.
pub fn choose_color(owner: HWND, initial: Rgb) -> Option<Rgb> {
    thread_local! {
        /// The 16 "custom colors" boxes, remembered while Glint runs.
        static CUSTOM: Cell<[COLORREF; 16]> = const { Cell::new([COLORREF(0x00ff_ffff); 16]) };
    }
    let mut custom = CUSTOM.with(Cell::get);
    let to_ref = |c: Rgb| COLORREF(u32::from(c.r) | u32::from(c.g) << 8 | u32::from(c.b) << 16);
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: to_ref(initial),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    // SAFETY: `cc` is fully initialized; `custom` (16 COLORREFs, as the
    // API requires) outlives the call.
    let ok = unsafe { ChooseColorW(&mut cc) }.as_bool();
    CUSTOM.with(|c| c.set(custom));
    ok.then(|| {
        let v = cc.rgbResult.0;
        Rgb::new(v as u8, (v >> 8) as u8, (v >> 16) as u8)
    })
}

impl SettingsWindow {
    /// Creates the window (still hidden; call `show`) at `at` (screen
    /// pixels, outer size).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        wndproc: WNDPROC,
        at: RECT,
        compositor: &Compositor,
        gfx: &Gfx,
        config: &Config,
        accent: Rgb,
        effects: SystemEffects,
    ) -> Result<Self> {
        register_class(wndproc)?;
        // SAFETY: creates a window of our registered class; the strings are
        // 'static.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("GlintSettings"),
                w!("Glint Settings"),
                WS_OVERLAPPEDWINDOW,
                at.left,
                at.top,
                at.right - at.left,
                at.bottom - at.top,
                None,
                None,
                Some(GetModuleHandleW(None)?.into()),
                None,
            )?
        };
        // From here on, a failure must not leave the window behind.
        Self::build(hwnd, compositor, gfx, config, accent, effects).inspect_err(|_| {
            // SAFETY: destroys the window we just created.
            let _ = unsafe { DestroyWindow(hwnd) };
        })
    }

    fn build(
        hwnd: HWND,
        c: &Compositor,
        gfx: &Gfx,
        config: &Config,
        accent: Rgb,
        effects: SystemEffects,
    ) -> Result<Self> {
        dark_title_bar(hwnd);
        let scale = dpi_scale(hwnd);
        let size = client_dips(hwnd, scale);

        let interop: ICompositorDesktopInterop = c.cast()?;
        // SAFETY: `hwnd` is our live window, created on this thread.
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false)? };
        let root = c.CreateContainerVisual()?;
        root.SetScale(Vector3 {
            X: scale,
            Y: scale,
            Z: 1.0,
        })?;
        target.SetRoot(&root)?;
        let children = root.Children()?;

        let bg_layer = c.CreateContainerVisual()?;
        children.InsertAtTop(&bg_layer)?;
        // A tiny radius: the window's own corners are DWM's.
        let background = Background::new(c, &bg_layer, size, 1.0)?;
        background.border_visual().SetIsVisible(false)?;

        let side_fill = c.CreateSpriteVisual()?;
        side_fill.SetBrush(
            &c.CreateColorBrushWithColor(gfx::color(Rgb::new(0, 0, 0), SIDE_FILL_ALPHA))?,
        )?;
        side_fill.SetSize(Vector2 {
            X: page::SIDEBAR_W,
            Y: size.Y,
        })?;
        children.InsertAtTop(&side_fill)?;

        let side_selected = RoundRect::new(c, page::BUTTON_RADIUS, 0.0)?;
        side_selected.set_fill(gfx::color(Rgb::new(255, 255, 255), 0.10))?;
        children.InsertAtTop(&side_selected.visual)?;
        let hover = RoundRect::new(c, page::BUTTON_RADIUS, 0.0)?;
        hover.set_fill(gfx::color(Rgb::new(255, 255, 255), HOVER_ALPHA))?;
        hover.show(false)?;
        children.InsertAtTop(&hover.visual)?;

        let sidebar = Canvas::with_scale(
            c,
            gfx,
            Vector2 {
                X: page::SIDEBAR_W,
                Y: size.Y,
            },
            Some(scale),
        )?;
        children.InsertAtTop(&sidebar.visual)?;
        let page_layer = c.CreateContainerVisual()?;
        children.InsertAtTop(&page_layer)?;
        let title_rect = page::title_rect(size.X);
        let title = Canvas::with_scale(c, gfx, title_rect.size(), Some(scale))?;
        children.InsertAtTop(&title.visual)?;
        let ring = RoundRect::new(c, page::BUTTON_RADIUS, RING_WIDTH)?;
        ring.set_stroke(gfx::color(accent, 1.0))?;
        ring.show(false)?;
        children.InsertAtTop(&ring.visual)?;

        let window = Self {
            hwnd,
            _target: target,
            compositor: c.clone(),
            root,
            background,
            side_fill,
            side_selected,
            hover,
            ring,
            sidebar,
            page_layer,
            title,
            rows: RefCell::new(Vec::new()),
            fonts: Fonts::new(gfx)?,
            section: Cell::new(Section::Appearance),
            config: RefCell::new(config.clone()),
            accent: Cell::new(accent),
            scale: Cell::new(scale),
            size: Cell::new(size),
            focus: Cell::new(Spot::Side(Section::Appearance.index())),
            keyboard_nav: Cell::new(false),
            hovered: Cell::new(None),
            pressed: Cell::new(None),
            dragging: Cell::new(None),
            recording: Cell::new(false),
            hotkey_note: RefCell::new(None),
            confirm_reset: Cell::new(false),
            ram: RefCell::new(String::new()),
            tracking_mouse: Cell::new(false),
        };
        window.apply_background(config, effects);
        window.rebuild(gfx)?;
        Ok(window)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Where the window is on screen (pixels), for placing the preview.
    pub fn screen_rect(&self) -> RECT {
        let mut r = RECT::default();
        // SAFETY: valid out-pointer; our own window.
        let _ = unsafe { GetWindowRect(self.hwnd, &mut r) };
        r
    }

    // -----------------------------------------------------------------------
    // Things main.rs tells us
    // -----------------------------------------------------------------------

    /// config.toml was edited by hand (or reloaded): show its values.
    pub fn sync(&self, gfx: &Gfx, config: &Config) {
        if page::same_for_settings(&self.config.borrow(), config) {
            return;
        }
        *self.config.borrow_mut() = config.clone();
        logged("repaint", self.paint_page(gfx));
    }

    /// The theme changed: every text color and the font may differ, so the
    /// whole window is painted again.
    pub fn theme_changed(&self, gfx: &Gfx) {
        logged("repaint", self.paint_all(gfx));
    }

    /// The accent color in use changed (config, or the Windows accent).
    pub fn set_accent(&self, gfx: &Gfx, accent: Rgb) {
        if self.accent.replace(accent) == accent {
            return;
        }
        logged("accent", self.ring.set_stroke(gfx::color(accent, 1.0)));
        logged("repaint", self.paint_all(gfx));
    }

    /// The menu's colors, without the blur (lighter on the GPU, and the
    /// window is opaque anyway).
    pub fn apply_background(&self, config: &Config, effects: SystemEffects) {
        let effects = SystemEffects {
            transparency: false,
            ..effects
        };
        logged(
            "background",
            self.background
                .apply(&config.background, config.theme, effects, false)
                .map(|_| ()),
        );
    }

    /// WM_SIZE: lay the page out for the new size.
    pub fn resized(&self, gfx: &Gfx) {
        let size = client_dips(self.hwnd, self.scale.get());
        // Minimized: keep everything as it is.
        if size.X < 1.0 || size.Y < 1.0 || size == self.size.get() {
            return;
        }
        self.size.set(size);
        logged("resize", self.rebuild(gfx));
    }

    /// WM_DPICHANGED: paint everything again at the new scale (the window
    /// is resized right after, by main.rs).
    pub fn set_dpi(&self, gfx: &Gfx, dpi: u32) {
        let scale = dpi.max(96) as f32 / 96.0;
        self.scale.set(scale);
        self.size.set(client_dips(self.hwnd, scale));
        let result = (|| {
            self.root.SetScale(Vector3 {
                X: scale,
                Y: scale,
                Z: 1.0,
            })?;
            self.sidebar.set_own_scale(gfx, scale)?;
            self.title.set_own_scale(gfx, scale)?;
            self.rebuild(gfx)
        })();
        logged("DPI change", result);
    }

    /// The GPU device was replaced: every surface is empty.
    pub fn device_replaced(&self, gfx: &Gfx) {
        logged("repaint", self.paint_all(gfx));
    }

    /// The color picker closed with a color.
    pub fn color_picked(&self, gfx: &Gfx, target: ColorTarget, color: Rgb) -> Action {
        self.change(gfx, |c| match target {
            ColorTarget::Stop(i) => {
                let colors = &mut c.background.colors;
                while colors.len() <= i.min(2) {
                    let last = colors.last().copied().unwrap_or(color);
                    colors.push(last);
                }
                if let Some(slot) = colors.get_mut(i) {
                    *slot = color;
                }
                c.background.preset = Preset::Custom;
            }
            ColorTarget::Accent => c.accent = Accent::Color(color),
        })
    }

    /// The color the picker should start at.
    pub fn current_color(&self, target: ColorTarget) -> Rgb {
        match target {
            ColorTarget::Stop(i) => {
                let c = self.config.borrow();
                c.background
                    .colors
                    .get(i)
                    .or(c.background.colors.last())
                    .copied()
                    .unwrap_or(crate::config::DEFAULT_ACCENT)
            }
            ColorTarget::Accent => self.accent.get(),
        }
    }

    /// A line under the hotkey field (e.g. "already used"), or the hint.
    pub fn set_hotkey_note(&self, gfx: &Gfx, note: Option<String>) {
        *self.hotkey_note.borrow_mut() = note;
        logged("repaint", self.paint_rows(gfx, Some(Item::Hotkey)));
    }

    /// While recording a hotkey, Alt combinations must reach `key_down`
    /// instead of opening the window menu.
    pub fn is_recording(&self) -> bool {
        self.recording.get()
    }

    // -----------------------------------------------------------------------
    // Building and painting
    // -----------------------------------------------------------------------

    fn view<R>(&self, f: impl FnOnce(&View) -> R) -> R {
        let config = self.config.borrow();
        let note = self.hotkey_note.borrow();
        let ram = self.ram.borrow();
        f(&View {
            config: &config,
            accent: self.accent.get(),
            recording: self.recording.get(),
            hotkey_note: note.as_deref(),
            confirm_reset: self.confirm_reset.get(),
            ram: &ram,
        })
    }

    /// Makes the rows of the open section (dropping the old ones), sizes
    /// everything for the window, and paints it all.
    fn rebuild(&self, gfx: &Gfx) -> Result<()> {
        let size = self.size.get();
        self.background.set_size(size)?;
        self.side_fill.SetSize(Vector2 {
            X: page::SIDEBAR_W,
            Y: size.Y,
        })?;
        self.sidebar.resize(
            gfx,
            Vector2 {
                X: page::SIDEBAR_W,
                Y: size.Y,
            },
        )?;
        let title = page::title_rect(size.X);
        self.title.resize(gfx, title.size())?;
        self.title.set_offset(gfx, title.x, title.y)?;

        let children = self.page_layer.Children()?;
        children.RemoveAll()?;
        let scale = Some(self.scale.get());
        let mut rows = Vec::new();
        for (item, rect) in page::rows(self.section.get(), size.X) {
            let canvas = Canvas::with_scale(&self.compositor, gfx, rect.size(), scale)?;
            canvas.set_offset(gfx, rect.x, rect.y)?;
            children.InsertAtTop(&canvas.visual)?;
            rows.push(Row { item, rect, canvas });
        }
        *self.rows.borrow_mut() = rows;
        self.hovered.set(None);
        self.hover.show(false)?;
        self.paint_all(gfx)
    }

    fn paint_all(&self, gfx: &Gfx) -> Result<()> {
        let section = self.section.get();
        self.sidebar.draw(gfx, |ctx| {
            page::paint_sidebar(ctx, gfx, &self.fonts, section, self.accent.get())
        })?;
        let size = self.title.size();
        self.title.draw(gfx, |ctx| {
            page::paint_title(ctx, gfx, &self.fonts, section, size)
        })?;
        self.paint_page(gfx)
    }

    fn paint_page(&self, gfx: &Gfx) -> Result<()> {
        self.paint_rows(gfx, None)
    }

    /// Paints every row (`None`) or only the row showing `only`.
    fn paint_rows(&self, gfx: &Gfx, only: Option<Item>) -> Result<()> {
        for row in self.rows.borrow().iter() {
            if only.is_some_and(|item| item != row.item) {
                continue;
            }
            self.view(|v| {
                row.canvas.draw(gfx, |ctx| {
                    page::paint_row(ctx, gfx, &self.fonts, row.item, row.rect, v)
                })
            })?;
        }
        self.update_focus_visuals()
    }

    // -----------------------------------------------------------------------
    // Where things are
    // -----------------------------------------------------------------------

    fn row_item(&self, r: usize) -> Option<Item> {
        self.rows.borrow().get(r).map(|row| row.item)
    }

    fn row_of(&self, item: Item) -> Option<usize> {
        self.rows.borrow().iter().position(|row| row.item == item)
    }

    fn parts(&self, r: usize) -> Vec<Rect> {
        let rows = self.rows.borrow();
        let Some(row) = rows.get(r) else {
            return Vec::new();
        };
        self.view(|v| page::parts(row.item, row.rect, v))
    }

    fn spot_rect(&self, spot: Spot) -> Option<(Rect, f32)> {
        match spot {
            Spot::Side(i) => Some((page::side_item(i), page::BUTTON_RADIUS)),
            Spot::Part(r, p) => {
                let rect = *self.parts(r).get(p)?;
                let radius = if self.row_item(r) == Some(Item::ThemeCard) {
                    12.0
                } else {
                    page::BUTTON_RADIUS
                };
                Some((rect, radius))
            }
        }
    }

    /// Every Tab stop, in order: the section list, then each control.
    fn stops(&self) -> Vec<Spot> {
        let mut stops = vec![Spot::Side(self.section.get().index())];
        let items: Vec<Item> = self.rows.borrow().iter().map(|r| r.item).collect();
        for (r, item) in items.into_iter().enumerate() {
            match item.kind() {
                Kind::Radio => stops.push(Spot::Part(r, self.chosen(item))),
                Kind::Slider | Kind::Toggle => stops.push(Spot::Part(r, 0)),
                Kind::Buttons => stops.extend((0..self.parts(r).len()).map(|p| Spot::Part(r, p))),
                Kind::Text => {}
            }
        }
        stops
    }

    fn chosen(&self, item: Item) -> usize {
        page::chosen(item, &self.config.borrow())
    }

    fn hit(&self, p: Vector2) -> Option<Spot> {
        if p.X < page::SIDEBAR_W {
            return (0..Section::ALL.len())
                .find(|&i| page::side_item(i).contains(p))
                .map(Spot::Side);
        }
        let n = self.rows.borrow().len();
        (0..n).find_map(|r| {
            self.parts(r)
                .iter()
                .position(|part| part.contains(p))
                .map(|i| Spot::Part(r, i))
        })
    }

    fn to_dips(&self, x: i32, y: i32) -> Vector2 {
        let s = self.scale.get();
        Vector2 {
            X: x as f32 / s,
            Y: y as f32 / s,
        }
    }

    /// The section highlight and (after keyboard use) the focus ring.
    fn update_focus_visuals(&self) -> Result<()> {
        self.side_selected
            .set_rect(page::side_item(self.section.get().index()))?;
        // A part that no longer exists (a color stop was removed...).
        if let Spot::Part(r, p) = self.focus.get() {
            let n = self.parts(r).len();
            if n == 0 {
                self.focus.set(Spot::Side(self.section.get().index()));
            } else if p >= n {
                self.focus.set(Spot::Part(r, n - 1));
            }
        }
        match self.spot_rect(self.focus.get()) {
            Some((r, radius)) if self.keyboard_nav.get() => {
                // Just outside the control, so it never hides it.
                let out = Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
                self.ring.set_radius(radius + 3.0);
                self.ring.set_rect(out)?;
                self.ring.show(true)
            }
            _ => self.ring.show(false),
        }
    }

    fn set_hover(&self, spot: Option<Spot>) -> Result<()> {
        if self.hovered.replace(spot) == spot {
            return Ok(());
        }
        // Not on the open section, sliders or plain text.
        let shown = spot.filter(|s| match *s {
            Spot::Side(i) => i != self.section.get().index(),
            Spot::Part(r, _) => self
                .row_item(r)
                .is_some_and(|item| !matches!(item.kind(), Kind::Slider | Kind::Text)),
        });
        match shown.and_then(|s| self.spot_rect(s)) {
            Some((r, radius)) => {
                self.hover.set_radius(radius);
                self.hover.set_rect(r)?;
                self.hover.show(true)
            }
            None => self.hover.show(false),
        }
    }

    // -----------------------------------------------------------------------
    // Changing things
    // -----------------------------------------------------------------------

    /// Changes the config copy with `f`. If anything changed, the page is
    /// repainted and main.rs gets the new config to apply.
    fn change(&self, gfx: &Gfx, f: impl FnOnce(&mut Config)) -> Action {
        let new = {
            let mut c = self.config.borrow_mut();
            let before = c.clone();
            f(&mut c);
            if *c == before {
                return Action::None;
            }
            c.clone()
        };
        // While dragging, only the slider itself changes.
        let only = self.dragging.get().and_then(|r| self.row_item(r));
        logged("repaint", self.paint_rows(gfx, only));
        // A radio row's focus follows its choice.
        if let Spot::Part(r, _) = self.focus.get()
            && let Some(item) = self.row_item(r)
            && item.kind() == Kind::Radio
        {
            self.focus.set(Spot::Part(r, self.chosen(item)));
            logged("focus", self.update_focus_visuals());
        }
        Action::Apply(Box::new(new))
    }

    fn open_section(&self, gfx: &Gfx, section: Section) {
        if self.section.replace(section) == section {
            return;
        }
        self.confirm_reset.set(false);
        if section == Section::About {
            // Read once, when the page opens (not live).
            *self.ram.borrow_mut() = page::ram_text(crate::stress::private_working_set());
        }
        logged("page", self.rebuild(gfx));
    }

    /// Part `p` of row `r` was clicked, or Enter / Space was pressed on it.
    fn activate(&self, gfx: &Gfx, r: usize, p: usize) -> Action {
        let Some(item) = self.row_item(r) else {
            return Action::None;
        };
        match item {
            // A theme brings its own colors: the background and accent go
            // back to "the theme's" (they can be changed again afterwards).
            Item::ThemeCard => match ThemeId::ALL.get(p) {
                Some(&id) => self.change(gfx, |c| {
                    c.theme = id;
                    c.background.preset = Preset::Theme;
                    c.accent = Accent::Theme;
                }),
                None => Action::None,
            },
            Item::IconTint => self.change(gfx, |c| c.icon_tint = !c.icon_tint),
            Item::Position => match page::POSITIONS.get(p) {
                Some(&(position, _)) => self.change(gfx, |c| c.position = position),
                None => Action::None,
            },
            Item::Mode => match page::MODES.get(p) {
                Some(&(mode, _)) => self.change(gfx, |c| c.background.mode = mode),
                None => Action::None,
            },
            Item::Presets => match page::presets().get(p) {
                Some(&preset) => self.change(gfx, |c| c.background.preset = preset),
                None => Action::None,
            },
            Item::CustomColors => {
                let n = self.config.borrow().background.colors.len();
                match page::stop_part(n, p) {
                    Some(StopPart::Color(i)) => Action::PickColor(ColorTarget::Stop(i)),
                    Some(StopPart::Add) => self.change(gfx, |c| {
                        let colors = &mut c.background.colors;
                        if let Some(&last) = colors.last() {
                            colors.push(last);
                        }
                        c.background.preset = Preset::Custom;
                    }),
                    Some(StopPart::Remove) => self.change(gfx, |c| {
                        if c.background.colors.len() > 1 {
                            c.background.colors.pop();
                        }
                        c.background.preset = Preset::Custom;
                    }),
                    None => Action::None,
                }
            }
            Item::AccentColor if p == 0 => Action::PickColor(ColorTarget::Accent),
            Item::AccentColor => {
                // Stop following Windows: keep the color it has right now.
                let shown = self.accent.get();
                self.change(gfx, |c| {
                    c.accent = match c.accent {
                        Accent::System => Accent::Color(shown),
                        Accent::Theme | Accent::Color(_) => Accent::System,
                    }
                })
            }
            Item::ShowNames => self.change(gfx, |c| c.show_names = !c.show_names),
            Item::ShowMusic => self.change(gfx, |c| c.music.enabled = !c.music.enabled),
            Item::AlbumTint => self.change(gfx, |c| {
                c.background.tint_from_album = !c.background.tint_from_album
            }),
            Item::Autostart => self.change(gfx, |c| c.autostart = !c.autostart),
            Item::Replay => Action::Replay,
            Item::Hotkey => {
                self.recording.set(true);
                *self.hotkey_note.borrow_mut() = None;
                logged("repaint", self.paint_rows(gfx, Some(Item::Hotkey)));
                Action::Recording(true)
            }
            Item::OpenConfig => Action::OpenConfigFile,
            Item::Reset if !self.confirm_reset.get() => {
                // "Are you sure?" first; focus starts on Cancel.
                self.confirm_reset.set(true);
                self.focus.set(Spot::Part(r, 0));
                logged("repaint", self.paint_rows(gfx, Some(Item::Reset)));
                Action::None
            }
            Item::Reset => {
                self.confirm_reset.set(false);
                self.focus.set(Spot::Part(r, 0));
                if p == 1 {
                    log_info!("settings: reset to defaults (pins kept)");
                    let action = self.change(gfx, |c| *c = c.reset_keeping());
                    // Repaint the button even when nothing changed.
                    logged("repaint", self.paint_rows(gfx, Some(Item::Reset)));
                    action
                } else {
                    logged("repaint", self.paint_rows(gfx, Some(Item::Reset)));
                    Action::None
                }
            }
            Item::Link => Action::OpenUrl(REPO_URL),
            _ => Action::None,
        }
    }

    /// Moves slider `r` to window x `x` (DIPs).
    fn slide_to(&self, gfx: &Gfx, r: usize, x: f32) -> Action {
        let (Some(item), Some(&part)) = (self.row_item(r), self.parts(r).first()) else {
            return Action::None;
        };
        let t = page::track(part);
        let range = page::range(item);
        let value = range.min + ((x - t.x) / t.w).clamp(0.0, 1.0) * (range.max - range.min);
        self.change(gfx, |c| page::set_slider(item, c, value))
    }

    /// Changes slider `r` by `steps` steps (keyboard), or to an end.
    fn nudge(&self, gfx: &Gfx, r: usize, steps: f32) -> Action {
        let Some(item) = self.row_item(r) else {
            return Action::None;
        };
        let range = page::range(item);
        let now = page::slider_value(item, &self.config.borrow());
        let value = now + steps * range.step;
        self.change(gfx, |c| page::set_slider(item, c, value))
    }

    fn stop_recording(&self, gfx: &Gfx) {
        self.recording.set(false);
        *self.hotkey_note.borrow_mut() = None;
        logged("repaint", self.paint_rows(gfx, Some(Item::Hotkey)));
    }

    // -----------------------------------------------------------------------
    // Mouse (client pixels)
    // -----------------------------------------------------------------------

    pub fn mouse_move(&self, gfx: &Gfx, x: i32, y: i32) -> Action {
        if !self.tracking_mouse.replace(true) {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: valid, correctly sized struct for our own window.
            let _ = unsafe { TrackMouseEvent(&mut tme) };
        }
        let p = self.to_dips(x, y);
        if let Some(r) = self.dragging.get() {
            return self.slide_to(gfx, r, p.X);
        }
        logged("hover", self.set_hover(self.hit(p)));
        Action::None
    }

    pub fn mouse_leave(&self) {
        self.tracking_mouse.set(false);
        logged("hover", self.set_hover(None));
    }

    pub fn mouse_down(&self, gfx: &Gfx, x: i32, y: i32) -> Action {
        self.keyboard_nav.set(false);
        let p = self.to_dips(x, y);
        let hit = self.hit(p);
        let hotkey = self.row_of(Item::Hotkey).map(|r| Spot::Part(r, 0));
        // A click anywhere else stops recording.
        let mut action = Action::None;
        if self.recording.get() && hit != hotkey {
            self.stop_recording(gfx);
            action = Action::Recording(false);
        }
        match hit {
            Some(Spot::Side(i)) => {
                self.open_section(gfx, Section::ALL[i]);
                self.focus.set(Spot::Side(i));
            }
            Some(Spot::Part(r, _))
                if self.row_item(r).is_some_and(|i| i.kind() == Kind::Slider) =>
            {
                self.focus.set(Spot::Part(r, 0));
                self.dragging.set(Some(r));
                // SAFETY: captures the mouse for our own window while dragging.
                unsafe { SetCapture(self.hwnd) };
                if action == Action::None {
                    action = self.slide_to(gfx, r, p.X);
                }
            }
            Some(spot) => self.pressed.set(Some(spot)),
            None => {}
        }
        logged("focus", self.update_focus_visuals());
        action
    }

    pub fn mouse_up(&self, gfx: &Gfx, x: i32, y: i32) -> Action {
        if self.dragging.take().is_some() {
            // SAFETY: releases the capture taken in `mouse_down`.
            let _ = unsafe { ReleaseCapture() };
            return Action::None;
        }
        let pressed = self.pressed.take();
        let hit = self.hit(self.to_dips(x, y));
        match pressed {
            Some(Spot::Part(r, p)) if hit == pressed => {
                self.focus.set(Spot::Part(r, p));
                let action = self.activate(gfx, r, p);
                logged("focus", self.update_focus_visuals());
                action
            }
            _ => Action::None,
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    /// WM_KEYDOWN / WM_SYSKEYDOWN.
    pub fn key_down(&self, gfx: &Gfx, vk: u16, ctrl: bool, alt: bool, shift: bool) -> Action {
        if self.recording.get() {
            return self.record_key(gfx, vk, ctrl, alt, shift);
        }
        let key = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        if key == VK_ESCAPE {
            if self.confirm_reset.get() {
                self.confirm_reset.set(false);
                logged("repaint", self.paint_rows(gfx, Some(Item::Reset)));
                return Action::None;
            }
            return Action::Close;
        }
        if matches!(
            key,
            VK_TAB | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT
        ) {
            self.keyboard_nav.set(true);
        }
        let action = match key {
            VK_TAB => {
                self.step_focus(if shift { -1 } else { 1 });
                Action::None
            }
            _ => match self.focus.get() {
                Spot::Side(i) => self.side_key(gfx, i, key),
                Spot::Part(r, p) => self.part_key(gfx, r, p, key),
            },
        };
        logged("focus", self.update_focus_visuals());
        action
    }

    /// Moves focus to the next (`1`) or previous (`-1`) Tab stop.
    fn step_focus(&self, delta: isize) {
        let stops = self.stops();
        let now = self.focus.get();
        let i = stops.iter().position(|&s| s == now).unwrap_or(0) as isize;
        let n = stops.len() as isize;
        if let Some(&next) = stops.get((i + delta).rem_euclid(n) as usize) {
            self.focus.set(next);
        }
    }

    fn side_key(
        &self,
        gfx: &Gfx,
        i: usize,
        key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    ) -> Action {
        let last = Section::ALL.len() - 1;
        let next = match key {
            VK_UP => i.saturating_sub(1),
            VK_DOWN => (i + 1).min(last),
            VK_HOME => 0,
            VK_END => last,
            VK_RIGHT | VK_RETURN | VK_SPACE => {
                // Into the page.
                self.step_focus(1);
                return Action::None;
            }
            _ => return Action::None,
        };
        self.open_section(gfx, Section::ALL[next]);
        self.focus.set(Spot::Side(next));
        Action::None
    }

    fn part_key(
        &self,
        gfx: &Gfx,
        r: usize,
        p: usize,
        key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    ) -> Action {
        let Some(item) = self.row_item(r) else {
            return Action::None;
        };
        match (item.kind(), key) {
            (_, VK_UP) => self.step_focus(-1),
            (_, VK_DOWN) => self.step_focus(1),
            (Kind::Slider, _) => {
                let big = 10.0;
                return match key {
                    VK_LEFT => self.nudge(gfx, r, -1.0),
                    VK_RIGHT => self.nudge(gfx, r, 1.0),
                    VK_PRIOR => self.nudge(gfx, r, big),
                    VK_NEXT => self.nudge(gfx, r, -big),
                    VK_HOME => self.nudge(gfx, r, -1e6),
                    VK_END => self.nudge(gfx, r, 1e6),
                    _ => Action::None,
                };
            }
            (Kind::Radio, VK_LEFT | VK_RIGHT) => {
                let n = self.parts(r).len();
                let next = if key == VK_LEFT {
                    p.saturating_sub(1)
                } else {
                    (p + 1).min(n.saturating_sub(1))
                };
                self.focus.set(Spot::Part(r, next));
                return self.activate(gfx, r, next);
            }
            (_, VK_LEFT) => self.step_focus(-1),
            (_, VK_RIGHT) => self.step_focus(1),
            (_, VK_RETURN | VK_SPACE) => return self.activate(gfx, r, p),
            _ => {}
        }
        Action::None
    }

    /// A key while the hotkey field waits for keys.
    fn record_key(&self, gfx: &Gfx, vk: u16, ctrl: bool, alt: bool, shift: bool) -> Action {
        // Shift, Ctrl, Alt, Win on their own: wait for the main key.
        const MODIFIERS: [u16; 10] = [0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4];
        if MODIFIERS.contains(&vk) || vk == 0xA5 {
            return Action::None;
        }
        let key = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        let plain = !ctrl && !alt && !shift;
        if plain && key == VK_ESCAPE {
            self.stop_recording(gfx);
            return Action::Recording(false);
        }
        let new = if plain && (key == VK_BACK || key == VK_DELETE) {
            None
        } else {
            match Hotkey::from_keys(ctrl, alt, shift, false, vk) {
                Some(k) => Some(k),
                None => {
                    self.set_hotkey_note(
                        gfx,
                        Some("That can't be a hotkey: use Ctrl or Alt with a key.".into()),
                    );
                    return Action::None;
                }
            }
        };
        self.recording.set(false);
        *self.hotkey_note.borrow_mut() = None;
        match self.change(gfx, |c| c.hotkey = new) {
            // The same hotkey again: just switch it back on.
            Action::None => {
                logged("repaint", self.paint_rows(gfx, Some(Item::Hotkey)));
                Action::Recording(false)
            }
            action => action,
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        if self.dragging.get().is_some() {
            // SAFETY: releases a capture we may hold; no pointers.
            let _ = unsafe { ReleaseCapture() };
        }
        // SAFETY: destroys our own window. The visuals and surfaces are
        // released right after, when the fields are dropped.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// The client area in DIPs.
fn client_dips(hwnd: HWND, scale: f32) -> Vector2 {
    let mut r = RECT::default();
    // SAFETY: valid out-pointer; our own window.
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    Vector2 {
        X: (r.right - r.left) as f32 / scale,
        Y: (r.bottom - r.top) as f32 / scale,
    }
}

/// A dark title bar to match the content (Windows 11).
fn dark_title_bar(hwnd: HWND) {
    // DWMWA_USE_IMMERSIVE_DARK_MODE
    const DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
    let on: i32 = 1; // Win32 BOOL
    // SAFETY: passes a pointer to a local 4-byte BOOL with its exact size.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DARK_MODE,
            (&raw const on).cast(),
            std::mem::size_of::<i32>() as u32,
        )
    };
}

/// Shows the Settings window. `activate` = bring it to the front with the
/// keyboard (not during the stress test). Takes the handle, not the window,
/// so nothing is borrowed while Windows sends the activation messages.
pub fn show(hwnd: HWND, activate: bool) {
    // SAFETY: shows one of our own windows.
    unsafe {
        if activate {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}
