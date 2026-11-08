//! Glint: a tiny, smooth Start menu for Windows 11.
//!
//! Startup order: single-instance check → DPI awareness → WinRT + dispatcher
//! queue → config.toml (created on first run) → hidden window → composition
//! tree (and painting the content) → config watcher → tray icon → keyboard
//! hook → hotkey → message loop.

// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod log;
mod apps;
mod autostart;
mod config;
mod config_watch;
mod hook;
mod icons;
mod media;
mod power;
mod search;
mod settings;
mod settings_watch;
mod stress;
mod tray;
mod ui;
mod window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Foundation::TypedEventHandler;
use windows::System::DispatcherQueueController;
use windows::UI::Composition::CompositionScopedBatch;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
    RO_INIT_SINGLETHREADED, RoInitialize,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::Ime::ISC_SHOWUICOMPOSITIONWINDOW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, TME_LEAVE,
    TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey, VK_CONTROL, VK_ESCAPE, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, HTCLIENT,
    IDC_IBEAM, KillTimer, LoadCursorW, MSG, PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE,
    PostMessageW, PostQuitMessage, RegisterWindowMessageW, SC_KEYMENU, SW_SHOWNORMAL,
    SWP_NOACTIVATE, SWP_NOZORDER, SetCursor, SetTimer, TranslateMessage, WA_INACTIVE, WM_ACTIVATE,
    WM_APP, WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION,
    WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_HOTKEY, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_POWERBROADCAST, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR,
    WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_TIMER, WM_WTSSESSION_CHANGE,
    WTS_SESSION_UNLOCK,
};
use windows::core::{Result, w};

use apps::{AppInfo, ChangeWatch, WM_APP_APPS_CHANGED, WM_APP_APPS_LOADED};
use config::{Accent, Config, ConfigError, Rgb};
use config_watch::{ConfigWatch, WM_APP_CONFIG_CHANGED};
use hook::{HookThread, WM_APP_TOGGLE};
use media::{Media, WM_APP_MEDIA, WM_APP_MEDIA_INFO, WM_APP_MEDIA_READY};
use settings_watch::{SettingsWatch, WM_APP_SETTINGS_CHANGED};
use tray::{Tray, TrayCommand, WM_APP_TRAY};
use ui::Action;
use ui::gfx::WM_APP_REDRAW;
use window::Placement;

/// Posted when a close animation finished; wParam = its generation.
const WM_APP_CLOSE_DONE: u32 = WM_APP + 4;
/// Posted when taking the keyboard focus failed right after opening; we try
/// once more when the current message is done.
const WM_APP_FOCUS_RETRY: u32 = WM_APP + 6;
/// Sent when the mouse leaves the window (after `TrackMouseEvent`). Defined
/// here so we don't pull in the whole `Win32_UI_Controls` feature for it.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// A tray click right after the menu closed (because the click took focus
/// away) should not reopen it.
const REOPEN_GUARD_MS: u64 = 400;

/// Everything the UI thread owns. Lives in `APP` for the window procedure.
struct App {
    hwnd: HWND,
    ui: ui::Ui,
    tray: Tray,
    hook: HookThread,
    settings: SettingsWatch,
    config: RefCell<Config>,
    /// The menu is open (or opening). False while closing and when hidden.
    is_open: Cell<bool>,
    /// The window is shown: open, or still playing the close animation.
    is_visible: Cell<bool>,
    placement: Cell<Option<Placement>>,
    /// Bumped on every open and close, so a stale "close finished" from an
    /// interrupted close is ignored.
    generation: Cell<u64>,
    close_batch: RefCell<Option<CompositionScopedBatch>>,
    closed_at: Cell<u64>,
    opened_at: Cell<u64>,
    last_background_log: RefCell<String>,
    msg_taskbar_created: u32,
    /// We asked Windows for a WM_MOUSELEAVE (it sends one per request).
    tracking_mouse: Cell<bool>,
    /// The app list: scanned on a helper thread, delivered through here.
    apps_tx: Sender<Vec<AppInfo>>,
    apps_rx: Receiver<Vec<AppInfo>>,
    /// A scan is running; `rescan` = something changed meanwhile, so scan
    /// once more when it's done.
    scanning: Cell<bool>,
    rescan: Cell<bool>,
    /// Tells us about installs and uninstalls (None if that failed).
    _app_watch: Option<ChangeWatch>,
    /// What is playing (the music card).
    media: Media,
    /// `%APPDATA%\Glint\config.toml` (None if APPDATA isn't set: then the
    /// defaults are used and nothing is saved).
    config_path: Option<PathBuf>,
    /// Tells us when config.toml changes (None if that failed).
    _config_watch: Option<ConfigWatch>,
    /// Launches not written to config.toml yet.
    stats_dirty: Cell<bool>,
    /// The `hotkey` from the config is registered.
    hotkey_on: Cell<bool>,
    /// The accent color in use (resolved, also for `accent = "system"`).
    accent: Cell<Rgb>,
    /// `--stress N` is running (the leak test).
    stress: Option<stress::Stress>,
    /// Trim memory once the menu is hidden again (after a GPU reset, or
    /// after Settings closed).
    trim_after_close: Cell<bool>,
    /// The Settings window, while it is open (milestone 8). While it is,
    /// the menu is its live preview: placed beside it, not topmost, and
    /// it doesn't close when it loses the focus.
    settings_window: RefCell<Option<settings::SettingsWindow>>,
    /// What config.toml holds (as last loaded or saved), so a save from
    /// Settings writes only the lines that changed.
    file_config: RefCell<Config>,
    /// Settings changed something that isn't saved yet.
    settings_dirty: Cell<bool>,
    /// The hotkey is switched off while Settings records a new one.
    hotkey_paused: Cell<bool>,
}

/// WM_TIMER id: an app was installed or removed; wait until things settle.
const TIMER_APPS_SETTLE: usize = 1;
/// Installers create and delete many files; scan only after 2 s of quiet.
const APPS_SETTLE_MS: u32 = 2000;
/// WM_TIMER id: config.toml changed; reload after a short quiet moment.
/// (Ids 2 and 3 are the UI's.)
const TIMER_CONFIG: usize = 4;
/// Editors save in several steps (temp file, rename...): wait this long.
const CONFIG_SETTLE_MS: u32 = 300;
/// WM_TIMER id: write the launch counts to config.toml.
const TIMER_SAVE_STATS: usize = 5;
/// Not on every launch: a few seconds after the last one.
const SAVE_STATS_MS: u32 = 5000;
/// RegisterHotKey id for the `hotkey` setting.
const HOTKEY_ID: i32 = 1;
/// WM_TIMER id: save what Settings changed (300 ms after the last change,
/// so dragging a slider writes the file once). Id 6 is the stress test's.
const TIMER_SETTINGS_SAVE: usize = 7;
const SETTINGS_SAVE_MS: u32 = 300;
/// WM_TIMER id: an animation value changed in Settings; replay the open
/// animation once it stops changing.
const TIMER_REPLAY: usize = 8;

/// The accent color to draw with.
fn resolve_accent(config: &Config, settings: &SettingsWatch) -> Rgb {
    // High contrast: the theme's highlight color, whatever the config says.
    if let Some(hc) = settings_watch::high_contrast() {
        return hc.highlight;
    }
    match config.accent {
        Accent::Color(c) => c,
        Accent::System => settings.accent().unwrap_or(config::DEFAULT_ACCENT),
    }
}

