//! "Start with Windows": one value named `Glint` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, holding the quoted
//! path of this exe. Windows starts everything listed there when the user
//! signs in. Only our own value is ever touched.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Glint");

/// What the `Run` value should hold: this exe's path, in quotes (the path
/// may contain spaces).
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// The `Run` value as it is now, if there is one.
fn current() -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the buffer and its byte size are valid; RRF_RT_REG_SZ makes
    // Windows return a terminated string (or an error).
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE_NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = (bytes as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Makes the registry match `on`. Cheap when nothing needs to change, so it
/// can run at every startup and config reload. With `on`, it also fixes a
/// value that points at an old location of the exe.
pub fn sync(on: bool) {
    let now = current();
    if on {
        let Some(want) = command() else {
            log_warn!("autostart: can't find this exe's path");
            return;
        };
        if now.as_deref() == Some(want.as_str()) {
            return;
        }
        let data = HSTRING::from(want.as_str());
        // SAFETY: `data` is a live, zero-terminated UTF-16 string; the byte
        // count includes the terminating zero, as REG_SZ requires.
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE_NAME,
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                ((data.len() + 1) * 2) as u32,
            )
        };
        match status.ok() {
            Ok(()) => log_info!("autostart on: {want}"),
            Err(e) => log_error!("autostart: can't write the Run key: {e}"),
        }
    } else if now.is_some() {
        // SAFETY: deletes only our own value; the names are static strings.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE_NAME) };
        if status == ERROR_FILE_NOT_FOUND || status == WIN32_ERROR(0) {
            log_info!("autostart off");
        } else if let Err(e) = status.ok() {
            log_error!("autostart: can't remove the Run value: {e}");
        }
    }
}
