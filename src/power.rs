//! Lock, sleep, restart, shut down (CLAUDE.md section 9; the owner dropped
//! "Sign out" because they never use it).

use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    InitiateShutdownW, LockWorkStation, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
    SHUTDOWN_FLAGS, SHUTDOWN_HYBRID, SHUTDOWN_POWEROFF, SHUTDOWN_RESTART,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
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