/// The default pins that are installed on this PC (for a new config file).
fn installed_default_pins() -> Vec<String> {
    config::DEFAULT_PINS
        .iter()
        .filter(|aumid| apps::find(aumid).is_some())
        .map(|s| s.to_string())
        .collect()
}

/// Writes a fresh config.toml and returns the settings it holds.
fn write_default_config(path: &std::path::Path) -> Config {
    let pins = installed_default_pins();
    match config::create_default(path, &pins) {
        Ok(()) => log_info!("created {} with the default settings", path.display()),
        Err(e) => log_error!("{e}"),
    }
    Config {
        pinned: pins,
        ..Config::default()
    }
}

/// The settings at startup, and an error to show (and log) once the tray
/// icon exists. A broken file means the defaults until it is fixed.
fn load_startup_config(path: Option<&std::path::Path>) -> (Config, Option<ConfigError>) {
    let Some(path) = path else {
        log_warn!("APPDATA is not set; using the default settings (nothing is saved)");
        return (Config::default(), None);
    };
    match config::load(path) {
        Ok(Some(config)) => (config, None),
        Ok(None) => (write_default_config(path), None),
        Err(e) => (Config::default(), Some(e)),
    }
}

/// Is this key held down right now (as of the current message)?
fn key_held(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    // SAFETY: GetKeyState has no preconditions.
    unsafe { GetKeyState(i32::from(vk.0)) < 0 }
}

/// The x, y packed into a mouse message's lParam (client pixels).
fn mouse_xy(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from((lparam.0 & 0xFFFF) as u16 as i16),
        i32::from(((lparam.0 >> 16) & 0xFFFF) as u16 as i16),
    )
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Gets the app without keeping the RefCell borrowed, so the window
/// procedure can be re-entered safely (Win32 does that a lot).
fn app() -> Option<Rc<App>> {
    APP.with(|a| a.borrow().clone())
}

fn now_ms() -> u64 {
    // SAFETY: GetTickCount64 has no preconditions.
    unsafe { GetTickCount64() }
}

fn log_err(what: &str, result: Result<()>) {
    if let Err(e) = result {
        log_error!("{what} failed: {e}");
    }
}

impl App {
    fn toggle(&self) {
        if self.is_open.get() {
            self.close();
        } else {
            self.open();
        }
    }

    fn reduced_motion(&self) -> bool {
        !self.settings.animations_enabled()
    }

    /// The window fits the tallest the menu can get (8 search results or
    /// the grid), so it never moves while the menu grows or shrinks.
    fn placement(&self) -> Placement {
        self.placement_for(self.ui.max_size())
    }

    /// Where a window for a `size` DIPs menu goes: above the taskbar, or,
    /// while Settings is open, beside the Settings window.
    fn placement_for(&self, size: windows_numerics::Vector2) -> Placement {
        match self.preview_anchor() {
            Some((rect, _)) => window::placement_beside(size.X, size.Y, rect),
            None => window::placement(size.X, size.Y),
        }
    }

    /// Settings is open: its screen rect and window handle.
    fn preview_anchor(&self) -> Option<(windows::Win32::Foundation::RECT, HWND)> {
        self.settings_window
            .borrow()
            .as_ref()
            .map(|s| (s.screen_rect(), s.hwnd()))
    }

    fn in_preview(&self) -> bool {
        self.settings_window.borrow().is_some()
    }

    /// Shows the menu window at `p`: topmost, or just below Settings.
    fn show_window(&self, p: &Placement) {
        match self.preview_anchor() {
            Some((_, settings)) => window::show_below(self.hwnd, p, settings),
            None => window::show(self.hwnd, p),
        }
    }

    fn open(&self) {
        self.open_with(true);
    }

