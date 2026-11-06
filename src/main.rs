//! Glint: a tiny, smooth Start menu for Windows 11.
//!
//! Startup order: single-instance check → DPI awareness → WinRT + dispatcher
//! queue → hidden window → composition tree → tray icon → keyboard hook →
//! message loop.

// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod log;
mod config;
mod hook;
mod settings_watch;
mod tray;
mod ui;
mod window;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

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
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
    RO_INIT_SINGLETHREADED, RoInitialize,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG, PBT_APMRESUMEAUTOMATIC,
    PBT_POWERSETTINGCHANGE, PostMessageW, PostQuitMessage, RegisterWindowMessageW, SC_KEYMENU,
    TranslateMessage, WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_POWERBROADCAST, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SETTINGCHANGE, WM_SYSCOMMAND, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK,
};
use windows::core::{Result, w};
use windows_numerics::Vector2;

use config::Config;
use hook::{HookThread, WM_APP_TOGGLE};
use settings_watch::{SettingsWatch, WM_APP_SETTINGS_CHANGED};
use tray::{Tray, TrayCommand, WM_APP_TRAY};
use window::Placement;

/// Placeholder size of the menu until the content arrives (DIPs).
const MENU_SIZE: Vector2 = Vector2 { X: 360.0, Y: 320.0 };

/// Posted when a close animation finished; wParam = its generation.
const WM_APP_CLOSE_DONE: u32 = WM_APP + 4;

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

    fn open(&self) {
        self.generation.set(self.generation.get() + 1);
        if !self.is_visible.get() {
            // Fully hidden: pick the monitor and position fresh. (If we are
            // interrupting a close, stay where we are and just reverse.)
            let p = window::placement(MENU_SIZE.X, MENU_SIZE.Y);
            log_err("layout", self.ui.set_layout(&p));
            log_err("reset", self.ui.reset_closed());
            window::show(self.hwnd, &p);
            self.placement.set(Some(p));
            self.is_visible.set(true);
        }
        self.is_open.set(true);
        self.opened_at.set(now_ms());
        log_err(
            "open animation",
            self.ui.animate_open(self.reduced_motion()),
        );
        window::take_focus(self.hwnd);
    }

    fn close(&self) {
        if !self.is_open.replace(false) {
            return;
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.closed_at.set(now_ms());

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
        let p = window::placement(MENU_SIZE.X, MENU_SIZE.Y);
        if self.placement.get() != Some(p) {
            log_err("layout", self.ui.set_layout(&p));
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
                    "background: {} / {} (blur: {}, overlay {:.2}; transparency effects {}, battery saver {})",
                    bg.mode.name(),
                    bg.preset.name(),
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
                let (mode, preset) = {
                    let c = self.config.borrow();
                    (c.background.mode, c.background.preset)
                };
                match self.tray.show_menu(mode, preset) {
                    Some(TrayCommand::Quit) => {
                        // SAFETY: posts WM_QUIT to this (the UI) thread.
                        unsafe { PostQuitMessage(0) };
                    }
                    Some(TrayCommand::SetMode(m)) => {
                        self.config.borrow_mut().background.mode = m;
                        self.apply_background();
                    }
                    Some(TrayCommand::SetPreset(p)) => {
                        self.config.borrow_mut().background.preset = p;
                        self.apply_background();
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// A click on the transparent shadow margin counts as "outside".
    fn on_click(&self, lparam: LPARAM) {
        let x = i32::from((lparam.0 & 0xFFFF) as u16 as i16);
        let y = i32::from(((lparam.0 >> 16) & 0xFFFF) as u16 as i16);
        if let Some(p) = self.placement.get()
            && !p.menu_contains(x, y)
        {
            self.close();
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
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => app.on_click(lparam),
            // Esc closes the menu (milestone 3: only when the search is empty).
            WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
                app.close();
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
    let ui = ui::Ui::new(hwnd, MENU_SIZE, &config)?;
    let settings = SettingsWatch::new(hwnd)?;
    let tray = Tray::add(hwnd)?;
    let hook = HookThread::start(hwnd)?;

    // SAFETY: registers our own window for lock/unlock notifications.
    if let Err(e) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
        log_warn!("WTSRegisterSessionNotification failed: {e}");
    }

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
    });
    app.apply_background();
    APP.with(|a| *a.borrow_mut() = Some(app));
    log_info!("Glint started (pid {})", std::process::id());

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
