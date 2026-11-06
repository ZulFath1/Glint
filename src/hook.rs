//! Win-key low-level keyboard hook.
//!
//! How it works (CLAUDE.md section 9):
//! 1. Win goes down: swallow it and remember "Win is held".
//! 2. Another key goes down while Win is held: swallow that key and re-send
//!    "Win down + that key down" with `SendInput`, in that order, so Windows
//!    sees a normal Win+key shortcut. From then on everything passes through,
//!    including the Win up.
//! 3. Win goes up with nothing else pressed: swallow it and toggle our menu.
//! 4. Events we injected carry `INJECT_TAG` in `dwExtraInfo` and are ignored.
//!
//! Why the key is re-sent instead of just let through: `SendInput` queues the
//! injected Win down *behind* the key we are currently looking at, so Windows
//! would see "E, then Win" instead of "Win+E".
//!
//! The hook runs on its own thread that does nothing else. Windows silently
//! removes low-level hooks that answer too slowly; a thread that only runs
//! this tiny callback never is. The UI thread can still ask for a reinstall.
//!
//! The decision logic lives in [`WinKeyState`], which has no Windows calls in
//! it, so it can be unit tested.

use std::cell::{Cell, RefCell};
use std::sync::mpsc;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Shutdown::LockWorkStation;
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WM_APP, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEHWHEEL, WM_MOUSEWHEEL, WM_QUIT,
    WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_USER, WM_XBUTTONDOWN,
};

/// Marks input that Glint injected itself ("GLNT" in ASCII).
pub const INJECT_TAG: usize = 0x474C_4E54;

/// Posted to the UI window when Win was tapped alone.
pub const WM_APP_TOGGLE: u32 = WM_APP + 1;

/// Hook-thread messages.
const WM_HOOK_REINSTALL: u32 = WM_APP + 50;

const VK_L: u16 = 0x4C;

/// If Win was "held" but no Win event came for this long, we missed its
/// release (e.g. it happened on the lock screen). A really held Win key keeps
/// auto-repeating, far more often than this.
const STALE_MS: u32 = 1500;

// ---------------------------------------------------------------------------
// Pure decision logic (unit tested at the bottom of this file)
// ---------------------------------------------------------------------------

/// What the hook should do with one keyboard event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Let Windows handle the event normally.
    Pass,
    /// Eat the event.
    Swallow,
    /// Eat the event and toggle the menu (Win was tapped alone).
    Toggle,
    /// Eat this key and re-send `win_vk` down followed by this key down.
    ReplayWithWin { win_vk: u16 },
    /// Win+L: Windows ignores injected Win+L, so lock the PC directly.
    Lock { win_vk: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Win is held and swallowed; nothing else pressed yet.
    Pending { mouse_used: bool },
    /// Another key was pressed; Windows now knows Win is down.
    Combo,
    /// The foreground app runs as admin, which blocks our `SendInput`, so this
    /// press is passed through untouched (the stock Start menu may open).
    Bypass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    vk: u16,
    mode: Mode,
    last_win_time: u32,
}

#[derive(Debug, Default)]
pub struct WinKeyState {
    held: Option<Held>,
}

fn is_win(vk: u16) -> bool {
    vk == VK_LWIN.0 || vk == VK_RWIN.0
}

impl WinKeyState {
    /// True while we need to know about mouse clicks (Win held, swallowed).
    pub fn watching_mouse(&self) -> bool {
        matches!(
            self.held,
            Some(Held {
                mode: Mode::Pending { .. },
                ..
            })
        )
    }

    /// Decide what to do with one key event.
    ///
    /// `time` is the event's timestamp in ms. `foreground_elevated` is only
    /// called on a fresh Win press.
    pub fn on_key(
        &mut self,
        vk: u16,
        down: bool,
        time: u32,
        injected_by_us: bool,
        foreground_elevated: impl FnOnce() -> bool,
    ) -> Decision {
        if injected_by_us {
            return Decision::Pass;
        }

        let Some(held) = self.held else {
            if is_win(vk) && down {
                return self.start(vk, time, foreground_elevated);
            }
            return Decision::Pass;
        };

        if vk == held.vk {
            if down {
                return match held.mode {
                    // Auto-repeat while held alone: keep swallowing.
                    Mode::Pending { .. } => {
                        self.set_time(time);
                        Decision::Swallow
                    }
                    Mode::Bypass => Decision::Pass,
                    // Windows does not resume auto-repeating Win after another
                    // key, so this is a new press whose release we missed.
                    Mode::Combo => self.start(vk, time, foreground_elevated),
                };
            }
            self.held = None;
            return match held.mode {
                Mode::Pending { mouse_used: false } => Decision::Toggle,
                Mode::Pending { mouse_used: true } => Decision::Swallow,
                Mode::Combo | Mode::Bypass => Decision::Pass,
            };
        }

        // Some other key (including the other Win key).
        // Key ups, and anything after the combo started, pass untouched.
        if !down || !matches!(held.mode, Mode::Pending { .. }) {
            return Decision::Pass;
        }
        if time.wrapping_sub(held.last_win_time) > STALE_MS {
            // We missed the Win release; this is just a normal key.
            self.held = None;
            return Decision::Pass;
        }
        self.held = Some(Held {
            mode: Mode::Combo,
            ..held
        });
        if vk == VK_L {
            Decision::Lock { win_vk: held.vk }
        } else {
            Decision::ReplayWithWin { win_vk: held.vk }
        }
    }