    /// Opens the menu. `focus` = give it the keyboard (not for the
    /// Settings preview, which leaves the keyboard with Settings).
    fn open_with(&self, focus: bool) {
        self.generation.set(self.generation.get() + 1);
        let fresh = !self.is_visible.get();
        if fresh {
            // A GPU reset while hidden is noticed here, before painting.
            self.ui.ensure_device();
            // Fully hidden: empty search and the grid back, then pick the
            // monitor and position fresh. (If we are interrupting a close,
            // stay where we are and just reverse.)
            log_err("reset", self.ui.reset_closed());
            let p = self.placement();
            log_err("layout", self.ui.set_layout(&p));
            // Covers are decoded for this monitor's DPI.
            self.media.set_cover_px(self.ui.cover_px());
            self.show_window(&p);
            self.placement.set(Some(p));
            self.is_visible.set(true);
        }
        self.is_open.set(true);
        self.opened_at.set(now_ms());
        // Fresh playback position (no timeline events arrive while hidden),
        // and timeline events from now on.
        self.media.set_open(true);
        self.push_music();
        log_err(
            "open animation",
            self.ui.animate_open(fresh, self.reduced_motion()),
        );
        // The stress test types through the UI directly and never takes the
        // keyboard away from whatever you are doing meanwhile.
        if focus && self.stress.is_none() && !window::take_focus(self.hwnd) {
            // SAFETY: posts a plain message (no pointers) to our window.
            let _ =
                unsafe { PostMessageW(Some(self.hwnd), WM_APP_FOCUS_RETRY, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Second try at taking the keyboard, a moment after opening.
    fn retry_focus(&self) {
        // SAFETY: GetForegroundWindow has no preconditions.
        if !self.is_open.get() || unsafe { GetForegroundWindow() } == self.hwnd {
            return;
        }
        let ok = window::take_focus(self.hwnd);
        log_info!("focus retry {}", if ok { "worked" } else { "failed too" });
    }

    /// Carries out what a key press or click asked for.
    fn run(&self, action: Result<Action>) {
        let action = match action {
            Ok(a) => a,
            Err(e) => {
                log_error!("input handling failed: {e}");
                return;
            }
        };
        match action {
            Action::None => {}
            Action::Close => self.close(),
            Action::OpenSettings => self.open_settings(true),
            Action::OpenAccountSettings => {
                self.close();
                // SAFETY: opens a static URI; no pointers are kept.
                let result = unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        w!("ms-settings:accounts"),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    )
                };
                // ShellExecute returns a value > 32 on success.
                if result.0 as isize <= 32 {
                    log_error!("opening account settings failed ({})", result.0 as isize);
                }
            }
            Action::Power(power_action) => {
                self.close();
                power::run(power_action);
            }
            Action::Launch(id) => {
                // Close first (the close animation runs in DWM, so it stays
                // smooth), then start the app on a helper thread.
                self.close();
                self.ui.record_launch(&id);
                apps::launch(id);
                // Saved to config.toml a few seconds after the last launch.
                self.stats_dirty.set(true);
                // SAFETY: a one-shot timer on our own window (killed when it
                // fires); setting it again restarts the wait.
                unsafe { SetTimer(Some(self.hwnd), TIMER_SAVE_STATS, SAVE_STATS_MS, None) };
            }
            Action::Pin(id) => self.change_pin(id, true),
            Action::Unpin(id) => self.change_pin(id, false),
            Action::Media(command) => {
                self.media.command(command);
                // Switching source changes the card right away; the other
                // buttons come back as playback events.
                if command == media::MediaCommand::SwitchSource {
                    self.push_music();
                }
            }
        }
    }

    /// Shows the current music state on the card (`[music] enabled =
    /// false`: never).
    fn push_music(&self) {
        if self.stress.is_some() {
            return; // the stress test shows its own made-up songs
        }
        let enabled = self.config.borrow().music.enabled;
        let view = if enabled { self.media.view() } else { None };
        log_err("music card", self.ui.set_music(view));
    }

    // -----------------------------------------------------------------------
    // config.toml
    // -----------------------------------------------------------------------

    /// config.toml changed (the watcher said so): reload once the editor
    /// has finished saving (300 ms of quiet).
    fn on_config_changed(&self) {
        // SAFETY: a one-shot timer on our own window (killed when it
        // fires); setting it again restarts the wait.
        unsafe { SetTimer(Some(self.hwnd), TIMER_CONFIG, CONFIG_SETTLE_MS, None) };
    }

    /// Reads config.toml again and applies what changed. A broken file
    /// changes nothing: the last good settings stay. `manual` = from the
    /// tray's "Reload config" (then success is shown too).
    fn reload_config(&self, manual: bool) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        // Something Settings changed and hasn't saved yet goes into the
        // file first; otherwise this reload would undo it.
        self.save_settings();
        match config::load(&path) {
            Ok(Some(new)) => {
                *self.file_config.borrow_mut() = new.clone();
                self.apply_config(new, "config.toml reloaded");
                if manual {
                    self.tray.notify("Glint", "config.toml reloaded.", false);
                }
            }
            Ok(None) => {
                // Deleted: back to the defaults (that's what deleting means).
                log_info!("config.toml was deleted; writing the defaults again");
                let fresh = write_default_config(&path);
                *self.file_config.borrow_mut() = fresh.clone();
                self.apply_config(fresh, "config.toml reloaded");
            }
            Err(e) => self.config_error(&e, false),
        }
    }

    /// Logs the problem and shows a small notice next to the tray icon.
    /// `startup` = there are no good settings yet, so the defaults are used.
    fn config_error(&self, e: &ConfigError, startup: bool) {
        let fallback = if startup {
            "Glint uses the default settings until it is fixed."
        } else {
            "Glint keeps using the last good settings."
        };
        log_error!("config.toml {e}. {fallback}");
        let title = match e.line {
            Some(line) => format!("config.toml has an error on line {line}"),
            None => "config.toml has an error".to_string(),
        };
        let text = match &e.snippet {
            Some(snippet) => format!("{snippet}\n{}\n{fallback}", e.message),
            None => format!("{}\n{fallback}", e.message),
        };
        self.tray.notify(&title, &text, true);
    }

    /// Switches to `new`, changing only what is different. Reloading the
    /// file Glint itself just saved therefore does nothing. `why` is for
    /// the log.
    fn apply_config(&self, new: Config, why: &str) {
        let old = self.config.replace(new.clone());
        let mut changed = Vec::new();
        if old.background != new.background {
            changed.push("background");
            self.apply_background();
        }
        if old.accent != new.accent {
            changed.push("accent");
            self.apply_accent();
        }
        if old.animation != new.animation {
            changed.push("animation");
            log_err("animation", self.ui.set_animation(&new.animation));
        }
        if old.pinned != new.pinned || old.max_columns != new.max_columns {
            changed.push("pins");
            self.apply_pins();
        }
        if old.show_names != new.show_names {
            changed.push("app names");
            log_err("app names", self.ui.set_show_names(new.show_names));
        }
        if old.autostart != new.autostart {
            changed.push("autostart");
            autostart::sync(new.autostart);
        }
        if old.hotkey != new.hotkey {
            changed.push("hotkey");
            self.register_hotkey();
        }
        if old.music != new.music
            || old.background.tint_from_album != new.background.tint_from_album
        {
            changed.push("music");
            self.ui.set_tint_from_album(new.background.tint_from_album);
            self.push_music();
        }
        if old.stats != new.stats {
            // Edited by hand. Launches not saved yet win: they are written
            // in a moment anyway.
            if !self.stats_dirty.get() {
                changed.push("launch counts");
                self.ui.set_launch_counts(&new.stats.launch_counts);
            }
        }
        if !changed.is_empty() {
            log_info!("{why}: {}", changed.join(", "));
            // An open Settings window shows the new values (a no-op when
            // the change came from Settings itself).
            if let Some(s) = self.settings_window.borrow().as_ref() {
                s.sync(self.ui.gfx(), &self.config.borrow());
            }
        }
    }

    /// The accent color again (config changed, or the Windows accent for
    /// `accent = "system"`).
    fn apply_accent(&self) {
        let accent = resolve_accent(&self.config.borrow(), &self.settings);
        if self.accent.replace(accent) != accent {
            log_err("accent color", self.ui.set_accent(accent));
            if let Some(s) = self.settings_window.borrow().as_ref() {
                s.set_accent(self.ui.gfx(), accent);
            }
        }
    }

    /// Pins or max_columns changed: the grid follows, gliding while open.
    fn apply_pins(&self) {
        let (pins, columns) = {
            let c = self.config.borrow();
            (c.pinned.clone(), c.max_columns)
        };
        let shown = self.is_visible.get();
        let result = self.ui.set_pins(&pins, columns, |size| {
            // A hidden menu is placed fresh on the next open.
            if !shown {
                return None;
            }
            let p = self.placement_for(size);
            if self.placement.get() == Some(p) {
                return None;
            }
            self.show_window(&p);
            self.placement.set(Some(p));
            Some(p)
        });
        log_err("updating the pins", result);
    }

    /// "Pin to Glint" / "Unpin from Glint": saved to config.toml (keeping
    /// its comments), then shown.
    fn change_pin(&self, id: String, pin: bool) {
        let mut pins = self.config.borrow().pinned.clone();
        let has = pins.iter().any(|p| p.eq_ignore_ascii_case(&id));
        if pin == has {
            return;
        }
        if pin {
            pins.push(id.clone());
        } else {
            pins.retain(|p| !p.eq_ignore_ascii_case(&id));
        }
        if let Some(path) = &self.config_path
            && let Err(e) = config::edit(path, |doc| config::set_pins(doc, &pins))
        {
            // Not saved = not done, so what you see matches the file.
            log_error!("saving the pins failed: config.toml {e}");
            let what = if pin { "Pin" } else { "Unpin" };
            self.tray.notify(
                &format!("{what} didn't work"),
                &format!("config.toml {e}\nFix the file, then try again."),
                true,
            );
            return;
        }
        log_info!("{} {id}", if pin { "pinned" } else { "unpinned" });
        self.file_config.borrow_mut().pinned = pins.clone();
        self.config.borrow_mut().pinned = pins;
        self.apply_pins();
    }

    /// The tray's "Start with Windows": saved to config.toml first (so the
    /// file stays the one place the setting lives), then the registry.
    fn set_autostart(&self, on: bool) {
        if let Some(path) = &self.config_path
            && let Err(e) = config::edit(path, |doc| config::set_autostart(doc, on))
        {
            log_error!("saving autostart failed: config.toml {e}");
            self.tray.notify(
                "Start with Windows didn't change",
                &format!(
                    "config.toml {e}
Fix the file, then try again."
                ),
                true,
            );
            return;
        }
        self.file_config.borrow_mut().autostart = on;
        self.config.borrow_mut().autostart = on;
        autostart::sync(on);
        if let Some(s) = self.settings_window.borrow().as_ref() {
            s.sync(self.ui.gfx(), &self.config.borrow());
        }
    }

    // -----------------------------------------------------------------------
    // The Settings window (milestone 8)
    // -----------------------------------------------------------------------

    /// Opens Settings (or brings it to the front), with the menu open
    /// beside it as a live preview. `activate` = give Settings the
    /// keyboard and open the preview (false only in the stress test, which
    /// opens the menu itself).
    fn open_settings(&self, activate: bool) {
        if let Some((_, hwnd)) = self.preview_anchor() {
            settings::show(hwnd, true);
            return;
        }
        let at = settings::initial_rect(self.ui.max_size().X);
        let opened = settings::SettingsWindow::open(
            Some(settings_wndproc),
            at,
            self.ui.compositor(),
            self.ui.gfx(),
            &self.config.borrow(),
            self.accent.get(),
            self.settings.effects(),
        );
        let hwnd = match opened {
            Ok(w) => {
                let hwnd = w.hwnd();
                *self.settings_window.borrow_mut() = Some(w);
                hwnd
            }
            Err(e) => {
                log_error!("opening Settings failed: {e}");
                return;
            }
        };
        if self.stress.is_none() {
            log_info!("settings opened");
        }
        // The preview: a closed menu opens beside Settings; an open one (or
        // one still closing) moves there and stops being topmost.
        if activate && !self.is_open.get() {
            self.open_with(false);
        }
        self.reposition(true);
        settings::show(hwnd, activate);
    }

    /// Closes Settings: saves what isn't saved, drops the window and all
    /// it made, and closes the preview. Memory is handed back afterwards.
    fn close_settings(&self) {
        self.save_settings();
        // Taken out first, so nothing is borrowed while it is destroyed
        // (destroying sends it messages).
        let window = self.settings_window.borrow_mut().take();
        let Some(window) = window else {
            return;
        };
        drop(window);
        if self.stress.is_none() {
            log_info!("settings closed");
        }
        // SAFETY: stops our own one-shot timer (if it is running).
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_REPLAY) };
        if self.hotkey_paused.replace(false) {
            self.register_hotkey();
        }
        // Settings' fonts, surfaces and scratch memory are free now: hand
        // them back (once the preview's close animation is over).
        if self.is_visible.get() {
            self.trim_after_close.set(true);
            self.close();
        } else {
            self.ui.release_caches();
            trim_memory();
        }
    }

    /// Carries out what Settings asked for (with nothing borrowed: the
    /// color picker runs its own message loop).
    fn run_settings(&self, action: settings::Action) {
        use settings::Action as A;
        match action {
            A::None => {}
            A::Apply(new) => self.apply_from_settings(*new),
            A::PickColor(target) => {
                let Some((hwnd, start)) = self
                    .settings_window
                    .borrow()
                    .as_ref()
                    .map(|s| (s.hwnd(), s.current_color(target)))
                else {
                    return;
                };
                let Some(color) = settings::choose_color(hwnd, start) else {
                    return;
                };
                let next = self
                    .settings_window
                    .borrow()
                    .as_ref()
                    .map(|s| s.color_picked(self.ui.gfx(), target, color));
                if let Some(next) = next {
                    self.run_settings(next);
                }
            }
            A::Replay => {
                // SAFETY: stops our own one-shot timer (if it is running).
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_REPLAY) };
                self.replay();
            }
            A::Recording(true) => {
                if self.hotkey_on.replace(false) {
                    // SAFETY: removes the hotkey we registered on our window.
                    let _ = unsafe { UnregisterHotKey(Some(self.hwnd), HOTKEY_ID) };
                }
                self.hotkey_paused.set(true);
            }
            A::Recording(false) => {
                if self.hotkey_paused.replace(false) {
                    self.register_hotkey();
                }
            }
            A::OpenConfigFile => match &self.config_path {
                Some(path) => apps::open_document(path.clone()),
                None => log_warn!("no config file (APPDATA is not set)"),
            },
            A::OpenUrl(url) => {
                let url = windows::core::HSTRING::from(url);
                // SAFETY: opens a URL; `url` outlives the call.
                let result =
                    unsafe { ShellExecuteW(None, w!("open"), &url, None, None, SW_SHOWNORMAL) };
                if result.0 as isize <= 32 {
                    log_error!("opening {url} failed ({})", result.0 as isize);
                }
            }
            A::Close => self.close_settings(),
        }
    }

    /// A change from Settings: the menu shows it now, the file gets it
    /// 300 ms after the last change.
    fn apply_from_settings(&self, new: Config) {
        let (animation_changed, same_hotkey) = {
            let c = self.config.borrow();
            (c.animation != new.animation, c.hotkey == new.hotkey)
        };
        self.apply_config(new, "settings");
        // A recording that ended without a new hotkey: switch it back on.
        // (A new one was registered by `apply_config`.)
        if self.hotkey_paused.replace(false) && same_hotkey {
            self.register_hotkey();
        }
        self.settings_dirty.set(true);
        // SAFETY: one-shot timers on our own window; setting one again
        // restarts its wait.
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_SETTINGS_SAVE, SETTINGS_SAVE_MS, None);
            if animation_changed {
                SetTimer(Some(self.hwnd), TIMER_REPLAY, SETTINGS_SAVE_MS, None);
            }
        }
    }

    /// Plays the open animation again (the preview opens if it was closed).
    fn replay(&self) {
        if self.is_open.get() {
            log_err("replay", self.ui.replay_open(self.reduced_motion()));
        } else if self.in_preview() {
            self.open_with(false);
        }
    }

    /// Writes what Settings changed to config.toml, keeping everything
    /// else in the file (comments too) as it is.
    fn save_settings(&self) {
        // SAFETY: stops our own one-shot timer (if it is running).
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_SETTINGS_SAVE) };
        if !self.settings_dirty.replace(false) {
            return;
        }
        let Some(path) = &self.config_path else {
            return;
        };
        let old = self.file_config.borrow().clone();
        let new = self.config.borrow().clone();
        match config::edit(path, |doc| config::save_changes(doc, &old, &new)) {
            Ok(()) => *self.file_config.borrow_mut() = new,
            Err(e) => {
                log_error!("saving settings failed: config.toml {e}");
                self.tray.notify(
                    "Settings not saved",
                    &format!("config.toml {e}\nFix the file, then change the setting again."),
                    true,
                );
            }
        }
    }

    /// A message for the Settings window. `None` = let Windows handle it.
    fn on_settings_message(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let gfx = self.ui.gfx();
        // Runs `f` on the window (if it is open) and returns what it asked
        // for. Only borrowed (never taken) here, so nested messages are fine.
        let with = |f: &dyn Fn(&settings::SettingsWindow) -> settings::Action| {
            self.settings_window
                .borrow()
                .as_ref()
                .map_or(settings::Action::None, f)
        };
        let action = match msg {
            WM_CLOSE => {
                self.close_settings();
                return Some(LRESULT(0));
            }
            WM_SIZE => with(&|s| {
                s.resized(gfx);
                settings::Action::None
            }),
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                with(&|s| {
                    s.set_dpi(gfx, dpi);
                    settings::Action::None
                });
                // SAFETY: for WM_DPICHANGED, lParam points to the suggested
                // window rect, valid during the message.
                if let (Some(r), Some((_, hwnd))) = (
                    unsafe { (lparam.0 as *const windows::Win32::Foundation::RECT).as_ref() },
                    self.preview_anchor(),
                ) {
                    // SAFETY: moves our own window to the suggested rect.
                    let _ = unsafe {
                        windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        )
                    };
                }
                return Some(LRESULT(0));
            }
            // Settings was moved: the preview follows it.
            WM_EXITSIZEMOVE => {
                self.reposition(true);
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse_xy(lparam);
                with(&|s| s.mouse_move(gfx, x, y))
            }
            WM_MOUSELEAVE => with(&|s| {
                s.mouse_leave();
                settings::Action::None
            }),
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_xy(lparam);
                with(&|s| s.mouse_down(gfx, x, y))
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_xy(lparam);
                with(&|s| s.mouse_up(gfx, x, y))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let recording = self
                    .settings_window
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.is_recording());
                // Alt+F4 and the other system keys work as usual, except
                // while a hotkey is being recorded.
                if msg == WM_SYSKEYDOWN && !recording {
                    return None;
                }
                let (ctrl, alt, shift) =
                    (key_held(VK_CONTROL), key_held(VK_MENU), key_held(VK_SHIFT));
                let vk = wparam.0 as u16;
                with(&|s| s.key_down(gfx, vk, ctrl, alt, shift))
            }
            // Recording Alt+key must not open the window menu.
            WM_SYSCOMMAND if (wparam.0 & 0xFFF0) as u32 == SC_KEYMENU => {
                let recording = self
                    .settings_window
                    .borrow()
                    .as_ref()
                    .is_some_and(|s| s.is_recording());
                if recording {
                    return Some(LRESULT(0));
                }
                return None;
            }
            _ => return None,
        };
        self.run_settings(action);
        Some(LRESULT(0))
    }

    /// Writes the launch counts to `[stats]` in config.toml, if any are new.
    fn save_stats(&self) {
        // SAFETY: stops our own one-shot timer (if it is running).
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_SAVE_STATS) };
        if !self.stats_dirty.get() {
            return;
        }
        let Some(path) = &self.config_path else {
            self.stats_dirty.set(false);
            return;
        };
        let counts = self.ui.launch_counts();
        match config::edit(path, |doc| config::set_launch_counts(doc, &counts)) {
            Ok(()) => {
                self.config.borrow_mut().stats.launch_counts = counts;
                self.stats_dirty.set(false);
            }
            // Tried again after the next launch, and on quit.
            Err(e) => log_warn!("saving launch counts failed: config.toml {e}"),
        }
    }

    /// Registers the `hotkey` from the config (and drops the old one).
    fn register_hotkey(&self) {
        if self.hotkey_on.replace(false) {
            // SAFETY: removes the hotkey we registered on our own window.
            let _ = unsafe { UnregisterHotKey(Some(self.hwnd), HOTKEY_ID) };
        }
        let Some(key) = self.config.borrow().hotkey.clone() else {
            return;
        };
        let mut mods = MOD_NOREPEAT;
        for (on, m) in [
            (key.ctrl, MOD_CONTROL),
            (key.alt, MOD_ALT),
            (key.shift, MOD_SHIFT),
            (key.win, MOD_WIN),
        ] {
            if on {
                mods |= m;
            }
        }
        // SAFETY: registers a hotkey for our own window; Windows frees it
        // when the window is destroyed.
        match unsafe { RegisterHotKey(Some(self.hwnd), HOTKEY_ID, mods, u32::from(key.vk)) } {
            Ok(()) => {
                self.hotkey_on.set(true);
                log_info!("hotkey {} registered", key.text);
            }
            Err(e) => {
                log_warn!("hotkey {} not available: {e}", key.text);
                if let Some(s) = self.settings_window.borrow().as_ref() {
                    let note = format!("{} is already used by Windows or another app.", key.text);
                    s.set_hotkey_note(self.ui.gfx(), Some(note));
                    return;
                }
                self.tray.notify(
                    "Glint hotkey not available",
                    &format!("{} is already used by Windows or another app.", key.text),
                    true,
                );
            }
        }
    }

    /// Lists all apps again, on a helper thread.
    fn scan_apps(&self) {
        if self.scanning.replace(true) {
            self.rescan.set(true);
            return;
        }
        apps::scan_in_background(self.hwnd, self.apps_tx.clone());
    }

    /// The scan thread is done: use the newest list.
    fn on_apps_loaded(&self) {
        let mut latest = None;
        while let Ok(list) = self.apps_rx.try_recv() {
            latest = Some(list);
        }
        self.scanning.set(false);
        if let Some(list) = latest {
            log_err("updating the app list", self.ui.set_apps(list));
            // "Playing from" can now name the app.
            self.push_music();
        }
        if self.rescan.replace(false) {
            self.scan_apps();
        } else if !self.is_visible.get() {
            // A scan touches memory it never needs again (the shell's own
            // bookkeeping); hand it back, like the startup trim. Scans are
            // rare (startup, installs), so this costs nothing. Measured
            // (milestone 4): idle 10.3 MB after startup and +3.5 MB after
            // the first rescan without this.
            trim_memory();
        }
        // The leak test starts once search has the app list.
        if let Some(s) = &self.stress
            && s.begin()
        {
            // SAFETY: posts a plain message (no pointers) to our window.
            let _ = unsafe {
                PostMessageW(Some(self.hwnd), stress::WM_APP_STRESS, WPARAM(0), LPARAM(0))
            };
        }
    }

    /// The shell says something changed in the Start menu folders: wait for
    /// 2 s of quiet (restarting the wait on every change), then scan.
    fn on_apps_changed(&self, wparam: WPARAM, lparam: LPARAM) {
        ChangeWatch::acknowledge(wparam, lparam);
        // SAFETY: a one-shot timer on our own window (killed when it fires).
        unsafe { SetTimer(Some(self.hwnd), TIMER_APPS_SETTLE, APPS_SETTLE_MS, None) };
    }

    fn on_timer(&self, id: usize) {
        match id {
            TIMER_APPS_SETTLE => {
                // SAFETY: stops our own timer; it is one-shot.
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_APPS_SETTLE) };
                log_info!("apps changed; listing them again");
                self.scan_apps();
            }
            TIMER_CONFIG => {
                // SAFETY: stops our own timer; it is one-shot.
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_CONFIG) };
                self.reload_config(false);
            }
            TIMER_SAVE_STATS => self.save_stats(),
            TIMER_SETTINGS_SAVE => self.save_settings(),
            TIMER_REPLAY => {
                // SAFETY: stops our own timer; it is one-shot.
                let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_REPLAY) };
                self.replay();
            }
            stress::TIMER_STRESS => self.stress_close(),
            ui::TIMER_ICONS => self.ui.load_icons(),
            ui::TIMER_SEEK => log_err("music time", self.ui.music_tick()),
            _ => {}
        }
    }

    /// Asks Windows for one WM_MOUSELEAVE when the mouse leaves the window.
    fn track_mouse(&self) {
        if self.tracking_mouse.replace(true) {
            return;
        }
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: valid, correctly sized struct for our own window.
        let _ = unsafe { TrackMouseEvent(&mut tme) };
    }

    fn close(&self) {
        if !self.is_open.replace(false) {
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.closed_at.set(now_ms());
        // No timeline events while hidden.
        self.media.set_open(false);

        match self.ui.animate_close(self.reduced_motion()) {
            Ok(batch) => {
                // Completed fires on this thread when the animation ends (or
                // is interrupted by a reopen). Hide the window then.
                let target = self.hwnd.0 as isize;
                let handler = TypedEventHandler::new(move |_, _| {
                    // SAFETY: posts a plain message (no pointers) to our window.
                    let _ = unsafe {
                        PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_CLOSE_DONE,
                            WPARAM(generation as usize),
                            LPARAM(0),
                        )
                    };
                    Ok(())
                });
                if let Err(e) = batch.Completed(&handler) {
                    log_error!("batch.Completed failed: {e}");
                    self.finish_close(generation);
                }
                *self.close_batch.borrow_mut() = Some(batch);
            }
            Err(e) => {
                log_error!("close animation failed: {e}");
                self.finish_close(generation);
            }
        }

        // Cheap insurance: if Windows silently dropped our hook, this puts it
        // back. (The tray icon still opens the menu if the hook is gone.)
        self.hook.reinstall(false);
    }

    /// The close animation is done: hide (never destroy) the window.
    fn finish_close(&self, generation: u64) {
        if generation != self.generation.get() || self.is_open.get() {
            return; // a reopen interrupted this close
        }
        self.close_batch.borrow_mut().take();
        self.is_visible.set(false);
        window::hide(self.hwnd);
        self.ui.release_caches();
        if self.trim_after_close.replace(false) {
            trim_memory();
        }
        if let Some(s) = &self.stress {
            if s.after_cycle() {
                // SAFETY: posts WM_QUIT to this (the UI) thread.
                unsafe { PostQuitMessage(0) };
            } else {
                // SAFETY: posts a plain message (no pointers) to our window.
                let _ = unsafe {
                    PostMessageW(Some(self.hwnd), stress::WM_APP_STRESS, WPARAM(0), LPARAM(0))
                };
            }
        }
    }

    /// Re-place the open menu, e.g. after a DPI change, or beside Settings.
    /// `always` = show the window again even if it stays put (to change
    /// whether it is topmost).
    fn reposition(&self, always: bool) {
        if !self.is_open.get() {
            return;
        }
        let p = self.placement();
        if self.placement.get() != Some(p) {
            log_err("layout", self.ui.set_layout(&p));
            self.media.set_cover_px(self.ui.cover_px());
            self.show_window(&p);
            self.placement.set(Some(p));
        } else if always {
            self.show_window(&p);
        }
    }

    fn apply_background(&self) {
        let effects = self.settings.effects();
        let config = self.config.borrow();
        if let Some(s) = self.settings_window.borrow().as_ref() {
            s.apply_background(&config, effects);
        }
        match self.ui.apply_background(&config, effects) {
            Ok(applied) => {
                let bg = &config.background;
                let line = format!(
                    "background: {} / {} / tint {:.0}% (blur: {}, overlay {:.2}; transparency effects {}, energy saver {})",
                    bg.mode.name(),
                    bg.preset.name(),
                    bg.tint * 100.0,
                    if applied.glass { "on" } else { "off" },
                    applied.overlay,
                    if effects.transparency { "on" } else { "off" },
                    if effects.battery_saver { "on" } else { "off" },
                ) + if effects.high_contrast.is_some() {
                    "; HIGH CONTRAST"
                } else {
                    ""
                };
                // Only log real changes (WM_SETTINGCHANGE can be chatty).
                if *self.last_background_log.borrow() != line {
                    log_info!("{line}");
                    *self.last_background_log.borrow_mut() = line;
                }
            }
            Err(e) => log_error!("applying the background failed: {e}"),
        }
    }

    // -----------------------------------------------------------------------
    // `--stress N` (the leak test; see stress.rs)
    // -----------------------------------------------------------------------

    /// A cycle starts: change what this cycle changes, then open.
    fn stress_open(&self) {
        let Some(s) = &self.stress else {
            return;
        };
        let (n, plan) = s.next();

        // Background mode: on the menu only (config.toml is never written).
        let mut look = self.config.borrow().clone();
        look.background.mode = plan.background;
        let applied = self.ui.apply_background(&look, self.settings.effects());
        log_err("stress: background", applied.map(|_| ()));

        // Pins: take the last one away, 25 cycles later put it back.
        if let Some(back) = plan.pin {
            let mut parked = s.parked_pin.borrow_mut();
            let changed = {
                let mut config = self.config.borrow_mut();
                if back {
                    parked.take().map(|p| config.pinned.push(p)).is_some()
                } else if config.pinned.len() > 1 {
                    *parked = config.pinned.pop();
                    true
                } else {
                    false
                }
            };
            drop(parked);
            if changed {
                self.apply_pins();
            }
        }
        if plan.reload_config {
            self.reload_config(false);
        }
        if plan.device_loss {
            self.ui.simulate_device_loss();
        }
        let song = plan.music.then(|| stress::fake_song(n, plan.two_sources));
        log_err("stress: music card", self.ui.set_music(song));
        if plan.settings {
            // Opened without the keyboard; the menu opens beside it.
            self.open_settings(false);
        }

        self.open();
        // Let the open animation play fully, like a real open.
        let wait = self.config.borrow().animation.open_ms + 60;
        // SAFETY: a one-shot timer on our own window (killed when it fires).
        unsafe { SetTimer(Some(self.hwnd), stress::TIMER_STRESS, wait, None) };
    }

    /// The menu is open: type a search, clear it with Esc, close.
    fn stress_close(&self) {
        // SAFETY: stops our own one-shot timer.
        let _ = unsafe { KillTimer(Some(self.hwnd), stress::TIMER_STRESS) };
        for unit in "no".encode_utf16() {
            log_err("stress: typing", self.ui.char_typed(unit));
        }
        self.run(self.ui.key_down(VK_ESCAPE.0, false, false));
        // Closing Settings closes the preview too.
        self.close_settings();
        self.close();
    }

    fn on_tray(&self, event: u32) {
        match event {
            WM_LBUTTONUP => {
                let just_closed = now_ms().saturating_sub(self.closed_at.get()) < REOPEN_GUARD_MS;
                if !self.is_open.get() && !just_closed {
                    self.open();
                }
            }
            WM_RBUTTONUP => {
                self.close();
                let debug = key_held(VK_SHIFT);
                let autostart = self.config.borrow().autostart;
                match self.tray.show_menu(autostart, debug) {
                    Some(TrayCommand::Settings) => self.open_settings(true),
                    Some(TrayCommand::Quit) => {
                        self.save_stats();
                        // SAFETY: posts WM_QUIT to this (the UI) thread.
                        unsafe { PostQuitMessage(0) };
                    }
                    Some(TrayCommand::OpenConfig) => match &self.config_path {
                        Some(path) => apps::open_document(path.clone()),
                        None => log_warn!("no config file (APPDATA is not set)"),
                    },
                    Some(TrayCommand::ReloadConfig) => self.reload_config(true),
                    Some(TrayCommand::ToggleAutostart) => self.set_autostart(!autostart),
                    Some(TrayCommand::SimulateGpuReset) => self.ui.simulate_device_loss(),
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// A mouse button went down. A click on the transparent margin around
    /// the menu counts as "outside" and closes it; a left click on the menu
    /// goes to the UI.
    fn on_button_down(&self, msg: u32, lparam: LPARAM) {
        if !self.is_open.get() {
            return;
        }
        let (x, y) = mouse_xy(lparam);
        let on_menu = self.ui.menu_contains(x, y);
        if !on_menu {
            // The preview stays while Settings is open.
            if !self.in_preview() {
                self.close();
            }
        } else if msg == WM_LBUTTONDOWN {
            self.run(self.ui.mouse_down(x, y, key_held(VK_SHIFT)));
        }
    }

    /// Another window was activated: close. If that happens right after we
    /// opened, the user didn't do it, so log who took the focus.
    fn on_deactivate(&self, lparam: LPARAM) {
        // The stress test closes the menu itself; the preview stays open
        // while you use Settings.
        if self.stress.is_some() || self.in_preview() {
            return;
        }
        let open_for = now_ms().saturating_sub(self.opened_at.get());
        if self.is_open.get() && open_for < 1500 {
            let other = HWND(lparam.0 as *mut _);
            // SAFETY: GetForegroundWindow has no preconditions.
            let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            log_warn!(
                "focus taken {open_for} ms after opening, by {} (foreground now: {} {})",
                window::process_name(other),
                window::process_name(fg),
                window::class_name(fg),
            );
        }
        self.close();
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(app) = app() {
        match msg {
            stress::WM_APP_STRESS => {
                app.stress_open();
                return LRESULT(0);
            }
            WM_APP_TOGGLE => {
                app.toggle();
                return LRESULT(0);
            }
            settings::WM_APP_OPEN_SETTINGS => {
                app.open_settings(true);
                return LRESULT(0);
            }
            WM_APP_TRAY => {
                app.on_tray(lparam.0 as u32);
                return LRESULT(0);
            }
            WM_APP_CLOSE_DONE => {
                app.finish_close(wparam.0 as u64);
                return LRESULT(0);
            }
            WM_APP_SETTINGS_CHANGED | WM_SETTINGCHANGE => {
                app.apply_background();
                // The Windows accent color may have changed.
                app.apply_accent();
            }
            WM_APP_CONFIG_CHANGED => {
                app.on_config_changed();
                return LRESULT(0);
            }
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
                app.toggle();
                return LRESULT(0);
            }
            // Right-click (sent by Windows after WM_RBUTTONUP), Shift+F10 or
            // the Menu key (then lParam is -1): the Pin / Unpin menu.
            WM_CONTEXTMENU => {
                let at = (lparam.0 != -1).then(|| {
                    let (x, y) = mouse_xy(lparam);
                    let mut pt = windows::Win32::Foundation::POINT { x, y };
                    // SAFETY: converts a point to our own window's client
                    // coordinates; valid out-pointer.
                    let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt) };
                    (pt.x, pt.y)
                });
                log_err("right-click menu", app.ui.context_menu(at));
                return LRESULT(0);
            }
            // Shutdown or sign-out: save what isn't saved yet.
            WM_ENDSESSION if wparam.0 != 0 => app.save_stats(),
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => app.on_deactivate(lparam),
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => app.on_button_down(msg, lparam),
            WM_LBUTTONUP if app.is_open.get() => {
                let (x, y) = mouse_xy(lparam);
                app.run(app.ui.mouse_up(x, y));
            }
            WM_MOUSEMOVE if app.is_open.get() => {
                app.track_mouse();
                let (x, y) = mouse_xy(lparam);
                log_err("hover", app.ui.mouse_move(x, y));
            }
            WM_MOUSELEAVE => {
                app.tracking_mouse.set(false);
                log_err("hover", app.ui.mouse_leave());
            }
            // An I-beam cursor over the search box.
            WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT && app.is_open.get() => {
                let mut pt = windows::Win32::Foundation::POINT::default();
                // SAFETY: valid out-pointer; converts to our own window's
                // client coordinates. The I-beam is a shared system cursor.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
                    if app.ui.over_text(pt.x, pt.y)
                        && let Ok(cursor) = LoadCursorW(None, IDC_IBEAM)
                    {
                        SetCursor(Some(cursor));
                        return LRESULT(1);
                    }
                }
            }
            // Keys: editing, Esc, arrows, Tab, Enter (see ui::Ui::key_down).
            WM_KEYDOWN if app.is_open.get() => {
                let action =
                    app.ui
                        .key_down(wparam.0 as u16, key_held(VK_CONTROL), key_held(VK_SHIFT));
                app.run(action);
                return LRESULT(0);
            }
            // Typed text (one UTF-16 unit at a time).
            WM_CHAR if app.is_open.get() => {
                log_err("typing", app.ui.char_typed(wparam.0 as u16));
                return LRESULT(0);
            }
            // IME: we draw the composition ourselves, so hide Windows' own
            // composition window, then let the default handling continue.
            WM_IME_SETCONTEXT => {
                let lparam = LPARAM(lparam.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
                // SAFETY: default handling with a modified flag set.
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            WM_IME_STARTCOMPOSITION => {
                log_err("IME", app.ui.ime_start());
                return LRESULT(0);
            }
            WM_IME_COMPOSITION => {
                log_err("IME", app.ui.ime_composition(lparam.0 as u32));
                return LRESULT(0);
            }
            WM_IME_ENDCOMPOSITION => {
                log_err("IME", app.ui.ime_end());
                return LRESULT(0);
            }
            WM_APP_FOCUS_RETRY => {
                app.retry_focus();
                return LRESULT(0);
            }
            WM_APP_APPS_LOADED => {
                app.on_apps_loaded();
                return LRESULT(0);
            }
            WM_APP_APPS_CHANGED => {
                app.on_apps_changed(wparam, lparam);
                return LRESULT(0);
            }
            WM_APP_MEDIA_READY => {
                app.media.on_ready();
                app.push_music();
                return LRESULT(0);
            }
            WM_APP_MEDIA => {
                app.media.on_changed(wparam.0);
                app.push_music();
                return LRESULT(0);
            }
            WM_APP_MEDIA_INFO => {
                if app.media.on_info() {
                    app.push_music();
                }
                return LRESULT(0);
            }
            WM_TIMER => {
                app.on_timer(wparam.0);
                return LRESULT(0);
            }
            WM_APP_REDRAW => {
                log_warn!("graphics device replaced; repainting");
                log_err("repaint", app.ui.device_replaced());
                if let Some(s) = app.settings_window.borrow().as_ref() {
                    s.device_replaced(app.ui.gfx());
                }
                // The new device's driver memory is all in RAM (measured in
                // milestone 7: +10 MB working set after the first reset, and
                // nothing more on later ones). Hand it back, like at startup;
                // not while the menu shows (that would only page it back in).
                if app.is_visible.get() {
                    app.trim_after_close.set(true);
                } else {
                    trim_memory();
                }
                return LRESULT(0);
            }
            // Alt+F4 must hide the menu, never destroy the window.
            WM_CLOSE => {
                app.close();
                return LRESULT(0);
            }
            // We size the window ourselves, so ignore the suggested rect.
            WM_DPICHANGED => {
                app.reposition(false);
                return LRESULT(0);
            }
            // A monitor was plugged in or out, or a resolution changed: the
            // open menu may now sit in the wrong place (or off-screen). Close
            // it; the next open picks the monitor and size fresh. The GPU may
            // have changed too.
            WM_DISPLAYCHANGE => {
                log_info!("display configuration changed");
                app.close();
                app.ui.ensure_device();
                return LRESULT(0);
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                app.hook.reinstall(true)
            }
            WM_POWERBROADCAST => match wparam.0 as u32 {
                PBT_APMRESUMEAUTOMATIC => {
                    app.hook.reinstall(true);
                    app.ui.ensure_device();
                }
                PBT_POWERSETTINGCHANGE => app.apply_background(), // battery saver
                _ => {}
            },
            m if m == app.msg_taskbar_created => {
                // Explorer restarted: our tray icon is gone, add it again.
                if let Err(e) = app.tray.register() {
                    log_error!("re-adding the tray icon failed: {e}");
                }
            }
            _ => {}
        }
    }
    // A tapped Alt (our focus workaround) must not open a window menu.
    if msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) as u32 == SC_KEYMENU {
        return LRESULT(0);
    }
    // SAFETY: default handling with the unchanged arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The Settings window's procedure. Its messages go to `App` like the
/// menu's; before the window is stored (while it is being created), Windows
/// handles them.
extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_GETMINMAXINFO {
        settings::min_max_info(hwnd, lparam);
        return LRESULT(0);
    }
    if let Some(app) = app()
        && let Some(result) = app.on_settings_message(msg, wparam, lparam)
    {
        return result;
    }
    // SAFETY: default handling with the unchanged arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Startup touches a lot of memory only once: creating the GPU device,
/// loading fonts, asking the shell for icons. Measured (milestone 3): about
/// 21 MB right after startup, but an open/close cycle afterwards needs only
/// about 5 MB. Handing the rest back to Windows keeps idle RAM low.
/// The same after each app scan (see `App::on_apps_loaded`).
/// And after a GPU device reset (see `WM_APP_REDRAW`).
/// (Not done after every close: that would only make each open fault the
/// same 5 MB back in.)
fn trim_memory() {
    // SAFETY: (-1, -1) = "trim what you can" for our own process.
    if let Err(e) = unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) }
    {
        log_warn!("SetProcessWorkingSetSize failed: {e}");
    }
}

