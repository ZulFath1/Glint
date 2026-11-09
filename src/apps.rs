//! Apps from `shell:AppsFolder`, the folder that lists every Win32 *and*
//! Store app, each under its AppUserModelID (AUMID).
//!
//! - [`find`] looks up one app (the pinned ones) on the UI thread.
//! - [`scan_in_background`] lists *all* apps on a short-lived thread and
//!   hands the list to the UI thread. Walking the folder takes 100–500 ms,
//!   so it must not block the menu.
//! - [`ChangeWatch`] tells us when apps are installed or uninstalled (the
//!   UI thread then waits 2 s for things to settle and scans again).
//! - [`launch`] starts an app, on its own short-lived thread.

use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, FOLDERID_CommonPrograms, FOLDERID_Programs,
    IEnumShellItems, ILFree, ILGetSize, IShellItem, KF_FLAG_DEFAULT, SHCNE_CREATE, SHCNE_DELETE,
    SHCNE_MKDIR, SHCNE_RENAMEFOLDER, SHCNE_RENAMEITEM, SHCNE_RMDIR, SHCNE_UPDATEDIR,
    SHCNE_UPDATEITEM, SHCNRF_InterruptLevel, SHCNRF_NewDelivery, SHCNRF_RecursiveInterrupt,
    SHCNRF_ShellLevel, SHChangeNotification_Lock, SHChangeNotification_Unlock,
    SHChangeNotifyDeregister, SHChangeNotifyEntry, SHChangeNotifyRegister, SHCreateItemFromIDList,
    SHCreateItemFromParsingName, SHGetIDListFromObject, SHGetKnownFolderIDList,
    SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, PostMessageW, SW_SHOWNORMAL, WM_APP,
};
use windows::core::{HSTRING, Result, w};

/// Posted to the UI window by the scan thread when a new list is ready.
pub const WM_APP_APPS_LOADED: u32 = WM_APP + 7;
/// Posted by the shell when something changed in the watched folders.
pub const WM_APP_APPS_CHANGED: u32 = WM_APP + 8;

/// A pinned app, looked up on the UI thread (it keeps its shell item, for
/// the grid's icon).
pub struct App {
    pub aumid: String,
    pub name: String,
    pub item: IShellItem,
}

/// One installed app, as plain data that can move between threads.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// What goes after `shell:AppsFolder\` to launch it (its AUMID; for some
    /// Win32 apps a path-like ID, which is normal).
    pub id: String,
    pub name: String,
    /// The item's shell address (an "ID list"), copied byte for byte. It
    /// turns back into a shell item quickly, for the icon.
    pidl: Box<[u8]>,
}

impl AppInfo {
    fn from_item(item: &IShellItem) -> Result<Self> {
        Ok(Self {
            id: display_name(item, SIGDN_PARENTRELATIVEPARSING)?,
            name: display_name(item, SIGDN_NORMALDISPLAY)?,
            pidl: copy_id_list(item)?,
        })
    }

    /// The shell item again (for its icon).
    pub fn shell_item(&self) -> Result<IShellItem> {
        // SAFETY: `pidl` is a complete ID list copied from the shell
        // (ending in its 2-byte terminator). ITEMIDLIST is byte-packed, so
        // any alignment is fine. The shell copies what it needs.
        unsafe { SHCreateItemFromIDList(self.pidl.as_ptr().cast::<ITEMIDLIST>()) }
    }
}

impl App {
    pub fn info(&self) -> Option<AppInfo> {
        Some(AppInfo {
            id: self.aumid.clone(),
            name: self.name.clone(),
            pidl: copy_id_list(&self.item).ok()?,
        })
    }
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Result<String> {
    // SAFETY: the shell allocates the string; we copy it and free it.
    unsafe {
        let raw = item.GetDisplayName(kind)?;
        let s = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(s)
    }
}

fn copy_id_list(item: &IShellItem) -> Result<Box<[u8]>> {
    // SAFETY: the shell allocates the ID list; ILGetSize gives its full
    // length (terminator included), we copy exactly that many bytes, then
    // free it.
    unsafe {
        let pidl = SHGetIDListFromObject(item)?;
        let len = ILGetSize(Some(pidl)) as usize;
        let bytes = std::slice::from_raw_parts(pidl.cast::<u8>(), len).into();
        ILFree(Some(pidl));
        Ok(bytes)
    }
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
    // SAFETY: plain shell call; `path` lives across it.
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&path, None)? };
    Ok(App {
        aumid: aumid.to_string(),
        name: display_name(&item, SIGDN_NORMALDISPLAY)?,
        item,
    })
}

