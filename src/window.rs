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

/// Space around the menu so the open overshoot (the little bounce past
/// full size) is never cut off by the window edge (DIPs).
pub const MARGIN: f32 = 32.0;
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
    /// Window rectangle in physical pixels (menu + margin, cut off
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
    /// The window is made for the tallest menu (`menu_height`). A shorter
    /// one (`height` DIPs) sits against the taskbar: with the taskbar at the
    /// bottom it is pushed down to the bottom of that space. Returns the
    /// top edge in window pixels.
    pub fn menu_top(&self, height: f32) -> i32 {
        match self.edge {
            Edge::Bottom => self.menu_y + self.menu_height - (height * self.scale).round() as i32,
            _ => self.menu_y,
        }
    }

    /// True if a point in window (client) coordinates is on the menu itself
    /// (`height` DIPs tall) rather than on the transparent space around it.
    pub fn menu_contains(&self, x: i32, y: i32, height: f32) -> bool {
        let top = self.menu_top(height);
        let bottom = top + (height * self.scale).round() as i32;
        (self.menu_x..self.menu_x + self.menu_width).contains(&x) && (top..bottom).contains(&y)
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
        let hwnd = CreateWindowExW(
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
        )?;
        allow_host_backdrop(hwnd);
        Ok(hwnd)
    }
}

/// A plain Win32 window must opt in before DWM fills a HostBackdropBrush
/// with the blurred view of what's behind it. Without this the "glass" is
/// just a dark, empty panel.
fn allow_host_backdrop(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute};
    let enable: i32 = 1; // Win32 BOOL
    // SAFETY: passes a pointer to a local 4-byte BOOL with its exact size.
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&raw const enable).cast(),
            std::mem::size_of::<i32>() as u32,
        )
    };
    if let Err(e) = result {
        log_warn!("DWMWA_USE_HOSTBACKDROPBRUSH failed (glass will look flat): {e}");
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
    let mut work = info.rcWork;
    if let Some(edge) = fake_taskbar() {
        // `--taskbar-edge`: pretend a 48 px taskbar sits on that edge.
        let thickness = (48.0 * dpi_x as f32 / 96.0).round() as i32;
        work = reserve_taskbar(info.rcMonitor, edge, thickness);
    }
    if work == info.rcMonitor
        && let Some((edge, thickness)) = auto_hide_taskbar()
    {
        work = reserve_taskbar(work, edge, thickness);
    }
    compute_placement(
        info.rcMonitor,
        work,
        dpi_x as f32 / 96.0,
        menu_width,
        menu_height,
    )
}

/// Debug flag `--taskbar-edge top|left|right|bottom`: 0 = off, else the
/// edge + 1. Windows 11 only puts the taskbar at the bottom, so this lets the
/// other edges be seen without a taskbar mod.
static FAKE_EDGE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const EDGES: [Edge; 4] = [Edge::Bottom, Edge::Top, Edge::Left, Edge::Right];

pub fn set_fake_taskbar(edge: Edge) {
    let i = EDGES.iter().position(|e| *e == edge).unwrap_or(0) as u8;
    FAKE_EDGE.store(i + 1, std::sync::atomic::Ordering::Relaxed);
}

fn fake_taskbar() -> Option<Edge> {
    match FAKE_EDGE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        n => EDGES.get(usize::from(n - 1)).copied(),
    }
}

impl Edge {
    /// "bottom" / "top" / "left" / "right" (for `--taskbar-edge`).
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bottom" => Some(Self::Bottom),
            "top" => Some(Self::Top),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

/// With an auto-hiding taskbar, Windows reports a work area that covers the
/// whole screen. Then we ask where the taskbar is and how thick it is.
/// Returns None if the taskbar is not set to auto-hide.
fn auto_hide_taskbar() -> Option<(Edge, i32)> {
    use windows::Win32::UI::Shell::{
        ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_GETSTATE, ABM_GETTASKBARPOS, ABS_AUTOHIDE,
        APPBARDATA, SHAppBarMessage,
    };
    let mut data = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    // SAFETY: `data` is a valid, correctly sized APPBARDATA.
    let state = unsafe { SHAppBarMessage(ABM_GETSTATE, &mut data) };
    if state as u32 & ABS_AUTOHIDE == 0 {
        return None;
    }
    // SAFETY: as above. The rect is the taskbar's full size even while it is
    // slid out of view, so its short side is the thickness.
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } == 0 {
        return None;
    }
    let r = data.rc;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let edge = match data.uEdge {
        ABE_TOP => Edge::Top,
        ABE_LEFT => Edge::Left,
        ABE_RIGHT => Edge::Right,
        ABE_BOTTOM => Edge::Bottom,
        _ => Edge::Bottom,
    };
    let thickness = if matches!(edge, Edge::Top | Edge::Bottom) {
        h
    } else {
        w
    };
    Some((edge, thickness))
}