/// Returns the mutex handle if we are the first instance, None otherwise.
fn single_instance() -> Result<Option<HANDLE>> {
    // SAFETY: creates a named mutex; the name is a static string.
    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\Glint.SingleInstance.7f3c9a1e"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            return Ok(None);
        }
        Ok(Some(mutex))
    }
}

fn create_dispatcher_queue() -> Result<DispatcherQueueController> {
    let options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_NONE,
    };
    // SAFETY: called once on the UI thread after RoInitialize.
    unsafe { CreateDispatcherQueueController(options) }
}

/// Command-line flags (all for testing; a normal start has none).
#[derive(Default)]
struct Args {
    /// `--stress N` / `--stress-gpu N`: the leak tests (bool = GPU resets).
    stress: Option<(u32, bool)>,
    /// `--taskbar-edge top|left|right|bottom`: pretend the taskbar is there.
    taskbar_edge: Option<window::Edge>,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--stress" | "--stress-gpu" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => args.stress = Some((n, arg == "--stress-gpu")),
                None => log_warn!("{arg} needs a number, like {arg} 1000"),
            },
            "--taskbar-edge" => match it.next().as_deref().and_then(window::Edge::parse) {
                Some(edge) => args.taskbar_edge = Some(edge),
                None => log_warn!("--taskbar-edge needs top, left, right or bottom"),
            },
            other => log_warn!("unknown argument {other:?} (ignored)"),
        }
    }
    args
}

