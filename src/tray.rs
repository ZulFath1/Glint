//! Notification-area (tray) icon with a right-click menu, and small
//! non-blocking notices (balloons / toasts) such as "config.toml has an
//! error on line 12".

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{Result, w};

/// Tray events arrive at the window as this message.
pub const WM_APP_TRAY: u32 = WM_APP + 2;

const ICON_ID: u32 = 1;
const CMD_QUIT: usize = 1;
const CMD_OPEN_CONFIG: usize = 2;
const CMD_RELOAD_CONFIG: usize = 3;

pub enum TrayCommand {
    Quit,
    OpenConfig,
    ReloadConfig,
}

pub struct Tray {
    hwnd: HWND,
}

/// Copies `text` into a fixed-size UTF-16 field, cut short (with "…") if
/// it doesn't fit, always leaving room for the terminating zero.
fn fill(field: &mut [u16], text: &str) {
    let max = field.len() - 1;
    let mut units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > max {
        units.truncate(max - 1);
        units.push('…' as u16);
    }
    field[..units.len()].copy_from_slice(&units);
    field[units.len()] = 0;
}

impl Tray {
    pub fn add(hwnd: HWND) -> Result<Self> {
        let tray = Self { hwnd };
        tray.register()?;
        Ok(tray)
    }

    /// Adds the icon. Also called when Explorer restarts ("TaskbarCreated").
    pub fn register(&self) -> Result<()> {
        let data = self.data()?;
        // SAFETY: `data` is a fully initialized NOTIFYICONDATAW for our window.
        unsafe { Shell_NotifyIconW(NIM_ADD, &data).ok() }
    }

    fn data(&self) -> Result<NOTIFYICONDATAW> {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            // SAFETY: loads a shared system icon; shared icons must not be
            // destroyed, so there is nothing to free.
            hIcon: unsafe { LoadIconW(None, IDI_APPLICATION)? },
            ..Default::default()
        };
        fill(&mut data.szTip, "Glint");
        Ok(data)
    }

    /// Shows a small notice next to the tray icon (a Windows notification).
    /// It never blocks: Windows shows and hides it on its own.
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let Ok(mut data) = self.data() else {
            return;
        };
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        fill(&mut data.szInfoTitle, title);
        fill(&mut data.szInfo, text);
        // SAFETY: `data` is a fully initialized NOTIFYICONDATAW for our icon.
        if let Err(e) = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data).ok() } {
            log_warn!("tray notice failed: {e}");
        }
    }

    /// Shows the right-click menu at the cursor and returns the chosen item.
    pub fn show_menu(&self) -> Option<TrayCommand> {
        // SAFETY: creates, shows and destroys a menu owned by this function.
        // The labels are static strings. The SetForegroundWindow / WM_NULL
        // dance is the documented way to make tray menus close when the user
        // clicks elsewhere.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_CONFIG, w!("Open config file"));
            let _ = AppendMenuW(menu, MF_STRING, CMD_RELOAD_CONFIG, w!("Reload config"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit Glint"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                pt.x,
                pt.y,
                None,
                self.hwnd,
                None,
            );
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            match chosen.0 as usize {
                CMD_QUIT => Some(TrayCommand::Quit),
                CMD_OPEN_CONFIG => Some(TrayCommand::OpenConfig),
                CMD_RELOAD_CONFIG => Some(TrayCommand::ReloadConfig),
                _ => None,
            }
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Ok(data) = self.data() {
            // SAFETY: removes the icon we added; `data` identifies it.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn long_notice_text_is_cut_with_an_ellipsis() {
        let mut field = [1u16; 8];
        fill(&mut field, "short");
        assert_eq!(
            &field[..6],
            &"short\0".encode_utf16().collect::<Vec<_>>()[..]
        );
        fill(&mut field, "much too long");
        assert_eq!(field[7], 0);
        assert_eq!(field[6], '…' as u16);
        assert_eq!(String::from_utf16_lossy(&field[..6]), "much t");
    }
}