/// Lists every app in the AppsFolder. Call with COM initialized.
pub fn enumerate() -> Result<Vec<AppInfo>> {
    // SAFETY: plain shell calls with valid arguments; each returned item is
    // a smart pointer released when dropped.
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let items: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
        let mut apps = Vec::with_capacity(256);
        loop {
            let mut batch: [Option<IShellItem>; 32] = Default::default();
            let mut fetched = 0u32;
            items.Next(&mut batch, Some(&mut fetched))?;
            if fetched == 0 {
                break;
            }
            for item in batch.iter().flatten() {
                match AppInfo::from_item(item) {
                    Ok(app) if !app.id.is_empty() && !app.name.is_empty() => apps.push(app),
                    Ok(_) => {}
                    Err(e) => log_warn!("skipping an app: {e}"),
                }
            }
        }
        // The same app can be listed twice (e.g. per-user and machine
        // shortcuts); keep the first.
        apps.sort_by_cached_key(|a| a.id.to_lowercase());
        apps.dedup_by(|a, b| a.id.eq_ignore_ascii_case(&b.id));
        Ok(apps)
    }
}

/// COM for one helper thread. Single-threaded apartment: what the shell
/// expects for ShellExecute and folder enumeration.
struct ComGuard;

impl ComGuard {
    fn new() -> Option<Self> {
        // SAFETY: initializes COM for the current (new) thread only.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        hr.is_ok().then_some(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful CoInitializeEx in `new`, on the
        // same thread.
        unsafe { CoUninitialize() };
    }
}

/// Lists all apps on a new thread, sends the list through `tx` and tells
/// the window. The thread ends right after.
pub fn scan_in_background(hwnd: HWND, tx: Sender<Vec<AppInfo>>) {
    let target = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("glint-apps".into())
        .spawn(move || {
            let started = std::time::Instant::now();
            let apps = {
                let Some(_com) = ComGuard::new() else {
                    log_error!("app scan: COM init failed");
                    return;
                };
                match enumerate() {
                    Ok(apps) => apps,
                    Err(e) => {
                        log_error!("listing apps failed: {e}");
                        return;
                    }
                }
            };
            log_info!(
                "found {} apps in {} ms",
                apps.len(),
                started.elapsed().as_millis()
            );
            if tx.send(apps).is_ok() {
                // SAFETY: posts a plain message (no pointers) to our window;
                // the data itself travels through the channel.
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(target as *mut _)),
                        WM_APP_APPS_LOADED,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        });
    if let Err(e) = spawned {
        log_error!("could not start the app scan thread: {e}");
    }
}

/// Starts an app by its AppsFolder ID, on a short-lived thread, so a slow
/// start can never freeze the menu. Failures are logged.
pub fn launch(id: String) {
    shell_open(format!("shell:AppsFolder\\{id}"), String::new());
}

/// Opens `target` (a program, `.msc` console, `ms-settings:` page or other
/// URI) with `args` through the shell, on a short-lived thread (the shell
/// may take a moment and must never block the menu).
pub fn shell_open(target: String, args: String) {
    // We still have the foreground (the menu just had the keyboard), so we
    // may pass that right on. Without this, an app that is already running
    // (VS Code, a browser) only hands the job to its open window, and
    // Windows refuses to bring that window to the front: the app seemed not
    // to open at all.
    // SAFETY: a plain Win32 call without pointers.
    if let Err(e) = unsafe { AllowSetForegroundWindow(ASFW_ANY) } {
        log_warn!("could not pass the foreground on: {e}");
    }
    let spawned = std::thread::Builder::new()
        .name("glint-launch".into())
        .spawn(move || {
            let _com = ComGuard::new();
            let file = HSTRING::from(target.as_str());
            let wide = HSTRING::from(args.as_str());
            let params = if args.is_empty() {
                windows::core::PCWSTR::null()
            } else {
                windows::core::PCWSTR(wide.as_ptr())
            };
            // SAFETY: `file` and `wide` (which `params` points into, ending
            // in a 0) live across the call; no window, no pointers kept by
            // the shell.
            let result =
                unsafe { ShellExecuteW(None, w!("open"), &file, params, None, SW_SHOWNORMAL) };
            // ShellExecute returns a value > 32 on success.
            let code = result.0 as isize;
            if code <= 32 {
                log_error!("opening {target} {args} failed (ShellExecute returned {code})");
            } else {
                log_info!("opened {target} {args}");
            }
        });
    if let Err(e) = spawned {
        log_error!("could not start the launch thread: {e}");
    }
}

