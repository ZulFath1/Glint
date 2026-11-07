//! The leak tests (CLAUDE.md section 11).
//!
//! `glint.exe --stress N` opens and closes the menu N times. Each cycle also
//! types in search, shows or hides a (fake) music card, and switches the
//! background mode; every 25 cycles it changes the pins, every 50 it
//! re-reads config.toml. It compares memory at the end with memory after
//! cycle 10 (the rule in CLAUDE.md) and after cycle 100 ("warm", see
//! `WARM_CYCLE`): the private working set (Task Manager's number) must not
//! grow by 1 MB or more. Then Glint quits.
//!
//! `glint.exe --stress-gpu N` does the same plus a (simulated) GPU reset in
//! every cycle, so even a tiny per-reset leak would add up N times. Each
//! reset is followed by a memory trim (see main.rs), which empties the
//! working set, so this one is judged by private bytes (everything
//! allocated, in RAM or not). The GPU driver keeps a pool of its own, so
//! its number moves a little either way.
//!
//! The steps run through the normal message loop (the close animation
//! finishes on its own time), driven by `WM_APP_STRESS` and a timer; see
//! `App::stress_*` in main.rs. config.toml is never written.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX, QueryWorkingSet,
};
use windows::Win32::System::Threading::{
    GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources, GetProcessHandleCount,
};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

use crate::config::{BackgroundMode, Rgb};
use crate::media::{Cover, Info, MusicView};

/// Posted to the UI window: start the next cycle.
pub const WM_APP_STRESS: u32 = WM_APP + 13;
/// WM_TIMER id: the open animation has played; type, then close.
pub const TIMER_STRESS: usize = 6;

/// Memory is compared against the value after this cycle (CLAUDE.md
/// section 11: "RAM after 1,000 cycles within 1 MB of RAM after 10").
const BASELINE_CYCLE: u32 = 10;
/// A second, "warm" comparison from here: by cycle 100 every step (pins,
/// config re-read, every background mode, both card sizes) has run at least
/// once. Until then the working set is still filling back up after the
/// startup trim, which looks like growth but is not a leak.
const WARM_CYCLE: u32 = 100;
/// Also logged every this many cycles, to see a trend.
const REPORT_EVERY: u32 = 100;
/// The pass mark: growth must stay under this.
const MAX_GROWTH: usize = 1024 * 1024;
const PAGE: usize = 4096;
/// Bit 8 of a working-set entry: the page is shared with other processes.
const SHARED_BIT: usize = 1 << 8;

/// One memory reading.
#[derive(Debug, Clone, Copy)]
pub struct Mem {
    /// Task Manager's "Memory" column: working-set pages not shared with
    /// any other process.
    pub private_ws: usize,
    /// Private bytes (commit): everything we allocated, in RAM or not.
    pub private_bytes: usize,
    pub handles: u32,
    pub gdi: u32,
    pub user: u32,
}

impl std::fmt::Display for Mem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "private working set {:.2} MB, private bytes {:.2} MB, handles {}, GDI {}, USER {}",
            mb(self.private_ws),
            mb(self.private_bytes),
            self.handles,
            self.gdi,
            self.user
        )
    }
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// What a cycle should do besides open, type and close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub background: BackgroundMode,
    /// Show the fake music card this cycle (otherwise hide it).
    pub music: bool,
    /// The card with 2 sources (adds the "Switch source" row).
    pub two_sources: bool,
    /// Take the last pin away (`Some(false)`) or put it back (`Some(true)`).
    pub pin: Option<bool>,
    pub reload_config: bool,
    pub device_loss: bool,
}

/// What happens in cycle `n` (1-based); `gpu` = `--stress-gpu`. Pure, so it
/// can be tested.
pub fn plan(n: u32, gpu: bool) -> Plan {
    Plan {
        background: BackgroundMode::ALL[n as usize % BackgroundMode::ALL.len()],
        music: n.is_multiple_of(2),
        two_sources: n.is_multiple_of(4),
        pin: match n % 50 {
            0 => Some(false),
            25 => Some(true),
            _ => None,
        },
        // Between putting the pin back (25) and taking it (50), so the file
        // and the menu agree on the pins when it is re-read.
        reload_config: n % 50 == 40,
        device_loss: gpu,
    }
}