    /// A mouse button or wheel was used while Win is held.
    pub fn on_mouse(&mut self) {
        if let Some(Held {
            mode: Mode::Pending { mouse_used },
            ..
        }) = &mut self.held
        {
            *mouse_used = true;
        }
    }

    /// Forget everything (after unlock / resume).
    pub fn reset(&mut self) {
        self.held = None;
    }

    fn start(
        &mut self,
        vk: u16,
        time: u32,
        foreground_elevated: impl FnOnce() -> bool,
    ) -> Decision {
        let bypass = foreground_elevated();
        self.held = Some(Held {
            vk,
            mode: if bypass {
                Mode::Bypass
            } else {
                Mode::Pending { mouse_used: false }
            },
            last_win_time: time,
        });
        if bypass {
            Decision::Pass
        } else {
            Decision::Swallow
        }
    }

    fn set_time(&mut self, time: u32) {
        if let Some(h) = &mut self.held {
            h.last_win_time = time;
        }
    }
}

// ---------------------------------------------------------------------------
// The hook thread
// ---------------------------------------------------------------------------

struct HookContext {
    /// The UI window (as a raw value, because HWND is not `Send`).
    target: Cell<isize>,
    state: RefCell<WinKeyState>,
    keyboard: Cell<Option<HHOOK>>,
    mouse: Cell<Option<HHOOK>>,
}

thread_local! {
    static CTX: HookContext = HookContext {
        target: Cell::new(0),
        state: RefCell::new(WinKeyState::default()),
        keyboard: Cell::new(None),
        mouse: Cell::new(None),
    };
}

/// Handle to the running hook thread.
pub struct HookThread {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl HookThread {
    /// Starts the hook thread. Toggle requests are posted to `target`.
    pub fn start(target: HWND) -> windows::core::Result<Self> {
        let target_raw = target.0 as isize;
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("glint-hook".into())
            .spawn(move || hook_thread_main(target_raw, tx))
            .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;
        let thread_id = rx
            .recv()
            .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))??;
        Ok(Self {
            thread_id,
            handle: Some(handle),
        })
    }

    /// Re-install the hook (in case Windows removed it). `reset` also forgets
    /// a Win press we may have half-seen (use after unlock / resume).
    pub fn reinstall(&self, reset: bool) {
        // SAFETY: posting a message to a thread id we own; no pointers passed.
        let result = unsafe {
            PostThreadMessageW(
                self.thread_id,
                WM_HOOK_REINSTALL,
                WPARAM(usize::from(reset)),
                LPARAM(0),
            )
        };
        if let Err(e) = result {
            log_warn!("could not ask the hook thread to reinstall: {e}");
        }
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own hook thread; no pointers passed.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn hook_thread_main(target: isize, ready: mpsc::Sender<windows::core::Result<u32>>) {
    let mut msg = MSG::default();
    // SAFETY: PeekMessageW with a valid MSG out-pointer. This forces Windows to
    // create this thread's message queue, so PostThreadMessageW works at once.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    CTX.with(|c| c.target.set(target));

    let installed = install_keyboard_hook();
    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let ok = installed.is_ok();
    let _ = ready.send(installed.map(|()| thread_id));
    if !ok {
        return;
    }

    loop {
        // SAFETY: standard message loop with a valid MSG out-pointer.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break; // WM_QUIT or error
        }
        if msg.message == WM_HOOK_REINSTALL {
            if msg.wParam.0 != 0 {
                CTX.with(|c| c.state.borrow_mut().reset());
                remove_mouse_hook();
            }
            remove_keyboard_hook();
            if let Err(e) = install_keyboard_hook() {
                log_error!("keyboard hook reinstall failed: {e}");
            }
        }
    }

    remove_mouse_hook();
    remove_keyboard_hook();
}

fn install_keyboard_hook() -> windows::core::Result<()> {
    // SAFETY: installs a global low-level hook whose callback is a plain
    // function in this binary; the module handle is our own exe.
    let hook = unsafe {
        let module = GetModuleHandleW(None)?;
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(module.into()), 0)?
    };
    CTX.with(|c| c.keyboard.set(Some(hook)));
    Ok(())
}