/// Shrinks the work area by the taskbar's thickness on its edge.
fn reserve_taskbar(work: RECT, edge: Edge, thickness: i32) -> RECT {
    let mut r = work;
    match edge {
        Edge::Bottom => r.bottom -= thickness,
        Edge::Top => r.top += thickness,
        Edge::Left => r.left += thickness,
        Edge::Right => r.right -= thickness,
    }
    r
}

fn compute_placement(monitor: RECT, work: RECT, scale: f32, menu_w: f32, menu_h: f32) -> Placement {
    let px = |dip: f32| (dip * scale).round() as i32;
    let (mw, mh, margin, gap) = (px(menu_w), px(menu_h), px(MARGIN), px(TASKBAR_GAP));

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

    // Menu + margin, but never over the taskbar (so taskbar clicks
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
        assert!(p.menu_contains(32, 32, 320.0) && !p.menu_contains(10, 100, 320.0));
        // A shorter menu (e.g. 3 search results instead of 8) keeps its
        // bottom edge above the taskbar; the space above it is not "menu".
        assert_eq!(p.menu_top(200.0), 32 + 120);
        assert!(!p.menu_contains(100, 100, 200.0) && p.menu_contains(100, 160, 200.0));
    }

    #[test]
    fn shorter_menus_stay_at_the_top_with_a_top_taskbar() {
        let p = compute_placement(
            rect(0, 0, 1920, 1080),
            rect(0, 48, 1920, 1080),
            1.0,
            360.0,
            320.0,
        );
        assert_eq!(p.edge, Edge::Top);
        assert_eq!(p.menu_top(200.0), p.menu_top(320.0));
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
    fn auto_hide_taskbar_keeps_the_menu_above_it() {
        // Auto-hide: Windows says the work area is the whole 1920x1200 screen.
        let m = rect(0, 0, 1920, 1200);
        let work = reserve_taskbar(m, Edge::Bottom, 48);
        let p = compute_placement(m, work, 1.0, 360.0, 320.0);
        assert_eq!(p.edge, Edge::Bottom);
        assert_eq!(p.y + p.menu_y + p.menu_height, 1200 - 48 - 12);
        assert_eq!(reserve_taskbar(m, Edge::Left, 60).left, 60);
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

    /// Every edge at every common scale: the window stays inside the work
    /// area (never over the taskbar), and the menu keeps the 12 DIP gap on
    /// the taskbar's side.
    #[test]
    fn every_edge_and_scale_stays_off_the_taskbar() {
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let px = |dip: f32| (dip * scale).round() as i32;
            let m = rect(0, 0, px(1920.0), px(1080.0));
            let bar = px(48.0);
            for edge in [Edge::Bottom, Edge::Top, Edge::Left, Edge::Right] {
                let work = reserve_taskbar(m, edge, bar);
                let p = compute_placement(m, work, scale, 360.0, 420.0);
                assert_eq!(p.edge, edge, "{edge:?} at {scale}");
                assert!(p.x >= work.left && p.y >= work.top, "{edge:?} at {scale}");
                assert!(p.x + p.width <= work.right, "{edge:?} at {scale}");
                assert!(p.y + p.height <= work.bottom, "{edge:?} at {scale}");
                let (mx, my) = (p.x + p.menu_x, p.y + p.menu_y);
                let gap = px(12.0);
                match edge {
                    Edge::Bottom => assert_eq!(work.bottom - (my + p.menu_height), gap),
                    Edge::Top => assert_eq!(my - work.top, gap),
                    Edge::Left => assert_eq!(mx - work.left, gap),
                    Edge::Right => assert_eq!(work.right - (mx + p.menu_width), gap),
                }
                assert_eq!(p.menu_width, px(360.0));
                assert_eq!(p.menu_height, px(420.0));
            }
        }
    }

    #[test]
    fn second_monitor_left_of_the_main_one() {
        // A 125% monitor placed left of the main screen has negative x.
        let m = rect(-2400, 0, 0, 1350);
        let work = rect(-2400, 0, 0, 1290);
        let p = compute_placement(m, work, 1.25, 360.0, 320.0);
        assert_eq!(p.edge, Edge::Bottom);
        assert_eq!(p.x + p.width / 2, -1200);
        assert!(p.x >= -2400 && p.x + p.width <= 0);
        assert_eq!(p.y + p.menu_y + p.menu_height, 1290 - 15);
    }

    #[test]
    fn taskbar_edge_names() {
        assert_eq!(Edge::parse("Top"), Some(Edge::Top));
        assert_eq!(Edge::parse("left"), Some(Edge::Left));
        assert_eq!(Edge::parse("RIGHT"), Some(Edge::Right));
        assert_eq!(Edge::parse("bottom"), Some(Edge::Bottom));
        assert_eq!(Edge::parse("middle"), None);
    }
}
