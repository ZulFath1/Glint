//! Notification-area (tray) icon with a right-click menu.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, IDI_APPLICATION, LoadIconW,
    MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::config::{BackgroundMode, Preset, TINT_STEPS};

/// Tray events arrive at the window as this message.
pub const WM_APP_TRAY: u32 = WM_APP + 2;

const ICON_ID: u32 = 1;
const CMD_QUIT: usize = 1;
const CMD_MODE_BASE: usize = 100;
const CMD_PRESET_BASE: usize = 200;
const CMD_TINT_BASE: usize = 300;

pub enum TrayCommand {
    Quit,
    SetMode(BackgroundMode),
    SetPreset(Preset),
    SetTint(f32),
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
    /// The Background/Preset submenus are a testing convenience until the
    /// config file arrives (milestone 6).
    pub fn show_menu(
        &self,
        mode: BackgroundMode,
        preset: Preset,
        tint: f32,
    ) -> Option<TrayCommand> {
        // SAFETY: creates, shows and destroys menus owned by this function
        // (destroying the main menu also destroys its submenus). Item labels
        // are HSTRINGs that outlive the AppendMenuW calls. The
        // SetForegroundWindow / WM_NULL dance is the documented way to make
        // tray menus close when the user clicks elsewhere.
        unsafe {
            let menu = CreatePopupMenu().ok()?;
            let add = |m: HMENU, id: usize, label: &str, checked: bool| {
                let label = HSTRING::from(label);
                let flags = if checked {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let _ = AppendMenuW(m, flags, id, PCWSTR(label.as_ptr()));
            };

            if let Ok(modes) = CreatePopupMenu() {
                for (i, m) in BackgroundMode::ALL.into_iter().enumerate() {
                    add(modes, CMD_MODE_BASE + i, m.name(), m == mode);
                }
                let _ = AppendMenuW(menu, MF_POPUP, modes.0 as usize, w!("Background"));
            }
            if let Ok(presets) = CreatePopupMenu() {
                for (i, p) in Preset::BUILT_IN.into_iter().enumerate() {
                    add(presets, CMD_PRESET_BASE + i, p.name(), p == preset);
                }
                let _ = AppendMenuW(menu, MF_POPUP, presets.0 as usize, w!("Preset"));
            }
            if let Ok(tints) = CreatePopupMenu() {
                for (i, &t) in TINT_STEPS.iter().enumerate() {
                    let label = if t == 0.0 {
                        "0% (colourless glass)".to_string()
                    } else {
                        format!("{:.0}%", t * 100.0)
                    };
                    add(tints, CMD_TINT_BASE + i, &label, (t - tint).abs() < 0.01);
                }
                let _ = AppendMenuW(menu, MF_POPUP, tints.0 as usize, w!("Glass tint"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            add(menu, CMD_QUIT, "Quit Glint", false);

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
            command_for(chosen.0 as usize)
        }
    }
}

fn command_for(id: usize) -> Option<TrayCommand> {
    if id == CMD_QUIT {
        return Some(TrayCommand::Quit);
    }
    if let Some(&m) = id
        .checked_sub(CMD_MODE_BASE)
        .and_then(|i| BackgroundMode::ALL.get(i))
    {
        return Some(TrayCommand::SetMode(m));
    }
    id.checked_sub(CMD_PRESET_BASE)
        .and_then(|i| Preset::BUILT_IN.get(i))
        .map(|&p| TrayCommand::SetPreset(p))
        .or_else(|| {
            id.checked_sub(CMD_TINT_BASE)
                .and_then(|i| TINT_STEPS.get(i))
                .map(|&t| TrayCommand::SetTint(t))
        })
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