fn remove_keyboard_hook() {
    if let Some(hook) = CTX.with(|c| c.keyboard.take()) {
        // SAFETY: the handle came from SetWindowsHookExW on this thread.
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    }
}

/// The mouse hook only exists while Win is held alone, so normal mouse
/// movement never wakes Glint up.
fn install_mouse_hook() {
    if CTX.with(|c| c.mouse.get()).is_some() {
        return;
    }
    // SAFETY: same as install_keyboard_hook.
    let hook = unsafe {
        GetModuleHandleW(None)
            .and_then(|m| SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(m.into()), 0))
    };
    match hook {
        Ok(h) => CTX.with(|c| c.mouse.set(Some(h))),
        Err(e) => log_warn!("mouse hook failed: {e}"),
    }
}

fn remove_mouse_hook() {
    if let Some(hook) = CTX.with(|c| c.mouse.take()) {
        // SAFETY: the handle came from SetWindowsHookExW on this thread.
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    }
}

extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT that is
        // valid for the duration of this call (WH_KEYBOARD_LL contract).
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        let vk = kb.vkCode as u16;

        let (decision, watch_mouse) = CTX.with(|c| {
            let mut state = c.state.borrow_mut();
            let decision = state.on_key(
                vk,
                down,
                kb.time,
                kb.dwExtraInfo == INJECT_TAG,
                foreground_is_elevated,
            );
            (decision, state.watching_mouse())
        });

        if watch_mouse {
            install_mouse_hook();
        } else {
            remove_mouse_hook();
        }

        match decision {
            Decision::Pass => {}
            Decision::Swallow => return LRESULT(1),
            Decision::Toggle => {
                post_toggle();
                return LRESULT(1);
            }
            Decision::ReplayWithWin { win_vk } => {
                let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
                let inputs = [
                    win_input(win_vk, false),
                    key_input(vk, kb.scanCode as u16, extended, false),
                ];
                if send(&inputs) {
                    return LRESULT(1);
                }
                // Injection failed: at least let the key itself through.
            }
            Decision::Lock { win_vk } => {
                // Tell Windows Win is down so the later real Win up matches.
                send(&[win_input(win_vk, false)]);
                // SAFETY: LockWorkStation has no preconditions; it is async.
                if let Err(e) = unsafe { LockWorkStation() } {
                    log_warn!("LockWorkStation failed: {e}");
                }
                return LRESULT(1);
            }
        }
    }
    // SAFETY: forwarding the unchanged arguments to the next hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let message = wparam.0 as u32;
        if matches!(
            message,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        ) {
            CTX.with(|c| c.state.borrow_mut().on_mouse());
        }
    }
    // SAFETY: forwarding the unchanged arguments to the next hook.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn post_toggle() {
    let target = HWND(CTX.with(|c| c.target.get()) as *mut _);
    // SAFETY: posting a plain message (no pointers) to our own UI window.
    if let Err(e) = unsafe { PostMessageW(Some(target), WM_APP_TOGGLE, WPARAM(0), LPARAM(0)) } {
        log_error!("could not post toggle: {e}");
    }
}

fn win_input(vk: u16, up: bool) -> INPUT {
    // SAFETY: MapVirtualKeyW is a pure lookup.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    key_input(vk, scan, true, up)
}

fn key_input(vk: u16, scan: u16, extended: bool, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_TAG,
            },
        },
    }
}

/// Injects tagged input. Returns false if Windows refused some of it.
pub fn send(inputs: &[INPUT]) -> bool {
    // SAFETY: `inputs` is a valid slice of fully initialized INPUT structs and
    // the size argument matches the struct size.
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}

/// True if the foreground window belongs to an elevated (admin) process.
/// Windows blocks our `SendInput` into such windows (UIPI), so Win shortcuts
/// would silently stop working there. If we can't even inspect the process,
/// assume it is elevated: passing the key through is the safe choice.
fn foreground_is_elevated() -> bool {
    // SAFETY: all calls get valid out-pointers; both handles are closed below.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return false;
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true;
        };
        let mut token = HANDLE::default();
        let elevated = if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok() {
            let mut info = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut info as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            !ok || info.TokenIsElevated != 0
        } else {
            true
        };
        let _ = CloseHandle(process);
        elevated
    }
}

