//! Tiny file logger: `%LOCALAPPDATA%\Glint\log.txt`, rotated at 1 MB.
//!
//! Logging is rare (startup, errors), so each line opens the file, appends and
//! closes it again. No handle or buffer stays alive while the app is idle.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Sets up the log folder and a panic hook that writes panics to the log.
pub fn init() {
    let path = std::env::var_os("LOCALAPPDATA").and_then(|dir| {
        let dir = PathBuf::from(dir).join("Glint");
        fs::create_dir_all(&dir).ok()?;
        Some(dir.join("log.txt"))
    });
    let _ = LOG_PATH.set(path);

    std::panic::set_hook(Box::new(|info| {
        write("PANIC", format_args!("{info}"));
    }));
}

pub fn write(level: &str, message: std::fmt::Arguments) {
    #[cfg(debug_assertions)]
    eprintln!("[{level}] {message}");

    let Some(Some(path)) = LOG_PATH.get() else {
        return;
    };
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = fs::rename(path, path.with_extension("old.txt"));
    }
    // SAFETY: GetLocalTime only fills in a SYSTEMTIME it returns by value.
    let t = unsafe { GetLocalTime() };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{level}] {message}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        );
    }
}

macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::write("INFO", format_args!($($arg)*)) };
}

macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::write("WARN", format_args!($($arg)*)) };
}

macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", format_args!($($arg)*)) };
}