pub struct Stress {
    pub total: u32,
    /// `--stress-gpu`: a GPU reset every cycle, judged by private bytes.
    gpu: bool,
    cycle: Cell<u32>,
    started: Cell<bool>,
    start_time: Cell<Option<Instant>>,
    first: Cell<Option<Mem>>,
    baseline: Cell<Option<Mem>>,
    warm: Cell<Option<Mem>>,
    /// Reused for every reading, so measuring doesn't itself allocate.
    buffer: RefCell<Vec<usize>>,
    /// The pin taken away (to put it back 25 cycles later).
    pub parked_pin: RefCell<Option<String>>,
}

impl Stress {
    pub fn new(total: u32, gpu: bool) -> Self {
        Self {
            total: total.max(1),
            gpu,
            cycle: Cell::new(0),
            started: Cell::new(false),
            start_time: Cell::new(None),
            first: Cell::new(None),
            baseline: Cell::new(None),
            warm: Cell::new(None),
            buffer: RefCell::new(vec![0; 32 * 1024]),
            parked_pin: RefCell::new(None),
        }
    }

    /// True the first time only (the test starts once the app list is in).
    pub fn begin(&self) -> bool {
        if self.started.replace(true) {
            return false;
        }
        let m = self.measure();
        log_info!(
            "stress{}: {} open/close cycles; at start: {m}",
            if self.gpu {
                " (GPU reset every cycle)"
            } else {
                ""
            },
            self.total
        );
        self.first.set(Some(m));
        self.start_time.set(Some(Instant::now()));
        true
    }

    /// Starts the next cycle and returns its number and plan.
    pub fn next(&self) -> (u32, Plan) {
        let n = self.cycle.get() + 1;
        self.cycle.set(n);
        (n, plan(n, self.gpu))
    }

    /// The cycle is over (menu hidden). Returns true when the test is done.
    pub fn after_cycle(&self) -> bool {
        let n = self.cycle.get();
        // Every 10 cycles during the first 100 (to see warm-up), then every 100.
        let every = if n <= 100 { 10 } else { REPORT_EVERY };
        if n == BASELINE_CYCLE || (n.is_multiple_of(every) && n != self.total) {
            let m = self.measure();
            log_info!("stress: after cycle {n}: {m}");
            if n == BASELINE_CYCLE {
                self.baseline.set(Some(m));
            }
            if n == WARM_CYCLE {
                self.warm.set(Some(m));
            }
        }
        if n < self.total {
            return false;
        }
        self.report();
        true
    }

    fn report(&self) {
        let end = self.measure();
        let secs = self
            .start_time
            .get()
            .map_or(0.0, |t| t.elapsed().as_secs_f64());
        log_info!("stress: after cycle {} ({secs:.0} s): {end}", self.total);
        let Some(base) = self.baseline.get() else {
            log_warn!("stress: fewer than {BASELINE_CYCLE} cycles, so no baseline to compare");
            return;
        };
        self.verdict("spec rule", BASELINE_CYCLE, base, end);
        if let Some(warm) = self.warm.get() {
            self.verdict("warm", WARM_CYCLE, warm, end);
        }
        if let Some(first) = self.first.get() {
            log_info!("stress: (for reference, at start: {first})");
        }
    }

    /// One RESULT line: growth from cycle `from` (reading `base`) to the end.
    fn verdict(&self, name: &str, from: u32, base: Mem, end: Mem) {
        let growth = end.private_ws as i64 - base.private_ws as i64;
        let commit = end.private_bytes as i64 - base.private_bytes as i64;
        let judged = if self.gpu { commit } else { growth };
        let pass = judged < MAX_GROWTH as i64;
        log_info!(
            "stress: RESULT ({name}, cycle {from} to {}) {}, judged by {} (limit +1 MB): \
             private working set {:+.2} MB; private bytes {:+.2} MB; handles {:+}, GDI {:+}, \
             USER {:+}",
            self.total,
            if pass { "PASS" } else { "FAIL" },
            if self.gpu {
                "private bytes"
            } else {
                "private working set"
            },
            growth as f64 / (1024.0 * 1024.0),
            commit as f64 / (1024.0 * 1024.0),
            i64::from(end.handles) - i64::from(base.handles),
            i64::from(end.gdi) - i64::from(base.gdi),
            i64::from(end.user) - i64::from(base.user),
        );
    }