/// Opens a file with its default app (for config.toml), on a short-lived
/// thread. If no app is set for that file type, it opens in Notepad.
pub fn open_document(path: std::path::PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("glint-open".into())
        .spawn(move || {
            let _com = ComGuard::new();
            let file = HSTRING::from(path.as_os_str());
            // SAFETY: `file` lives across the calls; no window, no pointers
            // kept by the shell.
            let code = unsafe { ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL) };
            // ShellExecute returns a value > 32 on success.
            if code.0 as isize > 32 {
                return;
            }
            log_info!(
                "no app opens {} ({}); using Notepad",
                path.display(),
                code.0 as isize
            );
            let quoted = HSTRING::from(format!("\"{}\"", path.display()));
            // SAFETY: as above.
            let code = unsafe {
                ShellExecuteW(
                    None,
                    w!("open"),
                    w!("notepad.exe"),
                    &quoted,
                    None,
                    SW_SHOWNORMAL,
                )
            };
            if code.0 as isize <= 32 {
                log_error!("opening {} failed ({})", path.display(), code.0 as isize);
            }
        });
    if let Err(e) = spawned {
        log_error!("could not start the open thread: {e}");
    }
}

/// Watches the AppsFolder and both Start-menu "Programs" folders. The shell
/// then posts [`WM_APP_APPS_CHANGED`] to our window when something changes.
/// No polling: the shell tells us.
pub struct ChangeWatch {
    id: u32,
}

impl ChangeWatch {
    pub fn new(hwnd: HWND) -> Option<Self> {
        let folders = [
            (FOLDERID_AppsFolder, false),
            (FOLDERID_Programs, true),
            (FOLDERID_CommonPrograms, true),
        ];
        let mut pidls = Vec::new();
        for (folder, recursive) in folders {
            // SAFETY: the shell allocates the ID list; freed below.
            match unsafe { SHGetKnownFolderIDList(&folder, 0, None) } {
                Ok(pidl) => pidls.push((pidl, recursive)),
                Err(e) => log_warn!("can't watch a folder for app changes: {e}"),
            }
        }
        let entries: Vec<SHChangeNotifyEntry> = pidls
            .iter()
            .map(|&(pidl, recursive)| SHChangeNotifyEntry {
                pidl,
                fRecursive: recursive.into(),
            })
            .collect();
        let events = SHCNE_CREATE
            | SHCNE_DELETE
            | SHCNE_RENAMEITEM
            | SHCNE_RENAMEFOLDER
            | SHCNE_MKDIR
            | SHCNE_RMDIR
            | SHCNE_UPDATEDIR
            | SHCNE_UPDATEITEM;
        // SAFETY: `entries` and the ID lists it points to are alive during
        // the call (the shell copies them). Our window gets the messages.
        let id = unsafe {
            SHChangeNotifyRegister(
                hwnd,
                SHCNRF_ShellLevel
                    | SHCNRF_InterruptLevel
                    | SHCNRF_RecursiveInterrupt
                    | SHCNRF_NewDelivery,
                events.0 as i32,
                WM_APP_APPS_CHANGED,
                entries.len() as i32,
                entries.as_ptr(),
            )
        };
        for (pidl, _) in pidls {
            // SAFETY: frees the ID lists we got above, after the call.
            unsafe { ILFree(Some(pidl)) };
        }
        if id == 0 {
            log_warn!("SHChangeNotifyRegister failed; installs won't show until restart");
            return None;
        }
        Some(Self { id })
    }

    /// Handles one [`WM_APP_APPS_CHANGED`]. With "new delivery" the details
    /// sit in shared memory that must be locked and unlocked; we don't need
    /// them (any change means "scan again soon").
    pub fn acknowledge(wparam: WPARAM, lparam: LPARAM) {
        let mut pidls: *mut *mut ITEMIDLIST = std::ptr::null_mut();
        let mut event = 0i32;
        // SAFETY: wParam/lParam are the handle and process id the shell
        // sent with this message; the out-pointers are locals.
        unsafe {
            let lock = SHChangeNotification_Lock(
                HANDLE(wparam.0 as *mut _),
                lparam.0 as u32,
                Some(&mut pidls),
                Some(&mut event),
            );
            if !lock.is_invalid() {
                let _ = SHChangeNotification_Unlock(lock);
            }
        }
    }
}

impl Drop for ChangeWatch {
    fn drop(&mut self) {
        // SAFETY: removes the registration made in `new`.
        unsafe {
            let _ = SHChangeNotifyDeregister(self.id);
        }
    }
}
