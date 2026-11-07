//! Watches `config.toml` for changes, with no polling.
//!
//! A small helper thread asks Windows (`ReadDirectoryChangesW`) to tell it
//! when something in the Glint folder changes, then sleeps inside the
//! kernel until that happens, so it uses no CPU while idle. When the change
//! touched `config.toml`, it posts [`WM_APP_CONFIG_CHANGED`] to the window.
//! The UI thread then waits for a short quiet moment (editors save in
//! several steps) and reloads the file.
//!
//! The thread waits on two things at once: "the folder changed" and "stop".
//! Dropping [`ConfigWatch`] sets "stop", and the thread cancels its request,
//! waits for Windows to let go of the buffer, and ends.

use std::ffi::c_void;
use std::path::PathBuf;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadDirectoryChangesW,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, Result};

/// Posted to the UI window when `config.toml` may have changed.
pub const WM_APP_CONFIG_CHANGED: u32 = WM_APP + 12;

/// The file we care about (Windows compares names without case).
const FILE_NAME: &str = "config.toml";

/// Change records land here. 4 KB holds dozens of them; if it overflows,
/// Windows reports "too many changes" and we simply reload.
const BUFFER_WORDS: usize = 1024;

/// Owns a kernel handle and closes it when dropped.
struct Handle(HANDLE);

// SAFETY: a kernel handle (folder, event) is just a number that any thread
// of the process may use; it isn't tied to the thread that opened it.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW / CreateEventW and is
        // closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct ConfigWatch {
    stop: Handle,
    thread: Option<JoinHandle<()>>,
}

impl ConfigWatch {
    /// Starts watching the folder `dir` (it must exist). `None` if that
    /// failed; then changes apply only via "Reload config" in the tray.
    pub fn start(hwnd: HWND, dir: PathBuf) -> Option<Self> {
        match Self::try_start(hwnd, dir) {
            Ok(watch) => Some(watch),
            Err(e) => {
                log_warn!("can't watch config.toml for changes: {e}");
                None
            }
        }
    }

    fn try_start(hwnd: HWND, dir: PathBuf) -> Result<Self> {
        // SAFETY: plain object creation; the handles are owned by `Handle`.
        let (folder, changed, stop) = unsafe {
            let folder = Handle(CreateFileW(
                &HSTRING::from(dir.as_os_str()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                // A folder can only be opened with BACKUP_SEMANTICS.
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )?);
            let changed = Handle(CreateEventW(None, true, false, None)?);
            let stop = Handle(CreateEventW(None, true, false, None)?);
            (folder, changed, stop)
        };
        // The thread gets its own copy of the stop event handle's value; the
        // event itself lives until `Drop` has joined the thread.
        let stop_raw = stop.0.0 as isize;
        let target = hwnd.0 as isize;
        let thread = std::thread::Builder::new()
            .name("glint-config-watch".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let stop = HANDLE(stop_raw as *mut c_void);
                if let Err(e) = watch(&folder, &changed, stop, HWND(target as *mut c_void)) {
                    log_warn!("config watch stopped: {e}");
                }
            })
            .map_err(|e| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string())
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ConfigWatch {
    fn drop(&mut self) {
        // SAFETY: signals our own event; the thread is waiting on it.
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread's loop: ask for changes, sleep until they come (or "stop"),
/// tell the window if config.toml was among them, repeat.
fn watch(folder: &Handle, changed: &Handle, stop: HANDLE, hwnd: HWND) -> Result<()> {
    // u32s, because Windows needs the records 4-byte aligned.
    let mut buffer = vec![0u32; BUFFER_WORDS];
    let filter =
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;
    loop {
        let mut overlapped = OVERLAPPED {
            hEvent: changed.0,
            ..Default::default()
        };
        // SAFETY: `buffer` and `overlapped` stay alive and untouched until
        // the request has finished: below we always wait for it to finish
        // (GetOverlappedResult with wait = true), even after cancelling.
        unsafe {
            ReadDirectoryChangesW(
                folder.0,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 4) as u32,
                false,
                filter,
                None,
                Some(&mut overlapped),
                None,
            )?;
        }
        // SAFETY: both handles are live events.
        let woke = unsafe { WaitForMultipleObjects(&[changed.0, stop], false, INFINITE) };
        let mut bytes = 0u32;
        if woke != WAIT_OBJECT_0 {
            // Stop (or the wait failed): cancel, wait until Windows is done
            // with the buffer, end.
            // SAFETY: cancels our own request, then waits for it.
            unsafe {
                let _ = CancelIoEx(folder.0, Some(&overlapped));
                let _ = GetOverlappedResult(folder.0, &overlapped, &mut bytes, true);
            }
            return Ok(());
        }
        // SAFETY: the request finished (its event is set); this only reads
        // the result.
        let finished = unsafe { GetOverlappedResult(folder.0, &overlapped, &mut bytes, true) };
        // SAFETY: resets our own manual-reset event for the next request.
        unsafe {
            let _ = windows::Win32::System::Threading::ResetEvent(changed.0);
        }
        finished?;
        // 0 bytes = the buffer overflowed: something changed, maybe ours.
        if bytes == 0 || touches_config(&buffer, bytes as usize) {
            // SAFETY: posts a plain message (no pointers) to our window.
            let _ =
                unsafe { PostMessageW(Some(hwnd), WM_APP_CONFIG_CHANGED, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// Walks the change records and checks whether any is about config.toml.
fn touches_config(buffer: &[u32], bytes: usize) -> bool {
    let base = buffer.as_ptr().cast::<u8>();
    let len = bytes.min(buffer.len() * 4);
    let mut offset = 0usize;
    let header = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    while offset + header <= len {
        // SAFETY: `offset + header` is within the filled part of the
        // buffer, and Windows puts each record on a 4-byte boundary, so the
        // read is aligned. The name is `FileNameLength` bytes after the
        // header, checked against `len` before reading.
        let (next, name) = unsafe {
            let info = &*base.add(offset).cast::<FILE_NOTIFY_INFORMATION>();
            let name_bytes = info.FileNameLength as usize;
            if offset + header + name_bytes > len {
                return false;
            }
            let name =
                std::slice::from_raw_parts(base.add(offset + header).cast::<u16>(), name_bytes / 2);
            (
                info.NextEntryOffset as usize,
                String::from_utf16_lossy(name),
            )
        };
        if name.eq_ignore_ascii_case(FILE_NAME) {
            return true;
        }
        if next == 0 {
            break;
        }
        offset += next;
    }
    false
}
