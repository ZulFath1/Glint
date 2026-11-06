//! Watches the Windows settings that change how the menu looks or moves:
//! "Transparency effects", battery saver and "Animation effects".
//!
//! Event-driven only: Windows tells us when something changes, and we post a
//! message to the UI window, which re-applies the background.

use windows::Foundation::TypedEventHandler;
use windows::UI::ViewManagement::UISettings;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, HPOWERNOTIFY, RegisterPowerSettingNotification, SYSTEM_POWER_STATUS,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::GUID_POWER_SAVING_STATUS;
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_WINDOW_HANDLE, PostMessageW, WM_APP};
use windows::core::Result;

use crate::ui::background::SystemEffects;

/// Posted to the UI window when transparency or battery saver changed.
pub const WM_APP_SETTINGS_CHANGED: u32 = WM_APP + 3;

pub struct SettingsWatch {
    ui_settings: UISettings,
    effects_token: i64,
    power_notify: Option<HPOWERNOTIFY>,
}

impl SettingsWatch {
    pub fn new(hwnd: HWND) -> Result<Self> {
        let ui_settings = UISettings::new()?;

        // UISettings events arrive on a worker thread; just post a message.
        let target = hwnd.0 as isize;
        let effects_token =
            ui_settings.AdvancedEffectsEnabledChanged(&TypedEventHandler::new(move |_, _| {
                // SAFETY: posts a plain message (no pointers) to our window.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        WM_APP_SETTINGS_CHANGED,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                Ok(())
            }))?;

        // Battery saver changes arrive as WM_POWERBROADCAST /
        // PBT_POWERSETTINGCHANGE on our window.
        // SAFETY: registers our own window; the GUID is a static constant.
        let power_notify = unsafe {
            RegisterPowerSettingNotification(
                HANDLE(hwnd.0),
                &GUID_POWER_SAVING_STATUS,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            )
        }
        .inspect_err(|e| log_warn!("battery saver notifications unavailable: {e}"))
        .ok();

        Ok(Self {
            ui_settings,
            effects_token,
            power_notify,
        })
    }

    pub fn effects(&self) -> SystemEffects {
        SystemEffects {
            transparency: self.ui_settings.AdvancedEffectsEnabled().unwrap_or(true),
            battery_saver: battery_saver_on(),
        }
    }

    /// Windows "Animation effects" setting. Off = reduced motion.
    pub fn animations_enabled(&self) -> bool {
        self.ui_settings.AnimationsEnabled().unwrap_or(true)
    }
}

fn battery_saver_on() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid out-pointer to a local struct.
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.SystemStatusFlag == 1
}

impl Drop for SettingsWatch {
    fn drop(&mut self) {
        // Remove what we subscribed to (re-subscribing without removing is
        // the classic leak).
        let _ = self
            .ui_settings
            .RemoveAdvancedEffectsEnabledChanged(self.effects_token);
        if let Some(handle) = self.power_notify.take() {
            // SAFETY: the handle came from RegisterPowerSettingNotification.
            let _ = unsafe { UnregisterPowerSettingNotification(handle) };
        }
    }
}
