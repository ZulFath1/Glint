//! The popup window: creation, placement above the taskbar, DPI, focus.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetCursorPos, GetForegroundWindow, HWND_TOPMOST, IDC_ARROW, LoadCursorW,
    RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos,
    ShowWindow, WINDOW_EX_STYLE, WNDCLASSW, WNDPROC, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Result, w};

use crate::hook;

/// Space around the menu for the drop shadow (DIPs).
pub const SHADOW_MARGIN: f32 = 32.0;
/// Gap between the menu and the taskbar (DIPs).
const TASKBAR_GAP: f32 = 12.0;

/// Which screen edge the taskbar is on. The menu grows out of that edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

/// Where and how big the window should be for the next open.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Window rectangle in physical pixels (menu + shadow margin, cut off
    /// where the taskbar starts so the window never covers it).
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The menu's rectangle inside the window, in physical pixels.
    pub menu_x: i32,
    pub menu_y: i32,
    pub menu_width: i32,
    pub menu_height: i32,
    /// DPI scale (1.0 = 100%, 1.5 = 150%, ...).
    pub scale: f32,
    /// The open animation grows out of this edge.
    pub edge: Edge,
}

impl Placement {
    /// True if a point in window (client) coordinates is on the menu itself
    /// rather than on the transparent shadow margin around it.
    pub fn menu_contains(&self, x: i32, y: i32) -> bool {
        (self.menu_x..self.menu_x + self.menu_width).contains(&x)
            && (self.menu_y..self.menu_y + self.menu_height).contains(&y)
    }
}

pub fn create(wndproc: WNDPROC) -> Result<HWND> {
    let class_name = w!("GlintMenu");
    // SAFETY: registers a class and creates a window with valid, static
    // arguments. The class name string is 'static.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: instance.into(),
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        // NOREDIRECTIONBITMAP: only our composition visuals draw in it.
        // TOOLWINDOW: no taskbar button. TOPMOST: above normal windows.
        let ex_style: WINDOW_EX_STYLE =
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        CreateWindowExW(
            ex_style,
            class_name,
            w!("Glint"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

/// Works out where the menu goes: on the cursor's monitor, centered on the
/// taskbar's edge, `TASKBAR_GAP` away from it.
pub fn placement(menu_width: f32, menu_height: f32) -> Placement {
    let mut cursor = POINT::default();
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: all calls get valid out-pointers to locals.
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    compute_placement(
        info.rcMonitor,
        info.rcWork,
        dpi_x as f32 / 96.0,
        menu_width,
        menu_height,
    )
}

fn compute_placement(monitor: RECT, work: RECT, scale: f32, menu_w: f32, menu_h: f32) -> Placement {
    let px = |dip: f32| (dip * scale).round() as i32;
    let (mw, mh, margin, gap) = (px(menu_w), px(menu_h), px(SHADOW_MARGIN), px(TASKBAR_GAP));

    let edge = if work.bottom < monitor.bottom {
        Edge::Bottom
    } else if work.top > monitor.top {
        Edge::Top
    } else if work.left > monitor.left {
        Edge::Left
    } else if work.right < monitor.right {
        Edge::Right
    } else {
        Edge::Bottom // auto-hide taskbar: assume the default position
    };

    let center_x = work.left + (work.right - work.left - mw) / 2;
    let (menu_x, menu_y) = match edge {
        Edge::Bottom => (center_x, work.bottom - gap - mh),
        Edge::Top => (center_x, work.top + gap),
        // Vertical taskbars keep the Start button at the top.
        Edge::Left => (work.left + gap, work.top + gap),
        Edge::Right => (work.right - gap - mw, work.top + gap),
    };

    // Menu + shadow margin, but never over the taskbar (so taskbar clicks
    // still work while the menu is open).
    let left = (menu_x - margin).max(work.left);
    let top = (menu_y - margin).max(work.top);
    let right = (menu_x + mw + margin).min(work.right);
    let bottom = (menu_y + mh + margin).min(work.bottom);

    Placement {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
        menu_x: menu_x - left,
        menu_y: menu_y - top,
        menu_width: mw,
        menu_height: mh,
        scale,
        edge,
    }
}

pub fn show(hwnd: HWND, p: &Placement) {
    // SAFETY: moves/shows our own window; no pointers involved.
    let result = unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            p.x,
            p.y,
            p.width,
            p.height,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        )
    };
    if let Err(e) = result {
        log_error!("SetWindowPos failed: {e}");
    }
}

pub fn hide(hwnd: HWND) {
    // SAFETY: hides our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

/// Makes our window the foreground window so it gets the keyboard.
///
/// Windows only lets a process take the foreground in some cases. Right after
/// the user's key press it usually works; if not, a synthetic Alt press makes
/// Windows treat us as the process that got the last input.
pub fn take_focus(hwnd: HWND) -> bool {
    // SAFETY: plain Win32 calls on our own window handle.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            let _ = SetFocus(Some(hwnd));
            return true;
        }
        let alt_down = alt_input(false);
        let alt_up = alt_input(true);
        hook::send(&[alt_down]);
        let _ = SetForegroundWindow(hwnd);
        hook::send(&[alt_up]);
        let _ = SetFocus(Some(hwnd));
        let ok = GetForegroundWindow() == hwnd;
        if !ok {
            log_warn!("could not take the foreground, even with the Alt workaround");
        }
        ok
    }
}

