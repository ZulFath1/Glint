//! Glint: a tiny, smooth Start menu for Windows 11.
//!
//! Startup order: single-instance check → DPI awareness → WinRT + dispatcher
//! queue → hidden window → composition tree (and painting the content) →
//! tray icon → keyboard hook → message loop.

// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod log;
mod apps;
mod config;
mod hook;
mod icons;
mod media;
mod power;
mod search;
mod settings_watch;
mod tray;
mod ui;
mod window;

use std::cell::{Cell, RefCell};
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
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_SHIFT,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, HTCLIENT,
    IDC_IBEAM, KillTimer, LoadCursorW, MSG, PBT_APMRESUMEAUTOMATIC, PBT_POWERSETTINGCHANGE,
    PostMessageW, PostQuitMessage, RegisterWindowMessageW, SC_KEYMENU, SW_SHOWNORMAL, SetCursor,
    SetTimer, TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CHAR, WM_CLOSE, WM_DPICHANGED,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_POWERBROADCAST,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETTINGCHANGE, WM_SYSCOMMAND, WM_TIMER,
    WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK,
};
use windows::core::{Result, w};

use apps::{AppInfo, ChangeWatch, WM_APP_APPS_CHANGED, WM_APP_APPS_LOADED};
use config::Config;
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
}

/// WM_TIMER id: an app was installed or removed; wait until things settle.
const TIMER_APPS_SETTLE: usize = 1;
/// Installers create and delete many files; scan only after 2 s of quiet.
const APPS_SETTLE_MS: u32 = 2000;

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
        let size = self.ui.max_size();
        window::placement(size.X, size.Y)
    }

    fn open(&self) {
        self.generation.set(self.generation.get() + 1);
        let fresh = !self.is_visible.get();
        if fresh {
            // Fully hidden: empty search and the grid back, then pick the
            // monitor and position fresh. (If we are interrupting a close,
            // stay where we are and just reverse.)
            log_err("reset", self.ui.reset_closed());
            let p = self.placement();
            log_err("layout", self.ui.set_layout(&p));
            // Covers are decoded for this monitor's DPI.
            self.media.set_cover_px(self.ui.cover_px());
            window::show(self.hwnd, &p);
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
        if !window::take_focus(self.hwnd) {
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
            }
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

    /// Shows the current music state on the card.
    fn push_music(&self) {
        log_err("music card", self.ui.set_music(self.media.view()));
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
    }

    /// Re-place the open menu, e.g. after a DPI change.
    fn reposition(&self) {
        if !self.is_open.get() {
            return;
        }
        let p = self.placement();
        if self.placement.get() != Some(p) {
            log_err("layout", self.ui.set_layout(&p));
            self.media.set_cover_px(self.ui.cover_px());
            window::show(self.hwnd, &p);
            self.placement.set(Some(p));
        }
    }

    fn apply_background(&self) {
        let effects = self.settings.effects();
        let config = self.config.borrow();
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
                );
                // Only log real changes (WM_SETTINGCHANGE can be chatty).
                if *self.last_background_log.borrow() != line {
                    log_info!("{line}");
                    *self.last_background_log.borrow_mut() = line;
                }
            }
            Err(e) => log_error!("applying the background failed: {e}"),
        }
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
                let (mode, preset, tint) = {
                    let c = &self.config.borrow().background;
                    (c.mode, c.preset, c.tint)
                };
                let Some(command) = self.tray.show_menu(mode, preset, tint) else {
                    return;
                };
                {
                    let bg = &mut self.config.borrow_mut().background;
                    match command {
                        TrayCommand::Quit => {
                            // SAFETY: posts WM_QUIT to this (the UI) thread.
                            unsafe { PostQuitMessage(0) };
                            return;
                        }
                        TrayCommand::SetMode(m) => bg.mode = m,
                        TrayCommand::SetPreset(p) => bg.preset = p,
                        TrayCommand::SetTint(t) => bg.tint = t,
                    }
                }
                // Show the new look right away.
                self.apply_background();
                self.open();
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
            self.close();
        } else if msg == WM_LBUTTONDOWN {
            self.run(self.ui.mouse_down(x, y, key_held(VK_SHIFT)));
        }
    }

    /// Another window was activated: close. If that happens right after we
    /// opened, the user didn't do it, so log who took the focus.
    fn on_deactivate(&self, lparam: LPARAM) {
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
            WM_APP_TOGGLE => {
                app.toggle();
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
            WM_APP_SETTINGS_CHANGED | WM_SETTINGCHANGE => app.apply_background(),
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
                return LRESULT(0);
            }
            // Alt+F4 must hide the menu, never destroy the window.
            WM_CLOSE => {
                app.close();
                return LRESULT(0);
            }
            // We size the window ourselves, so ignore the suggested rect.
            WM_DPICHANGED => {
                app.reposition();
                return LRESULT(0);
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                app.hook.reinstall(true)
            }
            WM_POWERBROADCAST => match wparam.0 as u32 {
                PBT_APMRESUMEAUTOMATIC => app.hook.reinstall(true),
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

/// Startup touches a lot of memory only once: creating the GPU device,
/// loading fonts, asking the shell for icons. Measured (milestone 3): about
/// 21 MB right after startup, but an open/close cycle afterwards needs only
/// about 5 MB. Handing the rest back to Windows keeps idle RAM low.
/// The same after each app scan (see `App::on_apps_loaded`).
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

fn run() -> Result<()> {
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

    let config = Config::from_args(std::env::args().skip(1));
    let hwnd = window::create(Some(wndproc))?;
    // Paint the content now, at the DPI of the monitor it will most likely
    // open on, so the first open is instant.
    let scale = window::placement(1.0, 1.0).scale;
    let ui = ui::Ui::new(hwnd, &config, scale)?;
    let settings = SettingsWatch::new(hwnd)?;
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
    });
    app.apply_background();
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
