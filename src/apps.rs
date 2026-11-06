//! Apps from `shell:AppsFolder`, the folder that lists every Win32 *and*
//! Store app, each under its AppUserModelID (AUMID).
//!
//! Milestone 3 only needs to look up the pinned apps. Listing all apps,
//! launching and watching for installs arrive in milestone 4.

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};
use windows::core::{HSTRING, Result};

pub struct App {
    pub aumid: String,
    pub name: String,
    pub item: IShellItem,
}

/// Finds an app by its AUMID. `None` if it isn't installed (any more):
/// uninstalled pins are skipped silently (CLAUDE.md section 9).
pub fn find(aumid: &str) -> Option<App> {
    match lookup(aumid) {
        Ok(app) => Some(app),
        Err(e) => {
            log_info!("pinned app not found, skipping: {aumid} ({e})");
            None
        }
    }
}

fn lookup(aumid: &str) -> Result<App> {
    let path = HSTRING::from(format!("shell:AppsFolder\\{aumid}"));
    // SAFETY: plain shell calls; `path` lives across the call. The name
    // string is allocated by the shell and freed right after copying it.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&path, None)?;
        let raw = item.GetDisplayName(SIGDN_NORMALDISPLAY)?;
        let name = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(App {
            aumid: aumid.to_string(),
            name,
            item,
        })
    }
}