/// The exe name of the process that owns `hwnd` (for diagnostics).
pub fn process_name(hwnd: HWND) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    use windows::core::PWSTR;

    if hwnd.is_invalid() {
        return "(no window)".into();
    }
    let mut pid = 0u32;
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    // SAFETY: valid out-pointers to locals; the handle is closed below.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return format!("pid {pid}");
        };
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(process);
        if !ok {
            return format!("pid {pid}");
        }
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().unwrap_or(&path).to_string()
}

/// The window class name of `hwnd` (for diagnostics).
pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: valid buffer; GetClassNameW writes at most buf.len() chars.
    let len = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn alt_input(up: bool) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: hook::INJECT_TAG,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn bottom_taskbar_at_100_percent() {
        let p = compute_placement(
            rect(0, 0, 1920, 1080),
            rect(0, 0, 1920, 1032),
            1.0,
            360.0,
            320.0,
        );
        assert_eq!(p.edge, Edge::Bottom);
        // Menu bottom = 1032 - 12 = 1020. The window stops at the taskbar.
        assert_eq!(p.y + p.menu_y + p.menu_height, 1020);
        assert_eq!(p.y + p.height, 1032);
        assert_eq!(p.width, 360 + 64);
        assert_eq!(p.x + p.width / 2, 960);
        assert_eq!((p.menu_x, p.menu_y), (32, 32));
        assert!(p.menu_contains(32, 32) && !p.menu_contains(10, 100));
    }

    #[test]
    fn bottom_taskbar_at_150_percent() {
        let p = compute_placement(
            rect(0, 0, 2880, 1620),
            rect(0, 0, 2880, 1548),
            1.5,
            360.0,
            320.0,
        );
        assert_eq!(p.width, 540 + 96);
        assert_eq!(p.y + p.menu_y + p.menu_height, 1548 - 18);
        assert_eq!(p.y + p.height, 1548);
    }

    #[test]
    fn top_and_side_taskbars() {
        let m = rect(0, 0, 1920, 1080);
        assert_eq!(
            compute_placement(m, rect(0, 48, 1920, 1080), 1.0, 360.0, 320.0).edge,
            Edge::Top
        );
        let left = compute_placement(m, rect(60, 0, 1920, 1080), 1.0, 360.0, 320.0);
        assert_eq!(left.edge, Edge::Left);
        // The window never reaches into the taskbar on the left.
        assert_eq!(left.x, 60);
        assert_eq!(left.x + left.menu_x, 60 + 12);
        assert_eq!(
            compute_placement(m, rect(0, 0, 1860, 1080), 1.0, 360.0, 320.0).edge,
            Edge::Right
        );
    }
}