    pub fn measure(&self) -> Mem {
        // SAFETY: the pseudo-handle of our own process needs no closing.
        let process = unsafe { GetCurrentProcess() };
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let mut handles = 0u32;
        // SAFETY: valid, correctly sized out-parameters for our own process.
        let (gdi, user) = unsafe {
            let _ = GetProcessMemoryInfo(
                process,
                std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            );
            let _ = GetProcessHandleCount(process, &mut handles);
            (
                GetGuiResources(process, GR_GDIOBJECTS),
                GetGuiResources(process, GR_USEROBJECTS),
            )
        };
        Mem {
            private_ws: self.private_working_set(),
            private_bytes: counters.PrivateUsage,
            handles,
            gdi,
            user,
        }
    }

    /// Counts our working-set pages that no other process shares.
    fn private_working_set(&self) -> usize {
        let mut buf = self.buffer.borrow_mut();
        for _ in 0..4 {
            let bytes = (buf.len() * std::mem::size_of::<usize>()) as u32;
            // SAFETY: `buf` is a live, writable buffer of `bytes` bytes. The
            // first entry receives the page count, the rest one entry each.
            let ok =
                unsafe { QueryWorkingSet(GetCurrentProcess(), buf.as_mut_ptr().cast(), bytes) };
            let count = buf[0];
            if ok.is_ok() && count < buf.len() {
                return buf[1..=count]
                    .iter()
                    .filter(|&&entry| entry & SHARED_BIT == 0)
                    .count()
                    * PAGE;
            }
            // Too small: `count` says how many pages there are now.
            let want = count + count / 4 + 1024;
            buf.resize(want, 0);
        }
        0
    }
}

/// A made-up song for the music card, different each cycle (so the cover
/// is decoded and painted again every time, like a real song change).
pub fn fake_song(n: u32, two_sources: bool) -> MusicView {
    let px = 44u32;
    let shade = (n * 37 % 200) as u8 + 30;
    let pixels = [shade, 64, 255 - shade, 255].repeat((px * px) as usize);
    MusicView {
        info: Rc::new(Info {
            title: format!("Stress test song {n}"),
            artist: "Glint".to_string(),
            cover: Some(Cover {
                px,
                pixels,
                color: Some(Rgb::new(255 - shade, 64, shade)),
            }),
        }),
        playing: true,
        can_previous: true,
        can_play_pause: true,
        can_next: !n.is_multiple_of(3),
        can_seek: true,
        position: i64::from(n) * 10_000_000,
        length: 180 * 10_000_000,
        rate: 1.0,
        at: Instant::now(),
        sources: if two_sources { 2 } else { 1 },
        source_id: "Microsoft.Windows.Explorer".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_feature_is_exercised_in_1000_cycles() {
        let plans: Vec<Plan> = (1..=1000).map(|n| plan(n, false)).collect();
        for mode in BackgroundMode::ALL {
            assert!(plans.iter().any(|p| p.background == mode), "{mode:?}");
        }
        assert!(plans.iter().any(|p| p.music) && plans.iter().any(|p| !p.music));
        assert!(plans.iter().any(|p| p.two_sources));
        let taken = plans.iter().filter(|p| p.pin == Some(false)).count();
        let back = plans.iter().filter(|p| p.pin == Some(true)).count();
        assert_eq!((taken, back), (20, 20));
        assert!(plans.iter().all(|p| !p.device_loss));
        assert!((1..=1000).all(|n| plan(n, true).device_loss));
        assert_eq!(plans.iter().filter(|p| p.reload_config).count(), 20);
        // Every pin taken away is put back before the end.
        assert_eq!(plan(975, false).pin, Some(true));
    }

    #[test]
    fn the_working_set_can_be_measured() {
        let s = Stress::new(1, false);
        let m = s.measure();
        assert!(
            m.private_ws > 0 && m.private_bytes > 0 && m.handles > 0,
            "{m}"
        );
    }
}
