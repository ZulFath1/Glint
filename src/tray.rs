//! Notification-area (tray) icon with a right-click menu.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_STRING,
    PostMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{Result, w};

/// Tray events arrive at the window as this message.
pub const WM_APP_TRAY: u32 = WM_APP + 2;

const ICON_ID: u32 = 1;
const CMD_QUIT: usize = 1;

pub enum TrayCommand {
    Quit,
}

pub struct Tray {
    hwnd: HWND,
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
        let tip: Vec<u16> = "Glint".encode_utf16().collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Ok(data)
    }

    /// Shows the right-click menu at the cursor and returns the chosen item.
    pub fn show_menu(&self) -> Option<TrayCommand> {
        // SAFETY: creates, shows and destroys a menu owned by this function.
        // The SetForegroundWindow / WM_NULL dance is the documented way to
        // make tray menus close when the user clicks elsewhere.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
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
            (chosen.0 as usize == CMD_QUIT).then_some(TrayCommand::Quit)
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
