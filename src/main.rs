//! Glint: a tiny, smooth Start menu for Windows 11.
//!
//! Startup order: single-instance check → DPI awareness → WinRT + dispatcher
//! queue → hidden window → composition tree → tray icon → keyboard hook →
//! message loop.

// Release builds have no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod log;
mod hook;
mod tray;
mod ui;
mod window;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use windows::System::DispatcherQueueController;
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
    PostQuitMessage, RegisterWindowMessageW, SC_KEYMENU, TranslateMessage, WA_INACTIVE,
    WM_ACTIVATE, WM_CLOSE, WM_KEYDOWN, WM_LBUTTONUP, WM_POWERBROADCAST, WM_RBUTTONUP,
    WM_SYSCOMMAND, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK,
};
use windows::core::{Result, w};
use windows_numerics::Vector2;

use hook::{HookThread, WM_APP_TOGGLE};
use tray::{Tray, TrayCommand, WM_APP_TRAY};

/// Milestone 1 placeholder size of the menu (DIPs).
const MENU_SIZE: Vector2 = Vector2 { X: 360.0, Y: 320.0 };

/// A tray click right after the menu closed (because the click took focus
/// away) should not reopen it.
const REOPEN_GUARD_MS: u64 = 400;

/// Everything the UI thread owns. Lives in `APP` for the window procedure.
struct App {
    hwnd: HWND,
    ui: ui::Ui,
    tray: Tray,
    hook: HookThread,
    is_open: Cell<bool>,
    closed_at: Cell<u64>,
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

impl App {
    fn toggle(&self) {
        if self.is_open.get() {
            self.close();
        } else {
            self.open();
        }
    }

    fn open(&self) {
        let placement = window::placement(MENU_SIZE.X, MENU_SIZE.Y);
        if let Err(e) = self.ui.set_scale(placement.scale) {
            log_error!("set_scale failed: {e}");
        }
        self.is_open.set(true);
        window::show(self.hwnd, &placement);
        window::take_focus(self.hwnd);
    }

    fn close(&self) {
        if !self.is_open.replace(false) {
            return;
        }
        window::hide(self.hwnd);
        self.closed_at.set(now_ms());
        // Cheap insurance: if Windows silently dropped our hook, this puts it
        // back. (The tray icon still opens the menu if the hook is gone.)
        self.hook.reinstall(false);
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
                if let Some(TrayCommand::Quit) = self.tray.show_menu() {
                    // SAFETY: posts WM_QUIT to this (the UI) thread.
                    unsafe { PostQuitMessage(0) };
                }
            }
            _ => {}
        }
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
            WM_ACTIVATE if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE => app.close(),
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
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                app.hook.reinstall(true)
            }
            WM_POWERBROADCAST if wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC => {
                app.hook.reinstall(true)
            }
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

    let hwnd = window::create(Some(wndproc))?;
    let ui = ui::Ui::new(hwnd, MENU_SIZE)?;
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
        is_open: Cell::new(false),
        closed_at: Cell::new(0),
        // SAFETY: registers (or looks up) a well-known message name.
        msg_taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
    });
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
    // while the window is destroyed. Dropping the app removes the tray icon
    // and stops the hook thread.
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