fn run() -> Result<()> {
    let args = parse_args();
    if let Some(edge) = args.taskbar_edge {
        log_info!("pretending the taskbar is on the {edge:?} edge");
        window::set_fake_taskbar(edge);
    }
    let Some(_instance) = single_instance()? else {
        log_info!("Glint is already running; exiting");
        return Ok(());
    };

    // SAFETY: process-wide setting, called before any window exists.
    if let Err(e) =
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
    {
        log_warn!("SetProcessDpiAwarenessContext failed: {e}");
    }

    // SAFETY: initializes WinRT once for this (the UI) thread.
    unsafe { RoInitialize(RO_INIT_SINGLETHREADED)? };
    let _dispatcher = create_dispatcher_queue()?;

    // config.toml (written with the defaults on first run).
    let config_path = config::path();
    let (config, config_problem) = load_startup_config(config_path.as_deref());
    let hwnd = window::create(Some(wndproc))?;
    let settings = SettingsWatch::new(hwnd)?;
    let accent = resolve_accent(&config, &settings);
    // Paint the content now, at the DPI of the monitor it will most likely
    // open on, so the first open is instant.
    let scale = window::placement(1.0, 1.0).scale;
    let ui = ui::Ui::new(hwnd, &config, accent, scale)?;
    let config_watch = config_path
        .as_deref()
        .and_then(std::path::Path::parent)
        .filter(|dir| dir.is_dir())
        .and_then(|dir| ConfigWatch::start(hwnd, dir.to_path_buf()));
    // Asks Windows for the media sessions; they arrive a moment later.
    let media = Media::new(hwnd, ui.cover_px());
    let tray = Tray::add(hwnd)?;
    let hook = HookThread::start(hwnd)?;

    // SAFETY: registers our own window for lock/unlock notifications.
    if let Err(e) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        log_warn!("WTSRegisterSessionNotification failed: {e}");
    }

    let (apps_tx, apps_rx) = channel();
    let app = Rc::new(App {
        hwnd,
        ui,
        tray,
        hook,
        settings,
        file_config: RefCell::new(config.clone()),
        config: RefCell::new(config),
        is_open: Cell::new(false),
        is_visible: Cell::new(false),
        placement: Cell::new(None),
        generation: Cell::new(0),
        close_batch: RefCell::new(None),
        closed_at: Cell::new(0),
        opened_at: Cell::new(0),
        last_background_log: RefCell::new(String::new()),
        // SAFETY: registers (or looks up) a well-known message name.
        msg_taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        tracking_mouse: Cell::new(false),
        apps_tx,
        apps_rx,
        scanning: Cell::new(false),
        rescan: Cell::new(false),
        _app_watch: ChangeWatch::new(hwnd),
        media,
        config_path,
        _config_watch: config_watch,
        stats_dirty: Cell::new(false),
        hotkey_on: Cell::new(false),
        accent: Cell::new(accent),
        stress: args.stress.map(|(n, gpu)| stress::Stress::new(n, gpu)),
        trim_after_close: Cell::new(false),
        settings_window: RefCell::new(None),
        settings_dirty: Cell::new(false),
        hotkey_paused: Cell::new(false),
    });
    app.apply_background();
    app.register_hotkey();
    // Also fixes the Run value if the exe was moved since it was written.
    // Not with a broken file: its stand-in defaults say "off", which would
    // wrongly remove a Run value the user turned on.
    if config_problem.is_none() {
        autostart::sync(app.config.borrow().autostart);
    }
    if let Some(e) = &config_problem {
        app.config_error(e, true);
    }
    // List every installed app in the background (search needs them).
    app.scan_apps();
    APP.with(|a| *a.borrow_mut() = Some(app));
    log_info!("Glint started (pid {})", std::process::id());
    trim_memory();

    let mut msg = MSG::default();
    // SAFETY: standard message loop with a valid MSG out-pointer.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Shut down: take the app out first, so the window procedure sees `None`
    // while the window is destroyed. Dropping the app removes the tray icon,
    // the settings subscriptions and stops the hook thread.
    let app = APP.with(|a| a.borrow_mut().take());
    // Launches not saved yet (however we were asked to quit).
    if let Some(app) = &app {
        app.save_stats();
        // Unsaved Settings changes, and its window goes before ours.
        app.save_settings();
        drop(app.settings_window.borrow_mut().take());
    }
    // SAFETY: unregisters and destroys our own window.
    unsafe {
        let _ = WTSUnRegisterSessionNotification(hwnd);
        let _ = DestroyWindow(hwnd);
    }
    drop(app);
    log_info!("Glint stopped");
    Ok(())
}

fn main() {
    log::init();
    if let Err(e) = run() {
        log_error!("fatal: {e}");
        std::process::exit(1);
    }
}