// ---------------------------------------------------------------------------
// Tests: combo pass-through
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const LWIN: u16 = 0x5B;
    const RWIN: u16 = 0x5C;
    const E: u16 = 0x45;
    const SHIFT: u16 = 0xA0;
    const S: u16 = 0x53;

    /// Feeds (vk, down, time) events through the state machine.
    fn run(events: &[(u16, bool, u32)]) -> Vec<Decision> {
        let mut s = WinKeyState::default();
        events
            .iter()
            .map(|&(vk, down, t)| s.on_key(vk, down, t, false, || false))
            .collect()
    }

    use Decision::*;

    #[test]
    fn win_tap_toggles() {
        assert_eq!(
            run(&[(LWIN, true, 0), (LWIN, false, 80)]),
            [Swallow, Toggle]
        );
        assert_eq!(
            run(&[(RWIN, true, 0), (RWIN, false, 80)]),
            [Swallow, Toggle]
        );
    }

    #[test]
    fn win_e_replays_win_then_passes_everything() {
        assert_eq!(
            run(&[
                (LWIN, true, 0),
                (E, true, 50),
                (E, false, 90),
                (LWIN, false, 120)
            ]),
            [Swallow, ReplayWithWin { win_vk: LWIN }, Pass, Pass]
        );
    }

    #[test]
    fn win_shift_s_replays_on_first_extra_key() {
        assert_eq!(
            run(&[
                (LWIN, true, 0),
                (SHIFT, true, 30),
                (S, true, 60),
                (S, false, 90),
                (SHIFT, false, 100),
                (LWIN, false, 120),
            ]),
            [
                Swallow,
                ReplayWithWin { win_vk: LWIN },
                Pass,
                Pass,
                Pass,
                Pass
            ]
        );
    }

    #[test]
    fn win_l_locks() {
        assert_eq!(
            run(&[(LWIN, true, 0), (VK_L, true, 50)]),
            [Swallow, Lock { win_vk: LWIN }]
        );
    }

    #[test]
    fn long_hold_with_auto_repeat_still_toggles() {
        let mut events = vec![(LWIN, true, 0)];
        for t in (500..3000).step_by(33) {
            events.push((LWIN, true, t));
        }
        events.push((LWIN, false, 3010));
        let out = run(&events);
        assert!(out[..out.len() - 1].iter().all(|d| *d == Swallow));
        assert_eq!(*out.last().unwrap(), Toggle);
    }

    #[test]
    fn long_hold_then_combo_still_works() {
        assert_eq!(
            run(&[
                (LWIN, true, 0),
                (LWIN, true, 500),
                (LWIN, true, 1000),
                (E, true, 1100)
            ]),
            [Swallow, Swallow, Swallow, ReplayWithWin { win_vk: LWIN }]
        );
    }

    #[test]
    fn fast_double_tap_toggles_twice() {
        assert_eq!(
            run(&[
                (LWIN, true, 0),
                (LWIN, false, 40),
                (LWIN, true, 70),
                (LWIN, false, 110)
            ]),
            [Swallow, Toggle, Swallow, Toggle]
        );
    }

    #[test]
    fn mouse_click_while_held_cancels_toggle() {
        let mut s = WinKeyState::default();
        assert_eq!(s.on_key(LWIN, true, 0, false, || false), Swallow);
        assert!(s.watching_mouse());
        s.on_mouse();
        assert_eq!(s.on_key(LWIN, false, 200, false, || false), Swallow);
        assert!(!s.watching_mouse());
    }

    #[test]
    fn our_injected_events_are_ignored() {
        let mut s = WinKeyState::default();
        assert_eq!(s.on_key(LWIN, true, 0, true, || false), Pass);
        assert_eq!(s.on_key(LWIN, false, 10, true, || false), Pass);
        assert!(!s.watching_mouse());
    }

    #[test]
    fn missed_release_does_not_hijack_later_keys() {
        // Win down, then (release missed), then much later the user types E.
        assert_eq!(run(&[(LWIN, true, 0), (E, true, 10_000)]), [Swallow, Pass]);
    }

    #[test]
    fn new_press_after_missed_release_in_combo() {
        // Win+E, release missed (e.g. lock screen), then a fresh Win tap.
        assert_eq!(
            run(&[
                (LWIN, true, 0),
                (E, true, 50),
                (LWIN, true, 60_000),
                (LWIN, false, 60_080)
            ]),
            [Swallow, ReplayWithWin { win_vk: LWIN }, Swallow, Toggle]
        );
    }

    #[test]
    fn elevated_foreground_passes_through() {
        let mut s = WinKeyState::default();
        assert_eq!(s.on_key(LWIN, true, 0, false, || true), Pass);
        assert_eq!(s.on_key(LWIN, true, 500, false, || true), Pass);
        assert_eq!(s.on_key(E, true, 520, false, || true), Pass);
        assert_eq!(s.on_key(LWIN, false, 600, false, || true), Pass);
        // Next press is normal again.
        assert_eq!(s.on_key(LWIN, true, 1000, false, || false), Swallow);
    }

    #[test]
    fn keys_without_win_pass() {
        assert_eq!(run(&[(E, true, 0), (E, false, 50)]), [Pass, Pass]);
        // A stray Win up (we never saw the down) passes.
        assert_eq!(run(&[(LWIN, false, 0)]), [Pass]);
    }
}
