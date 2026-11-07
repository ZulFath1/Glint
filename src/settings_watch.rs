//! Watches the Windows settings that change how the menu looks or moves:
//! "Transparency effects", battery saver and "Animation effects", plus the
//! Windows accent color (for `accent = "system"`).
//!
//! Event-driven only: Windows tells us when something changes, and we post a
//! message to the UI window, which re-applies the background.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use windows::Foundation::TypedEventHandler;
use windows::UI::ViewManagement::{UIColorType, UISettings};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};

use windows::Win32::System::Power::{
    EFFECTIVE_POWER_MODE, EFFECTIVE_POWER_MODE_V2, EffectivePowerModeBatterySaver,
    GetSystemPowerStatus, HPOWERNOTIFY, PowerRegisterForEffectivePowerModeNotifications,
    PowerUnregisterFromEffectivePowerModeNotifications, RegisterPowerSettingNotification,
    SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::GUID_POWER_SAVING_STATUS;
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_WINDOW_HANDLE, PostMessageW, WM_APP};
use windows::core::Result;

use crate::config::Rgb;
use crate::ui::background::SystemEffects;

/// Posted to the UI window when transparency or battery saver changed.
pub const WM_APP_SETTINGS_CHANGED: u32 = WM_APP + 3;

pub struct SettingsWatch {
    ui_settings: UISettings,
    effects_token: i64,
    power_notify: Option<HPOWERNOTIFY>,
    power_mode_handle: *mut c_void,
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

        // Energy saver: Windows calls us back right away with the current
        // mode, then on every change.
        let mut power_mode_handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: the callback is a plain function in this binary; the
        // context is our window handle (only ever posted to); the out-pointer
        // is a valid local.
        let registered = unsafe {
            PowerRegisterForEffectivePowerModeNotifications(
                EFFECTIVE_POWER_MODE_V2,
                Some(power_mode_changed),
                Some(hwnd.0 as *const c_void),
                &mut power_mode_handle,
            )
        };
        if let Err(e) = registered {
            log_warn!("energy saver notifications unavailable: {e}");
            power_mode_handle = std::ptr::null_mut();
        }

        Ok(Self {
            ui_settings,
            effects_token,
            power_notify,
            power_mode_handle,
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

    /// The Windows accent color, in the lighter shade Windows itself uses
    /// for accent text on dark backgrounds (readable on our dark menu).
    /// For `accent = "system"`. Re-read on WM_SETTINGCHANGE.
    pub fn accent(&self) -> Option<Rgb> {
        self.ui_settings
            .GetColorValue(UIColorType::AccentLight2)
            .inspect_err(|e| log_warn!("can't read the Windows accent color: {e}"))
            .ok()
            .map(|c| Rgb::new(c.R, c.G, c.B))
    }
}

/// The last "effective power mode" Windows reported (-1 = not known yet).
static POWER_MODE: AtomicI32 = AtomicI32::new(-1);

/// Called by Windows (on a worker thread) right after registering and on
/// every change. `context` is our window handle.
extern "system" fn power_mode_changed(mode: EFFECTIVE_POWER_MODE, context: *const c_void) {
    if POWER_MODE.swap(mode.0, Ordering::Relaxed) != mode.0 {
        log_info!(
            "effective power mode: {} (energy saver {})",
            mode.0,
            if mode == EffectivePowerModeBatterySaver {
                "on"
            } else {
                "off"
            }
        );
        // SAFETY: posts a plain message (no pointers) to our window.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(context as *mut _)),
                WM_APP_SETTINGS_CHANGED,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

/// Energy saver (Windows 11) or the older battery saver.
///
/// Windows 11's Energy saver also works while plugged in, and then the old
/// battery-saver APIs report "off". The effective power mode sees it.
fn battery_saver_on() -> bool {
    if POWER_MODE.load(Ordering::Relaxed) == EffectivePowerModeBatterySaver.0 {
        return true;
    }
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
        if !self.power_mode_handle.is_null() {
            // SAFETY: the handle came from
            // PowerRegisterForEffectivePowerModeNotifications.
            let _ = unsafe {
                PowerUnregisterFromEffectivePowerModeNotifications(self.power_mode_handle)
            };
        }
    }
}
