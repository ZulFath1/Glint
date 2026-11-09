//! Lock, sleep, restart, shut down (CLAUDE.md section 9; the owner dropped
//! "Sign out" from the power strip because they never use it). Sign out and
//! emptying the Recycle Bin are only reachable from the search box.

use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    EWX_LOGOFF, ExitWindowsEx, InitiateShutdownW, LockWorkStation, SHTDN_REASON_FLAG_PLANNED,
    SHTDN_REASON_MAJOR_OTHER, SHUTDOWN_FLAGS, SHUTDOWN_HYBRID, SHUTDOWN_POWEROFF, SHUTDOWN_RESTART,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW,
};
use windows::core::PCWSTR;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Sleep,
    Restart,
    ShutDown,
}

impl PowerAction {
    /// In the order of the power strip's buttons.
    pub const ALL: [Self; 4] = [Self::Lock, Self::Sleep, Self::Restart, Self::ShutDown];

    pub fn label(self) -> &'static str {
        match self {
            Self::Lock => "Lock",
            Self::Sleep => "Sleep",
            Self::Restart => "Restart",
            Self::ShutDown => "Shut down",
        }
    }

    /// The icon, a character of the Segoe Fluent Icons font.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Lock => "\u{E72E}",
            Self::Sleep => "\u{E708}",
            Self::Restart => "\u{E777}",
            Self::ShutDown => "\u{E7E8}",
        }
    }

    /// Restart and Shut down ask "Are you sure?" first (inside the strip).
    pub fn needs_confirm(self) -> bool {
        matches!(self, Self::Restart | Self::ShutDown)
    }

    pub fn question(self) -> &'static str {
        match self {
            Self::Restart => "Restart now?",
            Self::ShutDown => "Shut down now?",
            _ => "",
        }
    }
}

/// Does it. Errors are logged (there is no one else to tell).
pub fn run(action: PowerAction) {
    log_info!("power: {action:?}");
    // SAFETY: plain Win32 calls without pointers (apart from the privilege
    // helper, see there).
    let result = unsafe {
        match action {
            PowerAction::Lock => LockWorkStation(),
            PowerAction::Sleep => {
                if SetSuspendState(false, false, false) {
                    Ok(())
                } else {
                    Err(windows::core::Error::from_thread())
                }
            }
            PowerAction::Restart => shutdown(SHUTDOWN_RESTART),
            // Like the stock Start menu: a "hybrid" shutdown (fast startup),
            // or a normal one if fast startup is not available.
            PowerAction::ShutDown => shutdown(SHUTDOWN_POWEROFF | SHUTDOWN_HYBRID)
                .or_else(|_| shutdown(SHUTDOWN_POWEROFF)),
        }
    };
    if let Err(e) = result {
        log_error!("power action {action:?} failed: {e}");
    }
}

/// Signs out of Windows (from the search: "sign out", after a second
/// Enter). Apps get the usual chance to save.
pub fn sign_out() {
    log_info!("power: sign out");
    // SAFETY: a plain Win32 call without pointers.
    let result = unsafe { ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_MAJOR_OTHER) };
    if let Err(e) = result {
        log_error!("signing out failed: {e}");
    }
}

/// Empties the Recycle Bin, without Windows' own "Are you sure?" (the
/// search already asked) and without the progress window. A full bin can
/// take a while, so it runs on a short-lived thread.
pub fn empty_recycle_bin() {
    let spawned = std::thread::Builder::new()
        .name("glint-recycle".into())
        .spawn(|| {
            let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
            // SAFETY: no window, all drives (null root path).
            match unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), flags) } {
                Ok(()) => log_info!("Recycle Bin emptied"),
                // Also what an already empty bin returns.
                Err(e) => log_info!("emptying the Recycle Bin: {e}"),
            }
        });
    if let Err(e) = spawned {
        log_error!("could not start the Recycle Bin thread: {e}");
    }
}

/// Restarting or shutting down needs the shutdown privilege switched on in
/// our own process token (every user has it, it's just off by default).
unsafe fn shutdown(flags: SHUTDOWN_FLAGS) -> windows::core::Result<()> {
    // SAFETY: the caller is `run`; see `enable_shutdown_privilege`.
    unsafe {
        enable_shutdown_privilege()?;
        let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
        let error = InitiateShutdownW(PCWSTR::null(), PCWSTR::null(), 0, flags, reason);
        if error == 0 {
            Ok(())
        } else {
            Err(windows::core::HRESULT::from_win32(error).into())
        }
    }
}

unsafe fn enable_shutdown_privilege() -> windows::core::Result<()> {
    // SAFETY: opens our own process token (closed below) and passes pointers
    // to locals that live across each call.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )?;
        let mut luid = LUID::default();
        let result =
            LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid).and_then(|()| {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
            });
        let _ = CloseHandle(token);
        result
    }
}
